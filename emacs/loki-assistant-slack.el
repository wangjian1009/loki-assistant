;;; loki-assistant-slack.el --- Slack 会话与线程 -*- lexical-binding: t; -*-

;;; Code:

(require 'loki-assistant)
(declare-function consult--read "consult")
(declare-function consult--lookup-cdr "consult")

(defcustom loki-assistant-slack-refresh-interval 15
  "可见 Slack 窗口自动刷新的秒数。"
  :type 'number :group 'loki-assistant)

(defvar-local loki-assistant-slack--session nil)
(defvar-local loki-assistant-slack--team-id nil)
(defvar loki-assistant-slack--selection 0)
(defvar-local loki-assistant-slack--conversation nil)
(defvar-local loki-assistant-slack--thread nil)
(defvar-local loki-assistant-slack--title nil)
(defvar-local loki-assistant-slack--cursor nil)
(defvar-local loki-assistant-slack--timer nil)
(defvar-local loki-assistant-slack--generation 0)
(defvar-local loki-assistant-slack--busy nil)
(defvar-local loki-assistant-slack--messages nil)
(defvar-local loki-assistant-slack--root nil)
(defvar-local loki-assistant-slack--position nil)
(defvar-local loki-assistant-slack--first-unread nil)
(defvar-local loki-assistant-slack--draft-key nil)
(defvar-local loki-assistant-slack--draft-text nil)
(defvar-local loki-assistant-slack--draft-state nil)

(cl-defun loki-assistant-slack--request (method path callback &key session params body key team-id)
  "以 SESSION 调用 METHOD PATH，将结果交给 CALLBACK。"
  (let ((token (condition-case error
                   (loki-assistant--module-api-token "slack")
                 (error (funcall callback
                                 (loki-assistant--failure
                                  "slack-credentials-unavailable"
                                  (error-message-string error)))
                        nil))))
    (when token
      (loki-assistant-request
       method path :params (if team-id (cons (cons 'team_id team-id) params) params) :body body
       :headers (append (list (cons "Authorization" (concat "Bearer " token)))
                        (when session (list (cons "X-Loki-Slack-Session" session)))
                        (when key (list (cons "Idempotency-Key" key))))
       :callback callback :timeout 20))))

(defun loki-assistant-slack--show (response)
  "显示 RESPONSE 中的错误说明。"
  (message "%s" (or (plist-get response :error-message) "Slack 请求失败")))

(defun loki-assistant-slack--non-null (value)
  "把 JSON 空值 VALUE 转成 nil。"
  (unless (eq value :null) value))

(defun loki-assistant-slack--path (conversation thread resource)
  "构造 CONVERSATION 下 THREAD 的 RESOURCE 路径。"
  (concat "/slack/conversations/" (url-hexify-string conversation)
          (when thread (concat "/threads/" (url-hexify-string thread)))
          "/" resource))

(defun loki-assistant-slack--stop ()
  "停止当前缓冲区的定时刷新。"
  (when (timerp loki-assistant-slack--timer)
    (cancel-timer loki-assistant-slack--timer)
    (setq loki-assistant-slack--timer nil)))

(defun loki-assistant-slack--watch ()
  "在此缓冲区可见时刷新。"
  (loki-assistant-slack--stop)
  (setq loki-assistant-slack--timer
        (run-at-time
         loki-assistant-slack-refresh-interval loki-assistant-slack-refresh-interval
         (lambda (buffer)
           (when (and (buffer-live-p buffer) (get-buffer-window buffer t))
             (with-current-buffer buffer
               (unless loki-assistant-slack--busy
                 (loki-assistant-slack-refresh)))))
         (current-buffer)))
  (add-hook 'kill-buffer-hook #'loki-assistant-slack--stop nil t))

(defun loki-assistant-slack--error-header (response)
  "从 RESPONSE 构造可见的错误状态。"
  (let ((message (plist-get response :error-message)))
    (concat " Slack · " (replace-regexp-in-string "%" "%%" (or message "请求失败")))))

(defun loki-assistant-slack--time (ts)
  "将消息时间 TS 转成简短时间。"
  (if (stringp ts)
      (format-time-string "%H:%M" (seconds-to-time (string-to-number ts)))
    ""))

(defun loki-assistant-slack-refresh ()
  "刷新当前 Slack 聊天。"
  (interactive)
  (loki-assistant-slack--messages-refresh))

(defun loki-assistant-slack-older ()
  "加载当前 Slack 聊天的更早消息。"
  (interactive)
  (unless (and loki-assistant-slack--cursor (not loki-assistant-slack--busy))
    (user-error "目前没有可加载的下一页"))
  (loki-assistant-slack--messages-refresh t))

(defun loki-assistant-slack--open-view (team-id session conversation thread title)
  "打开 TEAM-ID 和 SESSION 对应的 CONVERSATION 或 THREAD。"
  (let ((buffer (get-buffer-create (format "*Slack %s/%s%s*" conversation session
                                          (if thread (concat "/" thread) "")))))
    (with-current-buffer buffer
      (unless (derived-mode-p 'loki-assistant-slack-message-mode)
        (loki-assistant-slack-message-mode)
        (setq loki-assistant-slack--session session
              loki-assistant-slack--team-id team-id
              loki-assistant-slack--conversation conversation
              loki-assistant-slack--thread thread
              loki-assistant-slack--title title)
        (loki-assistant-slack--watch))
      (loki-assistant-slack--messages-refresh))
    (if (and thread (>= (frame-width) 110))
        (select-window (display-buffer-in-side-window
                        buffer '((side . right) (slot . 1) (window-width . 0.31))))
      (pop-to-buffer-same-window buffer))))

(defun loki-assistant-slack--messages-refresh (&optional older)
  "刷新当前视图的消息。OLDER 表示读取更早一页。"
  (let ((buffer (current-buffer))
        (generation (cl-incf loki-assistant-slack--generation))
        (cursor (and older loki-assistant-slack--cursor)))
    (setq loki-assistant-slack--busy t)
    (loki-assistant-slack--request
     "GET" (loki-assistant-slack--path loki-assistant-slack--conversation
                                       loki-assistant-slack--thread "messages")
     (lambda (response)
       (when (buffer-live-p buffer)
         (with-current-buffer buffer
           (when (= generation loki-assistant-slack--generation)
             (setq loki-assistant-slack--busy nil)
             (if (not (plist-get response :ok))
                 (setq header-line-format (loki-assistant-slack--error-header response))
               (let ((data (plist-get response :data)))
                 (when (or cursor (not loki-assistant-slack--messages))
                   (setq loki-assistant-slack--cursor
                         (loki-assistant-slack--non-null (alist-get 'next_cursor data))))
                 (setq loki-assistant-slack--root (alist-get 'root data)
                       loki-assistant-slack--first-unread
                       (loki-assistant-slack--non-null (alist-get 'first_unread_ts data))
                       loki-assistant-slack--position
                       (loki-assistant-slack--non-null (alist-get 'read_through_ts data))
                       loki-assistant-slack--messages
                       (loki-assistant-slack--merge loki-assistant-slack--messages data))
                 (loki-assistant-slack--render (equal (alist-get 'sync_state data) "cached"))))))))
     :session loki-assistant-slack--session
     :params (when cursor (list (cons 'cursor cursor))))))

(defun loki-assistant-slack--stamp< (left right)
  "按精确时间比较 Slack 时间戳 LEFT 和 RIGHT。"
  (let* ((l (split-string left "[.]")) (r (split-string right "[.]"))
         (ls (string-to-number (car l))) (rs (string-to-number (car r))))
    (if (= ls rs)
        (string< (substring (concat (cadr l) "000000000") 0 9)
                 (substring (concat (cadr r) "000000000") 0 9))
      (< ls rs))))

(defun loki-assistant-slack--merge (previous data)
  "将 DATA 的已核对区间和新消息合并到 PREVIOUS。"
  (let* ((incoming (append (alist-get 'items data) nil))
         (range (loki-assistant-slack--non-null (alist-get 'covered_range data)))
         (lower (loki-assistant-slack--non-null (alist-get 'from_ts range)))
         (upper (loki-assistant-slack--non-null (alist-get 'through_ts range)))
         (ids (make-hash-table :test #'equal)))
    (dolist (item incoming) (puthash (alist-get 'ts item) t ids))
    (dolist (item previous)
      (let ((ts (alist-get 'ts item)))
        (unless (or (gethash ts ids)
                    (and range (or (not lower) (not (loki-assistant-slack--stamp< ts lower)))
                         (or (not upper) (not (loki-assistant-slack--stamp< upper ts)))))
          (push item incoming))))
    (sort incoming (lambda (left right)
                     (loki-assistant-slack--stamp< (alist-get 'ts left) (alist-get 'ts right))))))

(defun loki-assistant-slack--print-message (item &optional root)
  "插入消息 ITEM。ROOT 表示独立展示的线程根消息。"
  (let* ((start (point))
         (author (loki-assistant-slack--non-null (alist-get 'author item)))
         (ts (alist-get 'ts item))
         (replies (alist-get 'reply_count item)))
    (insert (propertize (or (alist-get 'name author) "Slack") 'face 'font-lock-keyword-face)
            "  " (loki-assistant-slack--time ts)
            (if (eq (alist-get 'edited item) t) " · 已编辑\n" "\n")
            (or (alist-get 'text item) "") "\n")
    (when (and (not loki-assistant-slack--thread) (numberp replies) (> replies 0))
      (insert-text-button (format "%d 条回复" replies)
                          'follow-link t 'action (lambda (_) (loki-assistant-slack-open-thread ts)))
      (insert "\n"))
    (insert "\n")
    (add-text-properties start (point) (list 'loki-assistant-slack-message item
                                             'loki-assistant-slack-root root))))

(defun loki-assistant-slack--render (cached)
  "重绘当前 Slack 消息。CACHED 非空时显示缓存状态。"
  (let* ((at (get-text-property (point) 'loki-assistant-slack-message))
         (ts (alist-get 'ts at))
         (old (point))
         (inhibit-read-only t)
         destination)
    (erase-buffer)
    (setq header-line-format
          (format " %s%s%s   o 更早   t 线程   r 回复   m 标为已读"
                  (replace-regexp-in-string "%" "%%" loki-assistant-slack--title)
                  (if loki-assistant-slack--thread " · 线程" "")
                  (if cached " · 缓存" "")))
    (when loki-assistant-slack--root
      (loki-assistant-slack--print-message loki-assistant-slack--root t)
      (insert (propertize "线程回复\n\n" 'face 'bold)))
    (let ((marked nil))
      (dolist (item loki-assistant-slack--messages)
        (let ((item-ts (alist-get 'ts item)))
          (when (and (not marked) (equal loki-assistant-slack--first-unread item-ts)
                     (or (not loki-assistant-slack--position)
                         (loki-assistant-slack--stamp< loki-assistant-slack--position item-ts)))
            (insert (propertize "新消息\n\n" 'face 'font-lock-warning-face))
            (setq marked t))
          (when (equal ts item-ts) (setq destination (point)))
          (loki-assistant-slack--print-message item))))
    (when (= (point-min) (point-max)) (insert "暂无消息\n"))
    (goto-char (or destination (min old (point-max))))))

(defun loki-assistant-slack-open-thread (&optional ts)
  "打开当前消息的线程，或直接打开 TS。"
  (interactive)
  (unless ts
    (let ((item (get-text-property (point) 'loki-assistant-slack-message)))
      (setq ts (or (loki-assistant-slack--non-null (alist-get 'thread_ts item))
                   (alist-get 'ts item)))))
  (unless ts (user-error "请选择一条消息"))
  (loki-assistant-slack--open-view loki-assistant-slack--team-id loki-assistant-slack--session
                                    loki-assistant-slack--conversation
                                    (or loki-assistant-slack--thread ts) loki-assistant-slack--title))

(defun loki-assistant-slack-mark-read ()
  "将当前视图标为已读到光标所在消息。"
  (interactive)
  (let* ((current (get-text-property (point) 'loki-assistant-slack-message))
         (message (if (get-text-property (point) 'loki-assistant-slack-root) nil current))
         (ts (alist-get 'ts message))
         (buffer (current-buffer)))
    (unless ts (user-error "请选择此视图中的一条消息"))
    (loki-assistant-slack--request
     "PUT" (loki-assistant-slack--path loki-assistant-slack--conversation
                                       loki-assistant-slack--thread "read-position")
     (lambda (response)
       (if (plist-get response :ok)
           (when (buffer-live-p buffer)
             (with-current-buffer buffer
               (setq loki-assistant-slack--position
                     (alist-get 'read_through_ts (plist-get response :data)))
               (loki-assistant-slack--messages-refresh)))
         (loki-assistant-slack--show response)))
     :session loki-assistant-slack--session :body `((through_ts . ,ts)))))

(defun loki-assistant-slack-compose ()
  "在当前 Slack 会话或线程中起草消息。"
  (interactive)
  (let* ((session loki-assistant-slack--session)
         (team-id loki-assistant-slack--team-id)
         (id loki-assistant-slack--conversation)
         (thread loki-assistant-slack--thread)
         (title loki-assistant-slack--title)
         (buffer (get-buffer-create (format "*Slack 草稿 %s/%s%s*" id session
                                            (if thread (concat "/" thread) "")))))
    (unless (and team-id session id) (user-error "请先打开会话"))
    (with-current-buffer buffer
      (unless (derived-mode-p 'loki-assistant-slack-draft-mode)
        (loki-assistant-slack-draft-mode)
        (setq loki-assistant-slack--session session loki-assistant-slack--conversation id
              loki-assistant-slack--team-id team-id
              loki-assistant-slack--thread thread loki-assistant-slack--title title
              header-line-format (format " 发送到 %s%s   C-c C-c 发送   C-c C-k 取消"
                                         (replace-regexp-in-string "%" "%%" title)
                                         (if thread " · 线程" "")))))
    (pop-to-buffer buffer '(display-buffer-below-selected))))

(defun loki-assistant-slack--uuid ()
  "为一次主动提交生成 UUID。"
  (let ((hash (secure-hash 'sha256 (format "%S:%S:%S:%S" (current-time)
                                          (random) (emacs-pid) (current-buffer)))))
    (format "%s-%s-4%s-8%s-%s" (substring hash 0 8) (substring hash 8 12)
            (substring hash 13 16) (substring hash 17 20) (substring hash 20 32))))

(defun loki-assistant-slack-send ()
  "提交草稿或核对之前的提交。"
  (interactive)
  (when loki-assistant-slack--busy (user-error "正在查询 Slack 发送结果"))
  (when (eq loki-assistant-slack--draft-state 'unknown)
    (unless (yes-or-no-p "发送结果未知，已经核对会话并确认再次发送？")
      (user-error "已保留草稿"))
    (setq loki-assistant-slack--draft-key nil loki-assistant-slack--draft-state nil))
  (if loki-assistant-slack--draft-key
      (loki-assistant-slack--poll-send (current-buffer) loki-assistant-slack--draft-key 0)
    (let ((content (buffer-substring-no-properties (point-min) (point-max))))
      (when (string-empty-p (string-trim content)) (user-error "请输入消息"))
      (setq loki-assistant-slack--draft-key (loki-assistant-slack--uuid)
            loki-assistant-slack--draft-text content)
      (loki-assistant-slack--submit (current-buffer) loki-assistant-slack--draft-key t))))

(defun loki-assistant-slack--submit (buffer key retry)
  "提交 BUFFER 的 KEY。RETRY 表示允许查询后复用该键。"
  (with-current-buffer buffer
    (setq loki-assistant-slack--busy t buffer-read-only t)
    (loki-assistant-slack--request
     "POST" (loki-assistant-slack--path loki-assistant-slack--conversation
                                        loki-assistant-slack--thread "messages")
     (lambda (response)
       (when (buffer-live-p buffer)
         (with-current-buffer buffer
           (cond
            ((plist-get response :ok)
             (loki-assistant-slack--send-result buffer key (plist-get response :data) 0))
            ((and retry (not (plist-get response :status)))
             (loki-assistant-slack--poll-send buffer key 0 t))
            (t
             (setq loki-assistant-slack--busy nil buffer-read-only nil)
             (when (plist-get response :status) (setq loki-assistant-slack--draft-key nil))
             (loki-assistant-slack--show response))))))
     :session loki-assistant-slack--session :key key
     :body `((text . ,loki-assistant-slack--draft-text)))))

(defun loki-assistant-slack--poll-send (buffer key count &optional retry)
  "查询 BUFFER 中 KEY 的发送结果，当前已查询 COUNT 次。"
  (when (buffer-live-p buffer)
    (with-current-buffer buffer
      (setq loki-assistant-slack--busy t buffer-read-only t)
      (loki-assistant-slack--request
       "GET" (concat "/slack/send-requests/" key)
       (lambda (response)
         (when (buffer-live-p buffer)
           (with-current-buffer buffer
             (cond
              ((plist-get response :ok)
               (loki-assistant-slack--send-result buffer key (plist-get response :data) count))
              ((and retry (equal (plist-get response :error-code) "slack-send-request-not-found"))
               (loki-assistant-slack--submit buffer key nil))
              (t
               (setq loki-assistant-slack--busy nil buffer-read-only nil)
               (loki-assistant-slack--show response))))))
       :session loki-assistant-slack--session :team-id loki-assistant-slack--team-id))))

(defun loki-assistant-slack--send-result (buffer key data count)
  "在 BUFFER 处理 KEY 的 DATA，最多轮询有限次数 COUNT。"
  (with-current-buffer buffer
    (pcase (alist-get 'state data)
      ("sent"
       (set-buffer-modified-p nil)
       (kill-buffer buffer)
       (message "Slack 消息已发送"))
      ("pending"
       (if (< count 20)
           (run-at-time 1 nil #'loki-assistant-slack--poll-send buffer key (1+ count))
         (setq loki-assistant-slack--busy nil buffer-read-only nil)
         (message "消息仍在发送，按 C-c C-c 重新查询结果")))
      ("failed"
       (setq loki-assistant-slack--busy nil loki-assistant-slack--draft-key nil buffer-read-only nil)
       (message "%s" (or (alist-get 'message (alist-get 'error data)) "Slack 发送失败")))
      (_
       (setq loki-assistant-slack--busy nil loki-assistant-slack--draft-state 'unknown buffer-read-only nil)
       (message "Slack 发送结果未知，草稿已保留。请刷新会话核对")))))

(defun loki-assistant-slack-cancel-draft ()
  "关闭当前 Slack 草稿。"
  (interactive)
  (when loki-assistant-slack--busy (user-error "正在查询 Slack 发送结果"))
  (when (or (string-empty-p (buffer-string)) (yes-or-no-p "取消当前 Slack 草稿？"))
    (set-buffer-modified-p nil)
    (kill-buffer (current-buffer))))

(defun loki-assistant-slack-next-message ()
  "移动到下一条 Slack 消息。"
  (interactive)
  (goto-char (next-single-property-change (point) 'loki-assistant-slack-message nil (point-max))))

(defun loki-assistant-slack-previous-message ()
  "移动到上一条 Slack 消息。"
  (interactive)
  (when (> (point) (point-min))
    (goto-char (previous-single-property-change (point) 'loki-assistant-slack-message nil (point-min)))))

(defvar loki-assistant-slack-message-mode-map
  (let ((map (make-sparse-keymap)))
    (set-keymap-parent map special-mode-map)
    (define-key map (kbd "n") #'loki-assistant-slack-next-message)
    (define-key map (kbd "p") #'loki-assistant-slack-previous-message)
    (define-key map (kbd "g") #'loki-assistant-slack-refresh)
    (define-key map (kbd "o") #'loki-assistant-slack-older)
    (define-key map (kbd "t") #'loki-assistant-slack-open-thread)
    (define-key map (kbd "r") #'loki-assistant-slack-compose)
    (define-key map (kbd "m") #'loki-assistant-slack-mark-read)
    map))

(define-derived-mode loki-assistant-slack-message-mode special-mode "Slack 消息"
  "阅读 Slack 会话或线程。")

(defvar loki-assistant-slack-draft-mode-map
  (let ((map (make-sparse-keymap)))
    (set-keymap-parent map text-mode-map)
    (define-key map (kbd "C-c C-c") #'loki-assistant-slack-send)
    (define-key map (kbd "C-c C-k") #'loki-assistant-slack-cancel-draft)
    map))

(define-derived-mode loki-assistant-slack-draft-mode text-mode "Slack 草稿"
  "起草 Slack 会话消息或线程回复。"
  (setq-local buffer-offer-save nil))

(defun loki-assistant-slack--room-label (room)
  "返回 ROOM 在聊天选择中的名称。"
  (let ((name (replace-regexp-in-string "[\n\r]+" " " (alist-get 'title room))))
    (if (member (alist-get 'kind room) '("public_channel" "private_channel"))
        (concat "#" name)
      name)))

(defun loki-assistant-slack--choose-room (window selection rooms)
  "在 WINDOW 通过 Consult 从 ROOMS 中选择聊天。SELECTION 标记当前请求。"
  (when (and (= selection loki-assistant-slack--selection) (window-live-p window))
    (cond
     ((active-minibuffer-window) (message "请退出当前 minibuffer 后重新选择 Slack 聊天。"))
     ((null rooms) (message "没有可用的 Slack 聊天，请检查工作区授权。"))
     (t
      (let ((counts (make-hash-table :test #'equal)))
        (dolist (room rooms)
          (cl-incf (gethash (loki-assistant-slack--room-label room) counts 0)))
        (with-selected-window window
          (let* ((choices
                  (mapcar
                   (lambda (room)
                     (let ((name (loki-assistant-slack--room-label room)))
                       (cons (if (> (gethash name counts) 1)
                                 (format "%s [%s]" name (alist-get 'id room))
                               name)
                             room)))
                   rooms))
                 (room (consult--read choices :prompt "Slack 聊天："
                                      :sort nil :require-match t
                                      :lookup #'consult--lookup-cdr)))
            (when room
              (loki-assistant-slack--open-view
               (alist-get 'team_id room) (alist-get 'session_id room)
               (alist-get 'id room) nil (loki-assistant-slack--room-label room))))))))))

(defun loki-assistant-slack--load-rooms (window selection &optional cursor pages)
  "读取聊天页，完成后在 WINDOW 发起 SELECTION。"
  (loki-assistant-slack--request
   "GET" "/slack/conversations"
   (lambda (response)
     (when (and (= selection loki-assistant-slack--selection) (window-live-p window))
       (if (not (plist-get response :ok))
           (loki-assistant-slack--show response)
         (let* ((data (plist-get response :data))
                (next (loki-assistant-slack--non-null (alist-get 'next_cursor data))))
           (push (append (alist-get 'items data) nil) pages)
           (if next
               (loki-assistant-slack--load-rooms window selection next pages)
             (run-at-time 0 nil #'loki-assistant-slack--choose-room window selection
                          (apply #'append (nreverse pages))))))))
   :team-id nil :params (append '((limit . 100)) (when cursor (list (cons 'cursor cursor))))))

;;;###autoload
(defun loki-assistant-slack ()
  "使用 Consult 选择 Slack 聊天并打开消息视图。"
  (interactive)
  (unless (require 'consult nil t)
    (user-error "选择 Slack 聊天需要安装 Consult"))
  (loki-assistant-slack--load-rooms (selected-window)
                                   (cl-incf loki-assistant-slack--selection)))

(define-key loki-assistant-command-map (kbd "s") #'loki-assistant-slack)

(provide 'loki-assistant-slack)
;;; loki-assistant-slack.el ends here
