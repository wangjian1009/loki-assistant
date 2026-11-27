;;; loki-assistant-agent-shell-stream.el --- Agent Shell 会话同步 -*- lexical-binding: t; -*-

;;; Code:

(require 'cl-lib)
(require 'json)
(require 'map)
(require 'subr-x)

(declare-function agent-shell-buffers "agent-shell")
(declare-function agent-shell--live-prompt-start "agent-shell-prompt")

;; 块和批次大小按字符数计算，留出 UTF-8 与 JSON 转义空间。
(defconst loki-assistant-agent-shell-stream--block-size 2048)
(defconst loki-assistant-agent-shell-stream--batch-size 20)
(defconst loki-assistant-agent-shell-stream--journal-limit 128)

(cl-defstruct loki-assistant-agent-shell-stream--state
  tick blocks (revision 0) snapshot-index (batches (make-hash-table :test #'eql)))

(defvar-local loki-assistant-agent-shell-stream--state nil)
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
  "提取 FROM 到 TO 的片段 STATE，忽略折叠指示和编辑器属性。"
  (let* ((id (alist-get :qualified-id state))
         (kind (loki-assistant-agent-shell-stream--kind id)) labels body)
    (unless (string-prefix-p "bootstrapping-" id)
      (while (< from to)
        (let* ((section (get-text-property from 'agent-shell-ui-section))
               (next (next-single-property-change from 'agent-shell-ui-section nil to)))
          (pcase section
            ((or 'label-left 'label-right)
             (push (buffer-substring-no-properties from next) labels))
            ('body (push (buffer-substring-no-properties from next) body)))
          (setq from next)))
      (let ((title (string-trim (loki-assistant-agent-shell-stream--plain
                                 (string-join (nreverse labels) " "))))
            (content (string-trim (loki-assistant-agent-shell-stream--plain
                                   (apply #'concat (nreverse body))))))
        (unless (and (string-empty-p title) (string-empty-p content))
          (list id kind title content))))))

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
                        (list (format "user-%d" position) "user" "" content)))))))
          (when fragment
            (pcase-let ((`(,id ,kind ,title ,body) fragment))
              ;; 行号区分由 Emacs 重用内部标识所产生的多个实际片段。
              (let ((offset 0) (length (length body))
                    (key (concat "block-" (number-to-string position) "-" id)))
                (while (or (zerop offset) (< offset length))
                  (push `((id . ,(format "%s:%d" key offset))
                          (kind . ,kind)
                          ,@(when (and (zerop offset) (not (string-empty-p title)))
                              `((title . ,(substring title 0 (min 256 (length title))))))
                          (text . ,(substring body offset (min length (+ offset loki-assistant-agent-shell-stream--block-size)))))
                        rows)
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

(defun loki-assistant-agent-shell-stream--refresh (state)
  "更新 STATE 的片段和变更记录。"
  (let ((tick (buffer-chars-modified-tick)))
    (unless (eql tick (loki-assistant-agent-shell-stream--state-tick state))
      (let* ((old (loki-assistant-agent-shell-stream--state-blocks state))
             (blocks (loki-assistant-agent-shell-stream--blocks))
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

(defun loki-assistant-agent-shell-stream-read (session-id revision)
  "返回 SESSION-ID 自 REVISION 之后的一批可读会话事件 JSON。"
  (let* ((candidate (get-buffer session-id))
         (buffer (and candidate (memq candidate (agent-shell-buffers)) candidate)))
    (if (not buffer)
        (json-encode `((kind . ,(if revision "closed" "missing")) (revision . ,(or revision 0))))
      (with-current-buffer buffer
        (let ((state (or loki-assistant-agent-shell-stream--state
                         (setq loki-assistant-agent-shell-stream--state
                               (make-loki-assistant-agent-shell-stream--state
                                :revision (ash (cl-incf loki-assistant-agent-shell-stream--epoch) 32))))))
          (loki-assistant-agent-shell-stream--refresh state)
          (let* ((first (loki-assistant-agent-shell-stream--state-snapshot-index state))
                 (last (loki-assistant-agent-shell-stream--state-revision state))
                 (next (if (and revision (<= first revision last)) (1+ revision) first))
                 (batch (gethash next (loki-assistant-agent-shell-stream--state-batches state))))
            (if batch (json-encode batch) "null")))))))

(provide 'loki-assistant-agent-shell-stream)
;;; loki-assistant-agent-shell-stream.el ends here
