;;; loki-assistant-slack.el --- Slack 会话与线程 -*- lexical-binding: t; -*-

;;; Code:

(require 'loki-assistant)
(require 'tabulated-list)

(defcustom loki-assistant-slack-refresh-interval 15
  "可见 Slack 窗口自动刷新的秒数。"
  :type 'number :group 'loki-assistant)

(defvar-local loki-assistant-slack--session nil)
(defvar-local loki-assistant-slack--project nil)
(defvar-local loki-assistant-slack--project-name nil)
(defvar-local loki-assistant-slack--conversation nil)
(defvar-local loki-assistant-slack--thread nil)
(defvar-local loki-assistant-slack--title nil)
(defvar-local loki-assistant-slack--unread t)
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

(cl-defun loki-assistant-slack--request (method path callback &key session params body key (project loki-assistant-slack--project))
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
       method path :params params :body body
       :headers (append (list (cons "Authorization" (concat "Bearer " token)))
                        (when project (list (cons "X-Loki-Slack-Project" project)))
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

(defun loki-assistant-slack--status-label (state)
  "返回 STATE 的简短中文说明。"
  (or (cdr (assoc state '(("connected" . "已连接") ("connecting" . "连接中")
                         ("reconnecting" . "正在重连") ("authorized" . "已授权")
                         ("unconfigured" . "待配置") ("authorization_required" . "待授权")
                         ("reauthorization_required" . "需重新授权")
                         ("credentials_unavailable" . "凭据暂不可用") ("stopped" . "连接已停止"))))
      "状态未知"))

(defun loki-assistant-slack--list-refresh (&optional older)
  "刷新会话列表。OLDER 表示追加下一页。"
  (let ((buffer (current-buffer))
        (generation (cl-incf loki-assistant-slack--generation))
        (cursor (and older loki-assistant-slack--cursor)))
    (setq loki-assistant-slack--busy t)
    (loki-assistant-slack--request
     "GET" "/slack/status"
     (lambda (response)
       (when (buffer-live-p buffer)
         (with-current-buffer buffer
           (when (= generation loki-assistant-slack--generation)
             (if (not (plist-get response :ok))
                 (setq loki-assistant-slack--busy nil
                       header-line-format (loki-assistant-slack--error-header response))
               (let* ((data (plist-get response :data))
                      (session (loki-assistant-slack--non-null (alist-get 'session data)))
                      (id (alist-get 'session_id session))
                      (state (alist-get 'authorization_state data)))
                 (unless (equal id loki-assistant-slack--session)
                   (setq tabulated-list-entries nil cursor nil)
                   (tabulated-list-print t))
                 (setq loki-assistant-slack--session id
                       header-line-format
                       (format " %s · %s%s   [%s]   P 项目   u 筛选   C 配置"
                               (or loki-assistant-slack--project-name "Slack")
                               (loki-assistant-slack--status-label
                                (if id (alist-get 'socket_state data) state))
                               (if session (concat " · " (alist-get 'team_name session)) "")
                               (if loki-assistant-slack--unread "未读" "全部")))
                 (if id
                     (loki-assistant-slack--list-page buffer generation id cursor)
                   (setq loki-assistant-slack--busy nil)))))))))))

(defun loki-assistant-slack--error-header (response)
  "从 RESPONSE 构造可见的错误状态。"
  (let ((message (plist-get response :error-message)))
    (concat " Slack · " (replace-regexp-in-string "%" "%%" (or message "请求失败")))))

(defun loki-assistant-slack--time (ts)
  "将消息时间 TS 转成简短时间。"
  (if (stringp ts)
      (format-time-string "%H:%M" (seconds-to-time (string-to-number ts)))
    ""))

(defun loki-assistant-slack--list-page (buffer generation session cursor)
  "更新 BUFFER 上属于 SESSION 和 GENERATION 的一页会话。"
  (loki-assistant-slack--request
   "GET" "/slack/conversations"
   (lambda (response)
     (when (buffer-live-p buffer)
       (with-current-buffer buffer
         (when (= generation loki-assistant-slack--generation)
           (setq loki-assistant-slack--busy nil)
           (if (not (plist-get response :ok))
               (setq header-line-format (loki-assistant-slack--error-header response))
             (let* ((data (plist-get response :data))
                    (items (append (alist-get 'items data) nil))
                    (rows (mapcar #'loki-assistant-slack--row items)))
               (setq loki-assistant-slack--cursor
                     (loki-assistant-slack--non-null (alist-get 'next_cursor data)))
               (setq tabulated-list-entries
                     (if cursor
                         (append (cl-remove-if
                                  (lambda (entry) (assoc (car entry) rows))
                                  tabulated-list-entries) rows)
                       rows))
               (when (equal (alist-get 'sync_state data) "cached")
                 (setq header-line-format (concat header-line-format " · 缓存")))
               (tabulated-list-print t)))))))
   :session session :params (append (when loki-assistant-slack--unread '((unread . "true")))
                                    (when cursor (list (cons 'cursor cursor))))))

(defun loki-assistant-slack--row (data)
  "将会话 DATA 转成表格行。"
  (let ((preview (loki-assistant-slack--non-null (alist-get 'preview data))))
    (list (alist-get 'id data)
          (vector (alist-get 'title data)
                  (number-to-string (or (alist-get 'unread_count data) 0))
                  (loki-assistant-slack--time (alist-get 'last_activity_ts data))
                  (if preview (replace-regexp-in-string "[\n\r]+" " " preview) "")))))

(defun loki-assistant-slack-toggle-unread ()
  "切换未读和全部会话。"
  (interactive)
  (setq loki-assistant-slack--unread (not loki-assistant-slack--unread)
        loki-assistant-slack--cursor nil)
  (loki-assistant-slack-refresh))

(defun loki-assistant-slack-refresh ()
  "刷新当前 Slack 视图。"
  (interactive)
  (if (derived-mode-p 'loki-assistant-slack-list-mode)
      (loki-assistant-slack--list-refresh)
    (loki-assistant-slack--messages-refresh)))

(defun loki-assistant-slack-older ()
  "加载当前 Slack 视图的更早内容。"
  (interactive)
  (unless (and loki-assistant-slack--cursor (not loki-assistant-slack--busy))
    (user-error "目前没有可加载的下一页"))
  (if (derived-mode-p 'loki-assistant-slack-list-mode)
      (loki-assistant-slack--list-refresh t)
    (loki-assistant-slack--messages-refresh t)))

(defun loki-assistant-slack-open-conversation ()
  "打开列表中的会话。"
  (interactive)
  (let ((id (tabulated-list-get-id))
        (row (tabulated-list-get-entry)))
    (unless id (user-error "请选择会话"))
    (loki-assistant-slack--open-view loki-assistant-slack--session id nil (aref row 0))))

(defun loki-assistant-slack--open-view (session conversation thread title)
  "打开 SESSION 中 CONVERSATION 的时间线或 THREAD。"
  (let ((project loki-assistant-slack--project)
        (buffer (get-buffer-create (format "*Slack %s %s%s*" session conversation
                                          (if thread (concat "/" thread) "")))))
    (with-current-buffer buffer
      (unless (derived-mode-p 'loki-assistant-slack-message-mode)
        (loki-assistant-slack-message-mode)
        (setq loki-assistant-slack--session session
              loki-assistant-slack--project project
              loki-assistant-slack--conversation conversation
              loki-assistant-slack--thread thread
              loki-assistant-slack--title title)
        (loki-assistant-slack--watch))
      (loki-assistant-slack--messages-refresh))
    (if (>= (frame-width) 110)
        (let ((window (display-buffer-in-side-window
                       buffer `((side . right) (slot . ,(if thread 1 0))
                                (window-width . ,(if thread 0.31 0.5))))))
          (select-window window))
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
  (loki-assistant-slack--open-view loki-assistant-slack--session loki-assistant-slack--conversation
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
         (project loki-assistant-slack--project)
         (id loki-assistant-slack--conversation)
         (thread loki-assistant-slack--thread)
         (title loki-assistant-slack--title)
         (buffer (get-buffer-create (format "*Slack 草稿 %s %s%s*" session id
                                            (if thread (concat "/" thread) "")))))
    (unless (and session id) (user-error "请先打开会话"))
    (with-current-buffer buffer
      (unless (derived-mode-p 'loki-assistant-slack-draft-mode)
        (loki-assistant-slack-draft-mode)
        (setq loki-assistant-slack--session session loki-assistant-slack--conversation id
              loki-assistant-slack--project project
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
       :session loki-assistant-slack--session))))

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

(defun loki-assistant-slack-switch-project ()
  "从状态中心选择 Slack 项目。"
  (interactive)
  (loki-assistant-slack--select-project (current-buffer) t))

(defun loki-assistant-slack--select-project (buffer &optional prompt wanted)
  "为 BUFFER 选择 WANTED 项目，PROMPT 非空时询问项目。"
  (loki-assistant-request
   "GET" "/status/slack/projects"
   :callback
   (lambda (response)
     (when (buffer-live-p buffer)
       (with-current-buffer buffer
         (if (not (plist-get response :ok))
             (setq header-line-format (loki-assistant-slack--error-header response))
           (let* ((projects (append (alist-get 'value (plist-get response :data)) nil))
                  (choices (mapcar (lambda (row)
                                     (cons (format "%s [%s]" (alist-get 'name row) (alist-get 'id row)) row))
                                   projects))
                  (selected (cond
                             (wanted (cl-find wanted projects :key (lambda (row) (alist-get 'id row)) :test #'equal))
                             ((and prompt choices)
                              (cdr (assoc (completing-read "Slack 项目：" choices nil t) choices)))
                             (t (or (cl-find loki-assistant-slack--project projects
                                              :key (lambda (row) (alist-get 'id row)) :test #'equal)
                                    (car projects))))))
             (if selected
                 (progn
                   (unless (equal loki-assistant-slack--project (alist-get 'id selected))
                     (setq loki-assistant-slack--session nil tabulated-list-entries nil))
                   (setq loki-assistant-slack--project (alist-get 'id selected)
                         loki-assistant-slack--project-name (replace-regexp-in-string
                                                              "%" "%%" (alist-get 'name selected)))
                   (loki-assistant-slack-refresh))
               (setq header-line-format " Slack · 请按 C 配置项目")))))))))

(defvar loki-assistant-slack-list-mode-map
  (let ((map (make-sparse-keymap)))
    (set-keymap-parent map tabulated-list-mode-map)
    (define-key map (kbd "RET") #'loki-assistant-slack-open-conversation)
    (define-key map (kbd "n") #'next-line)
    (define-key map (kbd "p") #'previous-line)
    (define-key map (kbd "g") #'loki-assistant-slack-refresh)
    (define-key map (kbd "o") #'loki-assistant-slack-older)
    (define-key map (kbd "u") #'loki-assistant-slack-toggle-unread)
    (define-key map (kbd "P") #'loki-assistant-slack-switch-project)
    (define-key map (kbd "C") #'loki-assistant-config)
    map))

(define-derived-mode loki-assistant-slack-list-mode tabulated-list-mode "Slack 会话"
  "Slack 私信与多人私信会话列表。"
  (setq tabulated-list-format [("会话" 25 t) ("未读" 5 nil) ("时间" 7 nil) ("摘要" 0 nil)]
        tabulated-list-padding 1)
  (tabulated-list-init-header))

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

;;;###autoload
(defun loki-assistant-slack (&optional project)
  "打开 Slack 未读会话，可指定 PROJECT。"
  (interactive)
  (let ((buffer (get-buffer-create "*Slack 会话*")))
    (with-current-buffer buffer
      (unless (derived-mode-p 'loki-assistant-slack-list-mode)
        (loki-assistant-slack-list-mode)
        (loki-assistant-slack--watch))
      (loki-assistant-slack--select-project buffer nil project))
    (pop-to-buffer buffer)))

(define-key loki-assistant-command-map (kbd "s") #'loki-assistant-slack)

(provide 'loki-assistant-slack)
;;; loki-assistant-slack.el ends here
