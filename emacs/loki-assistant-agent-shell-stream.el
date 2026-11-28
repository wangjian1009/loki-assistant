;;; loki-assistant-agent-shell-stream.el --- Agent Shell 会话同步 -*- lexical-binding: t; -*-

;;; Code:

(require 'cl-lib)
(require 'json)
(require 'map)
(require 'subr-x)
(require 'url-parse)
(require 'url-util)

(declare-function agent-shell-buffers "agent-shell")
(declare-function agent-shell-busy-submit-steer "agent-shell-prompt-queue")
(declare-function agent-shell-clear-buffer "agent-shell")
(declare-function agent-shell-initialized-p "agent-shell")
(declare-function agent-shell-insert "agent-shell")
(declare-function agent-shell-restart "agent-shell")
(declare-function agent-shell-session-id "agent-shell")
(declare-function agent-shell-status "agent-shell")
(declare-function agent-shell-subscribe-to "agent-shell")
(declare-function agent-shell-unsubscribe "agent-shell")
(declare-function agent-shell--live-prompt-start "agent-shell-prompt")
(declare-function agent-shell--active-requests-p "agent-shell")
(declare-function agent-shell--current-shell "agent-shell")
(declare-function agent-shell--display-buffer "agent-shell")
(declare-function agent-shell--prompt-input-start "agent-shell-prompt")
(declare-function agent-shell--prompt-queue-enqueue "agent-shell-prompt-queue")
(declare-function agent-shell--prompt-queue-process-next "agent-shell-prompt-queue")
(declare-function agent-shell-markdown-reconstruct "agent-shell-markdown")
(declare-function agent-shell-viewport--buffer "agent-shell-viewport")
(declare-function agent-shell-viewport--prompt "agent-shell-viewport")

(defvar agent-shell--state)
(defvar agent-shell-prefer-viewport-interaction)

;; 块和批次大小按字符数计算，留出 UTF-8 与 JSON 转义空间。
(defconst loki-assistant-agent-shell-stream--block-size 2048)
(defconst loki-assistant-agent-shell-stream--batch-size 20)
(defconst loki-assistant-agent-shell-stream--journal-limit 128)
(defconst loki-assistant-agent-shell-stream--revision-bits 17)
(defconst loki-assistant-agent-shell-stream--image-limit (* 1024 1024))

(cl-defstruct loki-assistant-agent-shell-stream--state
  tick blocks (revision 0) snapshot-index (batches (make-hash-table :test #'eql)) epoch-end)

(defvar-local loki-assistant-agent-shell-stream--state nil)
(defvar-local loki-assistant-agent-shell-stream--recent-state nil)
(defvar-local loki-assistant-agent-shell-stream--init-subscription nil)
(defvar-local loki-assistant-agent-shell-stream--clients nil)
(defvar-local loki-assistant-agent-shell-stream--flush-timer nil)
(defvar loki-assistant-agent-shell-stream--epoch-sequence nil)

(defun loki-assistant-agent-shell-stream--reset-revision (state)
  "为 STATE 启用新版本范围，避免重启后误用旧游标。"
  (unless loki-assistant-agent-shell-stream--epoch-sequence
    (let* ((seed (format "%s:%s:%s" before-init-time (emacs-pid) (random)))
           (digest (secure-hash 'sha256 seed)))
      ;; 版本只占 53 位，在 JavaScript 中也可精确表示。
      (setq loki-assistant-agent-shell-stream--epoch-sequence
            (string-to-number (substring digest 0 9) 16))))
  (let* ((bits loki-assistant-agent-shell-stream--revision-bits)
         (epoch-limit (ash 1 (- 53 bits)))
         (epoch (setq loki-assistant-agent-shell-stream--epoch-sequence
                      (mod (1+ loki-assistant-agent-shell-stream--epoch-sequence) epoch-limit)))
         (start (ash epoch bits)))
    (setf (loki-assistant-agent-shell-stream--state-revision state) start
          (loki-assistant-agent-shell-stream--state-epoch-end state) (+ start (1- (ash 1 bits))))))

(defun loki-assistant-agent-shell-stream--plain (text)
  "从 TEXT 去掉 UI 属性和非打印字符，保留换行。"
  (replace-regexp-in-string "[\0-\10\13\14\16-\37\177]" "" (substring-no-properties text)))

(defun loki-assistant-agent-shell-stream--kind (id)
  "从 Agent Shell 的片段 ID 推导展示类型。"
  (cond ((string-match-p "user_message_chunk" id) "user")
        ((string-match-p "agent_message_chunk" id) "assistant")
        ((string-match-p "agent_thought_chunk" id) "thought")
        ((string-match-p "permission\\|elicitation" id) "permission")
        ((string-match-p "activity\\|tool\\|exec" id) "tool")
        (t "system")))

(defun loki-assistant-agent-shell-stream--user-text (start end &optional reconstruct)
  "导出 START 到 END 的用户文本及附件。RECONSTRUCT 恢复正文的 Markdown。"
  (let (parts)
    (while (< start end)
      (let* ((next (next-single-property-change start 'agent-shell-context-image nil end))
             (image (get-text-property start 'agent-shell-context-image))
             (display (and image (get-text-property start 'display)))
             (file (when (eq (car-safe display) 'image)
                     (plist-get (cdr display) :file))))
        (when image
          (setq next (next-single-property-change start 'display nil next)))
        (push (cond
               ((and (stringp file) (not (file-remote-p file)))
                ;; 将路径作为一个 URL 编码，保留空格、非 ASCII 字符和括号。
                (format "\n\n![图片](%s)\n\n" (url-hexify-string (expand-file-name file))))
               ((and reconstruct (fboundp 'agent-shell-markdown-reconstruct))
                (agent-shell-markdown-reconstruct start next))
               (t (buffer-substring-no-properties start next)))
              parts)
        (setq start next)))
    (apply #'concat (nreverse parts))))

(defun loki-assistant-agent-shell-stream--submitted-user-text (start end)
  "提取 START 到 END 中已冻结的提示正文，跳过提示符和 steer 标签。"
  (let (parts)
    (while (< start end)
      (let ((next (min (next-single-property-change start 'field nil end)
                       (next-single-property-change start 'read-only nil end)
                       (next-single-property-change start 'agent-shell-steer-prefix nil end))))
        (when (and (null (get-text-property start 'field))
                   (get-text-property start 'read-only)
                   (not (get-text-property start 'agent-shell-steer-prefix)))
          (push (loki-assistant-agent-shell-stream--user-text start next) parts))
        (setq start next)))
    (apply #'concat (nreverse parts))))

(defun loki-assistant-agent-shell-stream--fragment (from to state)
  "提取 FROM 到 TO 的片段 STATE 及其折叠信息。"
  (let* ((id (alist-get :qualified-id state))
         (group (eq (alist-get :kind state) 'group))
         (kind (if group "group" (loki-assistant-agent-shell-stream--kind id)))
         labels body indicator)
    (unless (string-prefix-p "bootstrapping-" id)
      (while (< from to)
        (let* ((section (get-text-property from 'agent-shell-ui-section))
               (next (next-single-property-change from 'agent-shell-ui-section nil to)))
          (pcase section
            ((or 'label-left 'label-right)
             (push (buffer-substring-no-properties from next) labels))
            ('body
             (push (cond ((equal kind "user") (loki-assistant-agent-shell-stream--user-text from next t))
                         ((fboundp 'agent-shell-markdown-reconstruct)
                          (agent-shell-markdown-reconstruct from next))
                         (t (buffer-substring-no-properties from next)))
                   body))
            ('indicator (setq indicator t)))
          (setq from next)))
      (let ((title (string-trim (loki-assistant-agent-shell-stream--plain
                                 (string-join (nreverse labels) " "))))
            (content (string-trim (loki-assistant-agent-shell-stream--plain
                                   (apply #'concat (nreverse body))))))
        (unless (and (string-empty-p title) (string-empty-p content))
          ;; 普通正文虽然也有 :collapsed，但没有实际折叠控件。
          (list id kind title content (alist-get :group-id state)
                (when (or group (and indicator (not (string-empty-p title)) body))
                  (if (alist-get :collapsed state) t :json-false))))))))

(defun loki-assistant-agent-shell-stream--blocks ()
  "读取当前 Shell 已提交区域，保留会话的可读片段。"
  (save-restriction
    (widen)
    (let ((position (point-min))
          ;; 持久提示符与进程标记之后属于输入区。
          (end (or (ignore-errors (agent-shell--live-prompt-start))
                   (when-let* ((process (get-buffer-process (current-buffer))))
                     (marker-position (process-mark process)))
                   (point-min)))
          (counts (make-hash-table :test #'equal))
          (groups (make-hash-table :test #'equal))
          rows)
      (while (< position end)
        (let* ((state (get-text-property position 'agent-shell-ui-state))
               (next (next-single-property-change position 'agent-shell-ui-state nil end))
               (id (alist-get :qualified-id state))
               (fragment
                (if (and (stringp id)
                         (not (and (string-suffix-p "-steered-user_message_chunk" id)
                                   (null (get-text-property position 'agent-shell-ui-section)))))
                    (loki-assistant-agent-shell-stream--fragment position next state)
                  (let ((content (string-trim
                                  (loki-assistant-agent-shell-stream--plain
                                   (loki-assistant-agent-shell-stream--submitted-user-text position next)))))
                    (unless (string-empty-p content)
                      (list (or id "submitted-user") "user" "" content nil nil))))))
          (when fragment
            (pcase-let ((`(,id ,kind ,title ,body ,group-id ,collapsed) fragment))
              ;; 位置会随前文更新而移动。使用片段标识与同名片段出现次序。
              (let* ((count (1+ (gethash id counts 0)))
                     (key (format "%s:%d" id count))
                     (parent (and group-id (gethash group-id groups)))
                     (offset 0)
                     (length (length body)))
                (puthash id count counts)
                (when (equal kind "group")
                  (puthash id key groups))
                (while (or (zerop offset) (< offset length))
                  (let ((first (zerop offset)))
                    (push `((id . ,(if first key (format "%s:%d" key offset)))
                            (kind . ,(if first kind "continuation"))
                            ,@(when (if first parent key)
                                `((parent_id . ,(if first parent key))))
                            ,@(when (and first collapsed) `((collapsed . ,collapsed)))
                            ,@(when (and first (not (string-empty-p title)))
                                `((title . ,(substring title 0 (min 256 (length title))))))
                            (text . ,(substring body offset (min length (+ offset loki-assistant-agent-shell-stream--block-size)))))
                          rows))
                  (cl-incf offset loki-assistant-agent-shell-stream--block-size)))))
          (setq position next)))
      (nreverse rows))))

(defun loki-assistant-agent-shell-stream--chunks (blocks)
  "将 BLOCKS 分成可发送的批次，空会话也生成一个批次。"
  (or (seq-partition blocks loki-assistant-agent-shell-stream--batch-size) (list nil)))

(defun loki-assistant-agent-shell-stream--event (kind revision chunk &optional snapshot-end)
  "构造 KIND、REVISION 和 CHUNK 的事件，附上快照结束版本 SNAPSHOT-END。"
  `((kind . ,kind) (revision . ,revision) (blocks . ,(vconcat chunk))
    ,@(when snapshot-end `((snapshot_end_revision . ,snapshot-end)))))

(defun loki-assistant-agent-shell-stream--enqueue (state kind blocks)
  "在 STATE 中按批次追加 KIND 和 BLOCKS。"
  (when (equal kind "snapshot")
    (clrhash (loki-assistant-agent-shell-stream--state-batches state))
    (setf (loki-assistant-agent-shell-stream--state-snapshot-index state)
          (1+ (loki-assistant-agent-shell-stream--state-revision state))))
  (let* ((chunks (loki-assistant-agent-shell-stream--chunks blocks))
         (snapshot-end (when (equal kind "snapshot")
                         (+ (loki-assistant-agent-shell-stream--state-revision state) (length chunks)))))
    (dolist (chunk chunks)
      (let ((revision (cl-incf (loki-assistant-agent-shell-stream--state-revision state))))
        (puthash revision (loki-assistant-agent-shell-stream--event kind revision chunk snapshot-end)
                 (loki-assistant-agent-shell-stream--state-batches state)))
      (setq kind "update" snapshot-end nil))))

(defun loki-assistant-agent-shell-stream--recent (blocks count)
  "返回 BLOCKS 最后 COUNT 条逻辑片段，保留祖先和完整正文。"
  (let ((remaining count) (included (make-hash-table :test #'equal)))
    (dolist (row (reverse blocks))
      (let ((id (alist-get 'id row)) (kind (alist-get 'kind row)))
        (when (or (gethash id included)
                  (and (> remaining 0) (not (member kind '("group" "continuation")))))
          (unless (equal kind "group") (cl-decf remaining))
          (puthash id t included)
          (when-let* ((parent (alist-get 'parent_id row))) (puthash parent t included)))))
    (seq-filter (lambda (row)
                  (or (gethash (alist-get 'id row) included)
                      (and (equal (alist-get 'kind row) "continuation")
                           (gethash (alist-get 'parent_id row) included))))
                blocks)))

(defun loki-assistant-agent-shell-stream--refresh (state &optional recent)
  "更新 STATE 的片段和变更记录。"
  (let ((tick (buffer-modified-tick))
        (json-false :json-false))
    (unless (eql tick (loki-assistant-agent-shell-stream--state-tick state))
      (let* ((old (loki-assistant-agent-shell-stream--state-blocks state))
             (all-blocks (loki-assistant-agent-shell-stream--blocks))
             (blocks (if recent (loki-assistant-agent-shell-stream--recent all-blocks recent) all-blocks))
             (rest blocks) (reset nil) changes)
        (dolist (row old)
          (if (equal (alist-get 'id row) (alist-get 'id (car rest)))
              (unless (equal row (car rest)) (push (car rest) changes))
            (setq reset t))
          (setq rest (cdr rest)))
        (let ((max-batches (max 1 (ceiling (length blocks) loki-assistant-agent-shell-stream--batch-size))))
          (when (> (+ (loki-assistant-agent-shell-stream--state-revision state) max-batches)
                   (loki-assistant-agent-shell-stream--state-epoch-end state))
            (loki-assistant-agent-shell-stream--reset-revision state)
            (setq reset t)))
        (cond
         ((or reset (not (loki-assistant-agent-shell-stream--state-snapshot-index state))
              (>= (hash-table-count (loki-assistant-agent-shell-stream--state-batches state))
                  (+ loki-assistant-agent-shell-stream--journal-limit
                     (/ (length blocks) loki-assistant-agent-shell-stream--batch-size))))
          (loki-assistant-agent-shell-stream--enqueue state "snapshot" blocks))
         ((or changes rest)
          (loki-assistant-agent-shell-stream--enqueue state "update" (nconc (nreverse changes) rest))))
        (setf (loki-assistant-agent-shell-stream--state-tick state) tick
              (loki-assistant-agent-shell-stream--state-blocks state) blocks)))))

(defun loki-assistant-agent-shell-stream--current-state (recent)
  "返回当前 buffer 的会话状态，RECENT 可限制记录条数。"
  (let* ((slot (if recent 'loki-assistant-agent-shell-stream--recent-state
                 'loki-assistant-agent-shell-stream--state))
         (state (symbol-value slot)))
    (unless (and state (ignore-errors (loki-assistant-agent-shell-stream--state-epoch-end state)))
      ;; 已运行 Emacs 可能保留更新前格式的会话状态。
      (setq state (set slot (make-loki-assistant-agent-shell-stream--state)))
      (loki-assistant-agent-shell-stream--reset-revision state))
    (loki-assistant-agent-shell-stream--refresh state recent)
    state))

(defun loki-assistant-agent-shell-stream--next-event (state revision)
  "返回 STATE 自 REVISION 之后的下一批事件。"
  (let* ((first (loki-assistant-agent-shell-stream--state-snapshot-index state))
         (last (loki-assistant-agent-shell-stream--state-revision state))
         (next (if (and revision (<= first revision last)) (1+ revision) first)))
    (gethash next (loki-assistant-agent-shell-stream--state-batches state))))

(defun loki-assistant-agent-shell-stream--session-buffer (session-id)
  "按 SESSION-ID 寻找打开的 Agent Shell buffer。"
  (when-let* ((buffer (get-buffer session-id))
              ((memq buffer (agent-shell-buffers))))
    buffer))

(defun loki-assistant-agent-shell-stream-read (session-id revision &optional recent)
  "返回 SESSION-ID 自 REVISION 之后的一批可读会话事件 JSON。"
  (if-let* ((buffer (loki-assistant-agent-shell-stream--session-buffer session-id)))
      (with-current-buffer buffer
        (let ((event (loki-assistant-agent-shell-stream--next-event
                      (loki-assistant-agent-shell-stream--current-state recent) revision)))
          (if event (json-encode event) "null")))
    (json-encode `((kind . ,(if revision "closed" "missing")) (revision . ,(or revision 0))))))

(defun loki-assistant-agent-shell-stream--begin-replay (client state)
  "让 CLIENT 从 STATE 捕获一份完整的当前快照。"
  (let* ((chunks (loki-assistant-agent-shell-stream--chunks
                  (loki-assistant-agent-shell-stream--state-blocks state)))
         (last (loki-assistant-agent-shell-stream--state-revision state))
         (first (- (1+ last) (length chunks))))
    (process-put client :replay chunks)
    (process-put client :next first)
    (process-put client :snapshot-end last)
    (process-put client :first first)))

(defun loki-assistant-agent-shell-stream--flush-client (client)
  "发送 CLIENT 等待中的一批内容。"
  (when (and (eq (process-status client) 'open) (process-get client :ready))
    (let* ((revision (process-get client :revision))
           (state (loki-assistant-agent-shell-stream--current-state (process-get client :recent)))
           (first (loki-assistant-agent-shell-stream--state-snapshot-index state))
           (last (loki-assistant-agent-shell-stream--state-revision state)))
      (when (and (not (process-get client :replay))
                 (not (and revision (<= first revision last))))
        (loki-assistant-agent-shell-stream--begin-replay client state))
      (when-let* ((event
                   (if-let* ((chunks (process-get client :replay)))
                       (let* ((next (process-get client :next))
                              (snapshot (equal next (process-get client :first))))
                         (process-put client :replay (cdr chunks))
                         (process-put client :next (1+ next))
                         (loki-assistant-agent-shell-stream--event
                          (if snapshot "snapshot" "update") next (car chunks)
                          (when snapshot (process-get client :snapshot-end))))
                     (loki-assistant-agent-shell-stream--next-event state revision))))
        (process-put client :ready nil)
        (process-put client :revision (alist-get 'revision event))
        (process-send-string client (concat (json-encode event) "\n"))))))

(defun loki-assistant-agent-shell-stream--flush (buffer)
  "提交 BUFFER 的变更并推送给等待下一批的订阅者。"
  (when (buffer-live-p buffer)
    (with-current-buffer buffer
      (setq loki-assistant-agent-shell-stream--flush-timer nil)
      (dolist (client loki-assistant-agent-shell-stream--clients)
        (if (equal (buffer-name) (process-get client :session))
            (condition-case nil
                (loki-assistant-agent-shell-stream--flush-client client)
              (error (delete-process client)))
          (loki-assistant-agent-shell-stream--close-client client))))))

(defun loki-assistant-agent-shell-stream--schedule-flush (&rest _)
  "合并同一小段时间内的 buffer 变更。"
  (unless loki-assistant-agent-shell-stream--flush-timer
    (setq loki-assistant-agent-shell-stream--flush-timer
          (run-at-time 0.03 nil #'loki-assistant-agent-shell-stream--flush (current-buffer)))))

(defun loki-assistant-agent-shell-stream--close-client (client)
  "向 CLIENT 发出关闭事件并断开连接。"
  (ignore-errors
    (process-send-string
     client
     (concat (json-encode `((kind . "closed") (revision . ,(or (process-get client :revision) 0)))) "\n")))
  (delete-process client))

(defun loki-assistant-agent-shell-stream--close-buffer ()
  "关闭当前 buffer 的全部推送连接。"
  (mapc #'loki-assistant-agent-shell-stream--close-client loki-assistant-agent-shell-stream--clients))

(defun loki-assistant-agent-shell-stream--buffer-list-change ()
  "通知订阅者当前 buffer 已改名。"
  (dolist (client loki-assistant-agent-shell-stream--clients)
    (unless (equal (buffer-name) (process-get client :session))
      (loki-assistant-agent-shell-stream--close-client client))))

(defun loki-assistant-agent-shell-stream--client-status (client _event)
  "在 CLIENT 连接或离开时更新订阅。"
  (when-let* ((buffer (process-get client :shell-buffer))
              ((buffer-live-p buffer)))
    (with-current-buffer buffer
      (pcase (process-status client)
        ('open
         (when-let* ((token (process-get client :token)))
           (process-put client :token nil)
           (condition-case nil
               (progn
                 (process-send-string client (concat token "\n"))
                 (process-put client :ready t)
                 (loki-assistant-agent-shell-stream--schedule-flush))
             (error (delete-process client)))))
        ((or 'closed 'failed 'exit 'signal)
         (setq loki-assistant-agent-shell-stream--clients
               (delq client loki-assistant-agent-shell-stream--clients))
         (unless loki-assistant-agent-shell-stream--clients
           (remove-hook 'after-change-functions #'loki-assistant-agent-shell-stream--schedule-flush t)
           (remove-hook 'kill-buffer-hook #'loki-assistant-agent-shell-stream--close-buffer t)
           (remove-hook 'change-major-mode-hook #'loki-assistant-agent-shell-stream--close-buffer t)
           (remove-hook 'buffer-list-update-hook #'loki-assistant-agent-shell-stream--buffer-list-change t)
           (when (timerp loki-assistant-agent-shell-stream--flush-timer)
             (cancel-timer loki-assistant-agent-shell-stream--flush-timer)
             (setq loki-assistant-agent-shell-stream--flush-timer nil))))))))

(defun loki-assistant-agent-shell-stream--client-input (client data)
  "收到 CLIENT 的 DATA 确认后再推送下一批。"
  (when-let* ((buffer (process-get client :shell-buffer))
              ((buffer-live-p buffer)))
    (with-current-buffer buffer
      (if (equal data "\n")
          (progn
            (process-put client :ready t)
            (loki-assistant-agent-shell-stream--schedule-flush))
        (delete-process client)))))

(defun loki-assistant-agent-shell-stream--attach-client (client)
  "将 CLIENT 转接到当前 Agent Shell buffer。"
  (process-put client :shell-buffer (current-buffer))
  (push client loki-assistant-agent-shell-stream--clients)
  (add-hook 'after-change-functions #'loki-assistant-agent-shell-stream--schedule-flush nil t)
  (add-hook 'kill-buffer-hook #'loki-assistant-agent-shell-stream--close-buffer nil t)
  (add-hook 'change-major-mode-hook #'loki-assistant-agent-shell-stream--close-buffer nil t)
  (add-hook 'buffer-list-update-hook #'loki-assistant-agent-shell-stream--buffer-list-change nil t))

(defun loki-assistant-agent-shell-stream--follow-restart (restart &rest arguments)
  "在 RESTART 使用相同名称替换 buffer 后继续原会话推送。"
  (let* ((buffer (agent-shell--current-shell))
         (name (buffer-name buffer))
         (clients (and buffer (buffer-local-value 'loki-assistant-agent-shell-stream--clients buffer))))
    (when clients
      (with-current-buffer buffer
        (setq loki-assistant-agent-shell-stream--clients nil)))
    (unwind-protect
        (apply restart arguments)
      (when clients
        (let ((target (loki-assistant-agent-shell-stream--session-buffer name)))
          (dolist (client clients)
            (when (process-live-p client)
              (if target
                  (with-current-buffer target
                    (unless (eq target buffer)
                      (process-put client :revision nil)
                      (process-put client :replay nil))
                    (loki-assistant-agent-shell-stream--attach-client client)
                    (loki-assistant-agent-shell-stream--schedule-flush))
                (loki-assistant-agent-shell-stream--close-client client)))))))))

(defun loki-assistant-agent-shell-stream-subscribe (session-id port token &optional recent)
  "向回环 PORT 的一次性 TOKEN 连接推送 SESSION-ID 的内容，可用 RECENT 限制初始范围。"
  (loki-assistant-agent-shell-stream-resume session-id port token recent nil))

(defun loki-assistant-agent-shell-stream-resume (session-id port token recent after)
  "向回环 PORT 的一次性 TOKEN 连接推送 SESSION-ID 自 AFTER 之后的内容。
RECENT 可限制会话范围。AFTER 为空或已失效时推送完整快照。"
  (when-let* ((buffer (loki-assistant-agent-shell-stream--session-buffer session-id)))
    (with-current-buffer buffer
      (let ((client (make-network-process :name "loki-agent-shell-stream"
                                          :buffer nil :host "127.0.0.1" :service port
                                          :family 'ipv4 :nowait t :noquery t :coding 'utf-8-unix
                                          :filter #'loki-assistant-agent-shell-stream--client-input
                                          :sentinel #'loki-assistant-agent-shell-stream--client-status)))
        (process-put client :session session-id)
        (process-put client :token token)
        (process-put client :recent recent)
        (process-put client :revision after)
        (loki-assistant-agent-shell-stream--attach-client client)
        (loki-assistant-agent-shell-stream--client-status client nil)
        t))))

(with-eval-after-load 'agent-shell
  (advice-add 'agent-shell-restart :around #'loki-assistant-agent-shell-stream--follow-restart))

(defun loki-assistant-agent-shell-stream--submit-with-draft (submit)
  "执行 SUBMIT 并将桌面草稿恢复到提交后的输入区。"
  (save-restriction
    (widen)
    (let* ((start (agent-shell--prompt-input-start))
           (draft (when (and start (< start (point-max)))
                    (buffer-substring start (point-max))))
           (offset (when (and draft (>= (point) start))
                     (- (point) start))))
      (save-excursion
        (when draft (delete-region start (point-max)))
        (unwind-protect
            (funcall submit)
          (when draft
            (goto-char (point-max))
            (insert draft))))
      (when offset
        (goto-char (+ (- (point-max) (length draft)) offset))))))

(defun loki-assistant-agent-shell-stream--finish-init (buffer)
  "在 BUFFER 初始化完成后启动等待中的提交。"
  (when (buffer-live-p buffer)
    (with-current-buffer buffer
      (when (and (eq (agent-shell-status) 'ready)
                 (process-live-p (map-nested-elt agent-shell--state '(:client :process))))
        (loki-assistant-agent-shell-stream--submit-with-draft
         #'agent-shell--prompt-queue-process-next)))))

(defun loki-assistant-agent-shell-stream--queue-during-init (text)
  "将 TEXT 加入当前会话的队列，并在初始化完成后尝试提交。"
  (unless loki-assistant-agent-shell-stream--init-subscription
    (let ((buffer (current-buffer)))
      (setq loki-assistant-agent-shell-stream--init-subscription
            (agent-shell-subscribe-to
             :shell-buffer buffer :event 'init-finished
             :on-event
             (lambda (_event)
               (agent-shell-unsubscribe
                :subscription loki-assistant-agent-shell-stream--init-subscription)
               (setq loki-assistant-agent-shell-stream--init-subscription nil)
               ;; 让初始化处理器先发出已经附带的本机请求。
               (run-at-time 0 nil #'loki-assistant-agent-shell-stream--finish-init buffer))))))
  (agent-shell--prompt-queue-enqueue :prompt text))

(defun loki-assistant-agent-shell-send-message (session-id text)
  "向 SESSION-ID 提交 TEXT，执行中的会话使用 steer。"
  (let* ((buffer (get-buffer session-id))
         (kind
          (cond
           ((not (and buffer (memq buffer (agent-shell-buffers)))) "missing")
           (t
            (with-current-buffer buffer
              (cond
               ((not (process-live-p (map-nested-elt agent-shell--state '(:client :process))))
                "unavailable")
               ((not (if (fboundp 'agent-shell-initialized-p)
                         (agent-shell-initialized-p)
                       (agent-shell-session-id)))
                (loki-assistant-agent-shell-stream--queue-during-init text)
                "accepted")
               (t
                (if (eq (agent-shell-status) 'ready)
                    (loki-assistant-agent-shell-stream--submit-with-draft
                     (lambda () (agent-shell-insert :text text :submit t :no-focus t
                                                    :shell-buffer buffer)))
                  (agent-shell-busy-submit-steer text))
                "accepted")))))))
    (json-encode (list (cons 'kind kind)))))

(defun loki-assistant-agent-shell-restart-session (session-id)
  "在 Emacs 中重启 SESSION-ID 对应的 Agent Shell，沿用原有会话。"
  (let* ((buffer (loki-assistant-agent-shell-stream--session-buffer session-id))
         (kind
          (if (not buffer)
              "missing"
            (with-current-buffer buffer
              (let ((provider-id (agent-shell-session-id)))
                (cond
                 ((or (not (eq (agent-shell-status) 'ready))
                      (agent-shell--active-requests-p agent-shell--state)
                      (map-elt agent-shell--state :pending-prompts)
                      (when-let* ((start (agent-shell--prompt-input-start))) (< start (point-max)))
                      (when-let* ((viewport (agent-shell-viewport--buffer :shell-buffer buffer :existing-only t)))
                        (with-current-buffer viewport
                          (and (derived-mode-p 'agent-shell-viewport-edit-mode)
                               (not (string-empty-p (or (agent-shell-viewport--prompt) "")))))))
                  "busy")
                 ((not (and provider-id
                            (or (map-elt agent-shell--state :supports-session-load)
                                (map-elt agent-shell--state :supports-session-resume))))
                  "not_restartable")
                 (t
                  (condition-case nil
                      (save-selected-window
                        (let ((agent-shell-prefer-viewport-interaction nil))
                          (cl-letf (((symbol-function 'y-or-n-p) (lambda (&rest _) nil))
                                    ((symbol-function 'yes-or-no-p) (lambda (&rest _) nil))
                                    ((symbol-function 'agent-shell--display-buffer) #'identity))
                            (agent-shell-restart :session-id provider-id)))
                        "accepted")
                    (user-error "not_restartable")
                    (error "unavailable")))))))))
    (json-encode (list (cons 'kind kind)))))

(defun loki-assistant-agent-shell-clear-buffer (session-id)
  "清空 SESSION-ID 对应的 Agent Shell 历史显示，保留输入区。"
  (let* ((buffer (loki-assistant-agent-shell-stream--session-buffer session-id))
         (kind
          (if (not buffer)
              "missing"
            (with-current-buffer buffer
              (save-restriction
                (widen)
                (if-let* ((process (get-buffer-process buffer))
                          ((process-live-p process))
                          (prompt (ignore-errors (agent-shell--live-prompt-start))))
                    (let* ((process-position (process-mark process))
                           (original-position (copy-marker process-position)))
                      (unwind-protect
                          (condition-case nil
                              (progn
                                ;; 原生清屏按进程标记所在行截断。将边界移到提示符首行。
                                (set-marker process-position prompt)
                                (agent-shell-clear-buffer)
                                "accepted")
                            (error "unavailable"))
                        (set-marker process-position original-position)
                        (set-marker original-position nil)))
                  "unavailable"))))))
    (json-encode (list (cons 'kind kind)))))

(defun loki-assistant-agent-shell-stream--image-reference-p (source)
  "判断 SOURCE 是否是已提交会话中存在的链接目标。"
  (let ((pattern (concat "\\(?:](<?\\|\\[\\[\\)"
                         (regexp-opt (list source (concat "file:" source)))
                         "\\(?:[>) \t]\\|\\]\\)"))
        (tail ""))
    (cl-some (lambda (block)
               (let* ((chunk (alist-get 'text block))
                      (text (if (equal (alist-get 'kind block) "continuation") (concat tail chunk) chunk)))
                 (setq tail (substring text (max 0 (- (length text) (+ 16 (length source))))))
                 (string-match-p pattern text)))
             (loki-assistant-agent-shell-stream--blocks))))

(defun loki-assistant-agent-shell-stream--public-ipv4-p (address)
  "判断 ADDRESS 是否为公网 IPv4 地址。"
  (when (string-match "^\\([0-9]+\\)\\.\\([0-9]+\\)\\.\\([0-9]+\\)\\.[0-9]+$" address)
    (let ((a (string-to-number (match-string 1 address)))
          (b (string-to-number (match-string 2 address)))
          (c (string-to-number (match-string 3 address))))
      (and (<= 1 a 223) (not (memq a '(10 127)))
           (not (or (and (= a 100) (<= 64 b 127))
                    (and (= a 169) (= b 254))
                    (and (= a 172) (<= 16 b 31))
                    (and (= a 192) (or (= b 168) (and (= b 0) (memq c '(0 2)))))
                    (and (= a 198) (or (memq b '(18 19)) (and (= b 51) (= c 100))))
                    (and (= a 203) (= b 0) (= c 113))))))))

(defun loki-assistant-agent-shell-stream--fetch-image (source)
  "读取 SOURCE 的公网图片，固定 DNS 结果并限制大小。"
  (let* ((url (url-generic-parse-url source))
         (host (url-host url))
         (port (url-port url)))
    (when (and host (string-match-p "^[-.a-zA-Z0-9]+$" host)
               (null (url-user url)) (null (url-password url)) (memq port '(80 443)))
      (when-let* ((address (cl-find-if #'loki-assistant-agent-shell-stream--public-ipv4-p
                                      (mapcar (lambda (address) (format-network-address address t))
                                              (network-lookup-address-info host 'ipv4))))
                  (curl (executable-find "curl")))
        (with-temp-buffer
          (set-buffer-multibyte nil)
          (when (eq 0 (call-process curl nil (list (current-buffer) nil) nil
                                    "-q" "--silent" "--fail" "--ipv4" "--noproxy" "*"
                                    "--proto" "=http,https" "--globoff" "--max-time" "4"
                                    "--max-filesize" "1048576"
                                    "--resolve" (format "%s:%d:%s" host port address) "--" source))
            (buffer-string)))))))

(defun loki-assistant-agent-shell-stream--local-image-bytes (file)
  "读取本机 FILE，超限的 PNG 和 JPEG 使用符合传输上限的预览。"
  (let ((size (file-attribute-size (file-attributes file)))
        (format (pcase (downcase (or (file-name-extension file) ""))
                  ((or "jpg" "jpeg") "jpeg")
                  ("png" "png"))))
    (cond
     ((<= size loki-assistant-agent-shell-stream--image-limit)
      (with-temp-buffer
        (set-buffer-multibyte nil)
        (insert-file-contents-literally file nil 0 loki-assistant-agent-shell-stream--image-limit)
        (buffer-string)))
     ((and format (eq system-type 'darwin) (file-executable-p "/usr/bin/sips"))
      (let ((preview (make-temp-file "loki-agent-image-")))
        (unwind-protect
            (when (seq-some
                   (lambda (dimension)
                     (and (eq 0 (apply #'call-process "/usr/bin/sips" nil nil nil
                                       "-Z" dimension "-s" "format" format
                                       (append (when (equal format "jpeg") '("-s" "formatOptions" "75"))
                                               (list file "--out" preview))))
                          (<= 12 (file-attribute-size (file-attributes preview))
                              loki-assistant-agent-shell-stream--image-limit)))
                   '("1600" "1024" "640" "384"))
              (with-temp-buffer
                (set-buffer-multibyte nil)
                (insert-file-contents-literally preview nil 0 loki-assistant-agent-shell-stream--image-limit)
                (buffer-string)))
          (delete-file preview)))))))

(defun loki-assistant-agent-shell-stream--image-bytes (source)
  "读取 SOURCE 对应的图像字节。"
  (cond
   ((string-match-p "^https?://" source)
    (loki-assistant-agent-shell-stream--fetch-image source))
   ((not (file-remote-p default-directory))
    (let* ((encoded (cond
                     ((string-prefix-p "file://" source)
                      (let ((url (url-generic-parse-url source)))
                        (when (member (url-host url) '(nil "" "localhost"))
                          (url-filename url))))
                     ((string-prefix-p "file:" source) (substring source 5))
                     (t source)))
           (path (and encoded (decode-coding-string (url-unhex-string encoded) 'utf-8))))
      (when (and path (not (file-remote-p path))
                 (not (string-match-p "^[a-zA-Z][a-zA-Z0-9+.-]*:" path)))
        (let ((file (expand-file-name path)))
          (when (and (file-regular-p file) (file-readable-p file))
            (loki-assistant-agent-shell-stream--local-image-bytes file))))))))

(defun loki-assistant-agent-shell-read-image (session-id source)
  "读取 SESSION-ID 的已提交内容中 SOURCE 指向的图片。"
  (when-let* ((buffer (get-buffer session-id))
              ((memq buffer (agent-shell-buffers))))
    (with-current-buffer buffer
      (when-let* (((loki-assistant-agent-shell-stream--image-reference-p source))
                  (data (ignore-errors (loki-assistant-agent-shell-stream--image-bytes source)))
                  ((<= 12 (length data) loki-assistant-agent-shell-stream--image-limit))
                  (mime (cond ((string-prefix-p (unibyte-string 137 80 78 71 13 10 26 10) data) "image/png")
                              ((string-prefix-p (unibyte-string 255 216 255) data) "image/jpeg")
                              ((or (string-prefix-p "GIF87a" data) (string-prefix-p "GIF89a" data)) "image/gif")
                              ((and (string-prefix-p "RIFF" data)
                                    (string= (substring data 8 12) "WEBP")) "image/webp"))))
        (json-encode (list (cons 'mime mime) (cons 'data (base64-encode-string data t))))))))

(provide 'loki-assistant-agent-shell-stream)
;;; loki-assistant-agent-shell-stream.el ends here
