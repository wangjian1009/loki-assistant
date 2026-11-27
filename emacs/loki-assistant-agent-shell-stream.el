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
(declare-function agent-shell-initialized-p "agent-shell")
(declare-function agent-shell-insert "agent-shell")
(declare-function agent-shell-session-id "agent-shell")
(declare-function agent-shell-status "agent-shell")
(declare-function agent-shell-subscribe-to "agent-shell")
(declare-function agent-shell-unsubscribe "agent-shell")
(declare-function agent-shell--live-prompt-start "agent-shell-prompt")
(declare-function agent-shell--prompt-input-start "agent-shell-prompt")
(declare-function agent-shell--prompt-queue-enqueue "agent-shell-prompt-queue")
(declare-function agent-shell--prompt-queue-process-next "agent-shell-prompt-queue")
(declare-function agent-shell-markdown-reconstruct "agent-shell-markdown")

(defvar agent-shell--state)

;; 块和批次大小按字符数计算，留出 UTF-8 与 JSON 转义空间。
(defconst loki-assistant-agent-shell-stream--block-size 2048)
(defconst loki-assistant-agent-shell-stream--batch-size 20)
(defconst loki-assistant-agent-shell-stream--journal-limit 128)

(cl-defstruct loki-assistant-agent-shell-stream--state
  tick blocks (revision 0) snapshot-index (batches (make-hash-table :test #'eql)))

(defvar-local loki-assistant-agent-shell-stream--state nil)
(defvar-local loki-assistant-agent-shell-stream--recent-state nil)
(defvar-local loki-assistant-agent-shell-stream--init-subscription nil)
(defvar loki-assistant-agent-shell-stream--epoch 0)

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
             (push (if (fboundp 'agent-shell-markdown-reconstruct)
                       (agent-shell-markdown-reconstruct from next)
                     (buffer-substring-no-properties from next))
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
               (fragment
                (if (stringp (alist-get :qualified-id state))
                    (loki-assistant-agent-shell-stream--fragment position next state)
                  (let ((input position) parts)
                    (while (< input next)
                      (let ((limit (next-single-property-change input 'field nil next)))
                        (when (and (null (get-text-property input 'field))
                                   (get-text-property input 'read-only))
                          (push (buffer-substring-no-properties input limit) parts))
                        (setq input limit)))
                    (let ((content (string-trim (loki-assistant-agent-shell-stream--plain
                                                 (apply #'concat (nreverse parts))))))
                      (unless (string-empty-p content)
                        (list "submitted-user" "user" "" content nil nil)))))))
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

(defun loki-assistant-agent-shell-stream--enqueue (state kind blocks)
  "在 STATE 中按批次追加 KIND 和 BLOCKS。"
  (when (equal kind "snapshot")
    (clrhash (loki-assistant-agent-shell-stream--state-batches state))
    (setf (loki-assistant-agent-shell-stream--state-snapshot-index state)
          (1+ (loki-assistant-agent-shell-stream--state-revision state))))
  (let ((chunks (or (seq-partition blocks loki-assistant-agent-shell-stream--batch-size)
                    (list nil))))
    (dolist (chunk chunks)
      (let ((revision (cl-incf (loki-assistant-agent-shell-stream--state-revision state))))
        (puthash revision `((kind . ,kind) (revision . ,revision) (blocks . ,(vconcat chunk)))
                 (loki-assistant-agent-shell-stream--state-batches state)))
      (setq kind "update"))))

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

(defun loki-assistant-agent-shell-stream-read (session-id revision &optional recent)
  "返回 SESSION-ID 自 REVISION 之后的一批可读会话事件 JSON。"
  (let* ((candidate (get-buffer session-id))
         (buffer (and candidate (memq candidate (agent-shell-buffers)) candidate)))
    (if (not buffer)
        (json-encode `((kind . ,(if revision "closed" "missing")) (revision . ,(or revision 0))))
      (with-current-buffer buffer
        (let* ((slot (if recent 'loki-assistant-agent-shell-stream--recent-state
                       'loki-assistant-agent-shell-stream--state))
               (state (or (symbol-value slot)
                          (set slot (make-loki-assistant-agent-shell-stream--state
                                     :revision (ash (cl-incf loki-assistant-agent-shell-stream--epoch) 32))))))
          (loki-assistant-agent-shell-stream--refresh state recent)
          (let* ((first (loki-assistant-agent-shell-stream--state-snapshot-index state))
                 (last (loki-assistant-agent-shell-stream--state-revision state))
                 (next (if (and revision (<= first revision last)) (1+ revision) first))
                 (batch (gethash next (loki-assistant-agent-shell-stream--state-batches state))))
            (if batch (json-encode batch) "null")))))))

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
            (with-temp-buffer
              (set-buffer-multibyte nil)
              (insert-file-contents-literally file nil 0 1048577)
              (buffer-string)))))))))

(defun loki-assistant-agent-shell-read-image (session-id source)
  "读取 SESSION-ID 的已提交内容中 SOURCE 指向的图片。"
  (when-let* ((buffer (get-buffer session-id))
              ((memq buffer (agent-shell-buffers))))
    (with-current-buffer buffer
      (when-let* (((loki-assistant-agent-shell-stream--image-reference-p source))
                  (data (ignore-errors (loki-assistant-agent-shell-stream--image-bytes source)))
                  ((<= 12 (length data) 1048576))
                  (mime (cond ((string-prefix-p (unibyte-string 137 80 78 71 13 10 26 10) data) "image/png")
                              ((string-prefix-p (unibyte-string 255 216 255) data) "image/jpeg")
                              ((or (string-prefix-p "GIF87a" data) (string-prefix-p "GIF89a" data)) "image/gif")
                              ((and (string-prefix-p "RIFF" data)
                                    (string= (substring data 8 12) "WEBP")) "image/webp"))))
        (json-encode (list (cons 'mime mime) (cons 'data (base64-encode-string data t))))))))

(provide 'loki-assistant-agent-shell-stream)
;;; loki-assistant-agent-shell-stream.el ends here
