;;; loki-assistant-slack-chat.el --- Slack 会话和内嵌线程 -*- lexical-binding: t; -*-

;;; Code:

(require 'button)
(require 'cl-lib)
(require 'comint)
(require 'seq)
(require 'url)
(require 'loki-assistant)

(declare-function consult--read "consult")
(declare-function consult--tofu-append "consult")
(declare-function consult--async-sink "consult")
(declare-function shell-maker-start-v2 "shell-maker")
(declare-function make-shell-maker-config "shell-maker")

(cl-defstruct loki-assistant-slack--page items cursor loading loaded expanded error)
(cl-defstruct loki-assistant-slack--choice room group stamp title workspace search avatars status summary)

(defvar-local loki-assistant-slack--room nil)
(defvar-local loki-assistant-slack--timeline nil)
(defvar-local loki-assistant-slack--threads nil)
(defvar-local loki-assistant-slack--history-end nil)
(defvar-local loki-assistant-slack--reply nil)
(defvar-local loki-assistant-slack--send-request nil)
(defvar-local loki-assistant-slack--refresh-timer nil)
(defvar loki-assistant-slack--selection nil)
(defvar loki-assistant-slack--completion-choices nil)
(defvar url-http-response-status)
(defvar url-http-end-of-headers)

(cl-defun loki-assistant-slack--request (method path callback &key room params body key)
  "通过 services 发起 Slack 请求。"
  (loki-assistant-request
   method path :params params :body body :timeout 20
   :headers (append (when-let ((session (alist-get 'session_id room)))
                      (list (cons "X-Loki-Slack-Session" session)))
                    (when key (list (cons "Idempotency-Key" key))))
   :callback callback))

(defun loki-assistant-slack--path (room thread)
  "构造 ROOM 的消息路径。THREAD 非空时指向回复。"
  (concat "/slack/conversations/" (url-hexify-string (alist-get 'id room))
          (when thread (concat "/threads/" (url-hexify-string thread))) "/messages"))

(defun loki-assistant-slack--label (room)
  "生成 ROOM 的显示名称。"
  (concat (when (member (alist-get 'kind room) '("public_channel" "private_channel")) "#")
          (replace-regexp-in-string "[\n\r]+" " " (or (alist-get 'title room) (alist-get 'id room)))))

(defun loki-assistant-slack--single-line (text)
  "返回压成一行的 TEXT。"
  (when (stringp text) (string-trim (replace-regexp-in-string "[[:space:][:cntrl:]]+" " " text))))

(defun loki-assistant-slack--choice-for (room)
  "从 ROOM 的服务端候选资料生成显示内容。"
  (let* ((private (member (alist-get 'kind room) '("im" "mpim")))
         (mention (loki-assistant-slack--non-null (alist-get 'last_unread_mention_ts room)))
         (unread (or (alist-get 'unread_count room) 0))
         (last-message (loki-assistant-slack--non-null (alist-get 'last_message room)))
         (activity (alist-get 'ts last-message))
         (sender (loki-assistant-slack--single-line (alist-get 'author_name last-message)))
         (preview (loki-assistant-slack--single-line (alist-get 'preview last-message)))
         (details
          (if activity
              (delq nil (list sender
                              (format-time-string "%m-%d %H:%M" (seconds-to-time (string-to-number activity)))
                              (unless (string-empty-p (or preview "")) (truncate-string-to-width preview 60 nil nil "…"))))
            (list (if (eq (alist-get 'history_loaded room) :false) "消息摘要暂不可用" "暂无消息")))))
    (make-loki-assistant-slack--choice
     :room room :group (cond (mention 0) ((> unread 0) 1) (t 2))
     :stamp (or mention (loki-assistant-slack--non-null (alist-get 'last_activity_ts room)) activity "0.0")
     :title (loki-assistant-slack--label room)
     :workspace (or (loki-assistant-slack--single-line (alist-get 'team_name room))
                    (alist-get 'team_id room) "")
     :search (append (alist-get 'search_terms room) nil)
     :avatars (when private (or (mapcar (lambda (user) (alist-get 'avatar_url user))
                                        (alist-get 'participants room)) '(nil)))
     :status (concat (when (> unread 0) (format "%d 未读" unread))
                     (when mention (propertize " @我" 'face 'warning)))
     :summary (propertize (string-join details " · ") 'face 'shadow))))

(defun loki-assistant-slack--choice< (left right)
  "按默认顺序比较候选 LEFT 和 RIGHT。"
  (let ((a (loki-assistant-slack--choice-group left)) (b (loki-assistant-slack--choice-group right))
        (ta (loki-assistant-slack--choice-stamp left)) (tb (loki-assistant-slack--choice-stamp right))
        (na (loki-assistant-slack--choice-title left)) (nb (loki-assistant-slack--choice-title right)))
    (cond ((/= a b) (< a b))
          ((loki-assistant-slack--stamp< tb ta) t)
          ((loki-assistant-slack--stamp< ta tb) nil)
          ((not (equal na nb)) (string-lessp na nb))
          ((not (equal (loki-assistant-slack--choice-workspace left)
                       (loki-assistant-slack--choice-workspace right)))
           (string-lessp (loki-assistant-slack--choice-workspace left)
                         (loki-assistant-slack--choice-workspace right)))
          (t (string-lessp (alist-get 'id (loki-assistant-slack--choice-room left))
                           (alist-get 'id (loki-assistant-slack--choice-room right)))))))

(defun loki-assistant-slack--matches (input table pred)
  "按照分组和匹配程度返回 TABLE 中符合 INPUT 和 PRED 的候选。"
  (let* ((query (downcase (string-trim input)))
         (pattern (mapconcat (lambda (char) (regexp-quote (char-to-string char))) query ".*?"))
         matches)
    (dolist (candidate (all-completions "" table pred))
      (when-let ((choice (gethash candidate loki-assistant-slack--completion-choices)))
        (let* ((fields (loki-assistant-slack--choice-search choice))
               (exact (or (string-empty-p query) (equal candidate input) (member query fields))))
          (when (or exact (seq-some (lambda (field) (string-match-p pattern field)) fields))
            (push (list candidate (loki-assistant-slack--choice-group choice) (if exact 0 1)) matches)))))
    (mapcar #'car
            (cl-stable-sort (nreverse matches)
                            (lambda (left right)
                              (if (= (nth 1 left) (nth 1 right))
                                  (< (nth 2 left) (nth 2 right))
                                (< (nth 1 left) (nth 1 right))))))))

(defun loki-assistant-slack--try-completion (input table pred point)
  "根据 INPUT、TABLE 和 PRED 尝试补全，保留光标 POINT。"
  (let ((matches (loki-assistant-slack--matches input table pred)))
    (cond ((member input matches) t)
          ((null matches) nil)
          ((null (cdr matches)) (cons (car matches) (length (car matches))))
          (t (cons input point)))))

(defun loki-assistant-slack--all-completions (input table pred _point)
  "返回 INPUT 在 TABLE 和 PRED 下的全部匹配。"
  (when-let ((matches (loki-assistant-slack--matches input table pred)))
    (nconc matches 0)))

(defun loki-assistant-slack--avatar (url images update)
  "异步读取 URL 的头像，更新 IMAGES 并调用 UPDATE。"
  (when (and (display-graphic-p) (stringp url)
             (string-match-p "\\`https://\\(?:[a-z0-9-]+\\.\\)*\\(?:slack-edge\\.com\\|slack\\.com\\|gravatar\\.com\\)/" url)
             (eq (gethash url images 'missing) 'missing)
             (< (cl-count 'loading (hash-table-values images)) 8))
    (puthash url 'loading images)
    (let ((url-automatic-caching nil)
          (url-max-redirections 0)
          (url-request-extra-headers nil)
          (url-request-method "GET")
          (url-request-data nil))
      (condition-case nil
          (url-retrieve
           url (lambda (status)
                 (unwind-protect
                     (let* ((start (and url-http-end-of-headers (1+ url-http-end-of-headers)))
                            (data (and (not (plist-get status :error)) (eq url-http-response-status 200)
                                       start (<= 1 (- (point-max) start) (* 128 1024))
                                       (buffer-substring-no-properties start (point-max))))
                            (type (and data (image-type-from-data data))))
                       (puthash url (and (memq type '(jpeg png gif webp))
                                         (ignore-errors (create-image data type t :max-width 20 :max-height 20 :ascent 'center))) images))
                   (kill-buffer (current-buffer)))
                 (funcall update)) nil t t)
        (error (puthash url nil images)))))
  (let ((image (gethash url images)))
    (if (consp image) (propertize "○" 'display image) (propertize "○" 'face 'shadow))))

(defun loki-assistant-slack--align-to (column)
  "返回延伸至 COLUMN 的空格。"
  (propertize " " 'display `(space :align-to ,column)))

(defun loki-assistant-slack--fit-column (text width)
  "将 TEXT 缩短至 WIDTH 列。"
  (let* ((fitted (truncate-string-to-width text width nil nil "…"))
         (pixels (* width (frame-char-width))))
    (when (and (display-graphic-p) (fboundp 'string-pixel-width)
               (> (string-pixel-width fitted) pixels))
      (let ((end (length fitted)))
        (while (and (> end 0) (> (string-pixel-width (concat (substring fitted 0 end) "…")) pixels))
          (setq end (1- end)))
        (setq fitted (concat (substring fitted 0 end) "…"))))
    fitted))

(defun loki-assistant-slack--choice-row (key choice title-width workspace-width images refresh)
  "按 TITLE-WIDTH 和 WORKSPACE-WIDTH 对齐 KEY 对应的 CHOICE，使用 IMAGES 和 REFRESH 显示头像。"
  (let* ((kind (alist-get 'kind (loki-assistant-slack--choice-room choice)))
         (avatars (loki-assistant-slack--choice-avatars choice))
         (title-column 19)
         (workspace-column (+ title-column title-width))
         (status-column (+ workspace-column workspace-width)))
    (list key
          (concat (cond ((equal kind "im") "私聊")
                        ((equal kind "mpim") "群组私聊")
                        (t "主题聊天"))
                  (loki-assistant-slack--align-to 10)
                  (mapconcat (lambda (url) (loki-assistant-slack--avatar url images refresh))
                             (seq-take avatars 2) "")
                  (when (cddr avatars) (propertize "…" 'face 'shadow))
                  (loki-assistant-slack--align-to title-column))
          (concat (when (> workspace-width 0)
                    (concat (loki-assistant-slack--align-to workspace-column)
                            (loki-assistant-slack--fit-column
                             (loki-assistant-slack--choice-workspace choice) (- workspace-width 2))))
                  (loki-assistant-slack--align-to status-column)
                  (loki-assistant-slack--choice-status choice)
                  (loki-assistant-slack--align-to (+ status-column 15))
                  (loki-assistant-slack--choice-summary choice)))))

(defun loki-assistant-slack--choose (window selection rooms &optional status)
  "在 WINDOW 选择当前请求 SELECTION 得到的 ROOMS。"
  (when (and (eq selection loki-assistant-slack--selection) (window-live-p window))
    (cond
     ((active-minibuffer-window) (message "请退出当前 minibuffer 后重新选择 Slack 聊天。"))
     ((null rooms) (message "%s" (or status "没有可用的 Slack 聊天。")))
     (t
      (let* ((sink (consult--async-sink))
             (images (make-hash-table :test #'equal))
             (show-workspace (> (length (delete-dups (mapcar (lambda (room) (alist-get 'team_id room)) rooms))) 1))
             (window-width (window-body-width (minibuffer-window (window-frame window))))
             (title-width (min 34 (max 10 (/ window-width 4))))
             (workspace-width (if show-workspace (min 17 (max 8 (/ window-width 7))) 0))
             (loki-assistant-slack--completion-choices (make-hash-table :test #'equal))
             (completion-styles-alist (cons '(loki-slack loki-assistant-slack--try-completion
                                                        loki-assistant-slack--all-completions "Slack 模糊匹配。") completion-styles-alist))
             (completion-category-overrides (cons '(loki-slack (styles loki-slack)) completion-category-overrides))
             (completion-ignore-case t)
             (choices-table loki-assistant-slack--completion-choices)
             (refresh (lambda () (funcall sink 'refresh)))
             (items (cl-loop for room in rooms for index from 0
                             for choice = (loki-assistant-slack--choice-for room)
                             for title = (loki-assistant-slack--choice-title choice)
                             for key = (consult--tofu-append
                                        (loki-assistant-slack--fit-column title (- title-width 2)) index)
                             do (puthash key choice choices-table) collect key))
             minibuffer room)
        (funcall sink (sort items (lambda (left right)
                                    (loki-assistant-slack--choice< (gethash left choices-table)
                                                                  (gethash right choices-table)))))
        (setq room
              (with-selected-window window
                (consult--read
                 (lambda (action)
                   (cond
                    ((eq action 'setup) (setq minibuffer (current-buffer)) (funcall sink action))
                    ((stringp action)
                     (when (eq (current-buffer) minibuffer)
                       (cond ((not (loki-assistant-slack--matches action minibuffer-completion-table nil))
                              (minibuffer-message "未找到匹配的聊天"))
                             (status (minibuffer-message "%s" status)))))
                    (t (funcall sink action))))
                 :prompt "Slack 聊天：" :category 'loki-slack :async-wrap nil :sort nil :require-match t
                 :lookup (lambda (key &rest _)
                           (when-let ((choice (gethash key choices-table))) (loki-assistant-slack--choice-room choice)))
                 :group (lambda (key transform)
                          (if transform key
                            (aref ["未读 @我" "其他未读" "其余聊天"]
                                  (loki-assistant-slack--choice-group (gethash key choices-table)))))
                 :annotate (lambda (key)
                             (loki-assistant-slack--choice-row
                              key (gethash key choices-table)
                              title-width workspace-width images refresh)))))
        (when room (loki-assistant-slack--open room)))))))

;;;###autoload
(defun loki-assistant-slack-select-chat ()
  "通过 Consult 选择 Slack 聊天。"
  (interactive)
  (unless (require 'consult nil t) (user-error "Slack 聊天需要 Consult"))
  (unless (require 'shell-maker nil t) (user-error "Slack 聊天需要 shell-maker"))
  (let ((window (selected-window))
        (selection (setq loki-assistant-slack--selection (list nil))))
    (loki-assistant-slack--request
     "GET" "/slack/conversations/picker"
     (lambda (response)
       (when (eq selection loki-assistant-slack--selection)
         (if (not (plist-get response :ok))
             (message "%s" (or (plist-get response :error-message) "Slack 聊天列表暂不可用"))
           (let* ((data (plist-get response :data))
                  (states (seq-map (lambda (workspace) (alist-get 'state workspace))
                                   (alist-get 'workspaces data)))
                  (status (cond ((member "warming_up" states) "正在准备 Slack 聊天列表")
                                ((member "unavailable" states) "部分 Slack 工作区暂不可用"))))
             (run-at-time 0 nil #'loki-assistant-slack--choose
                          window selection (append (alist-get 'items data) nil) status))))))))

(defun loki-assistant-slack--open (room &optional defer-refresh)
  "在独立窗口中显示 ROOM。DEFER-REFRESH 非空时延后刷新。"
  (let* ((id (alist-get 'id room))
         (session (alist-get 'session_id room))
         (name (format "*Slack %s*" id))
         (buffer (if (comint-check-proc name) (get-buffer name)
                   (shell-maker-start-v2
                    :config (make-shell-maker-config
                             :name "loki-slack" :prompt "消息> " :prompt-regexp "^消息> ")
                    :no-focus t :buffer-name name :alias-commands nil))))
    (with-current-buffer buffer
      (unless loki-assistant-slack--history-end
        (setq loki-assistant-slack--history-end (copy-marker (point-min)))
        (loki-assistant-slack-input-mode 1)
        (add-hook 'kill-buffer-hook #'loki-assistant-slack--stop-refresh nil t))
      (unless (and loki-assistant-slack--timeline
                   (equal session (alist-get 'session_id loki-assistant-slack--room)))
        (setq loki-assistant-slack--timeline (make-loki-assistant-slack--page)
              loki-assistant-slack--threads (make-hash-table :test #'equal)
              loki-assistant-slack--reply nil
              loki-assistant-slack--send-request nil))
      (setq loki-assistant-slack--room room))
    (let* ((existing (get-buffer-window buffer t))
           (window (or existing
                      (display-buffer buffer
                                      '((display-buffer-pop-up-window display-buffer-pop-up-frame)
                                        (inhibit-same-window . t))))))
      (unless (window-live-p window) (user-error "当前无法打开 Slack 聊天窗口"))
      (set-window-dedicated-p window t)
      (select-frame-set-input-focus (window-frame window))
      (select-window window)
      (with-current-buffer buffer
        (unless (timerp loki-assistant-slack--refresh-timer)
          (setq loki-assistant-slack--refresh-timer
                (run-at-time 15 15 #'loki-assistant-slack--refresh-visible buffer)))
        (loki-assistant-slack--render)
        (unless existing (goto-char (point-max)))
        (unless defer-refresh (loki-assistant-slack-refresh)))
      buffer)))

(defun loki-assistant-slack--message-position (ts)
  "返回消息 TS 在聊天窗口中的位置。"
  (when ts
    (save-excursion
      (goto-char (point-min))
      (when-let ((match (text-property-search-forward 'loki-slack-message ts t)))
        (prop-match-beginning match)))))

(defun loki-assistant-slack-open-notification (data)
  "打开 services 提供的通知目标 DATA，返回实际打开的层级。"
  (unless (require 'shell-maker nil t) (user-error "Slack 聊天需要 shell-maker"))
  (let* ((target (json-parse-string data :object-type 'alist :null-object nil))
         (room (alist-get 'room target))
         (root (alist-get 'root target))
         (reply (alist-get 'reply target))
         (thread (alist-get 'thread_ts target))
         (message (alist-get 'message_ts target))
         (frame (or (and (display-graphic-p) (selected-frame))
                    (seq-find #'display-graphic-p (frame-list))
                    (make-frame '((window-system . ns)))))
         result)
    (select-frame frame)
    (with-current-buffer (loki-assistant-slack--open room t)
      (when root
        (setf (loki-assistant-slack--page-items loki-assistant-slack--timeline)
              (loki-assistant-slack--merge
               (loki-assistant-slack--page-items loki-assistant-slack--timeline)
               `((items . (,root))))))
      (when thread
        (let ((page (loki-assistant-slack--thread thread)))
          (setf (loki-assistant-slack--page-expanded page) t)
          (when reply
            (setf (loki-assistant-slack--page-items page)
                  (loki-assistant-slack--merge (loki-assistant-slack--page-items page)
                                               `((items . (,reply))))))))
      (loki-assistant-slack--render)
      (let ((message-position (loki-assistant-slack--message-position message))
            (thread-position (loki-assistant-slack--message-position thread)))
        (when-let ((position (or message-position thread-position)))
          (goto-char position)
          (recenter))
        (setq result (cond (message-position 'message) (thread-position 'thread) (t 'conversation))))
      (loki-assistant-slack-refresh))
    result))

(defun loki-assistant-slack--stop-refresh ()
  "清理当前会话的定时刷新。"
  (when (timerp loki-assistant-slack--refresh-timer)
    (cancel-timer loki-assistant-slack--refresh-timer)
    (setq loki-assistant-slack--refresh-timer nil)))

(defun loki-assistant-slack--refresh-visible (buffer)
  "仅在 BUFFER 可见且连接可用时刷新。"
  (when (and (buffer-live-p buffer) (get-buffer-window buffer t)
             (comint-check-proc buffer))
    (with-current-buffer buffer (loki-assistant-slack-refresh))))

(defun loki-assistant-slack--thread (root)
  "取得或建立根消息 ROOT 的线程状态。"
  (or (gethash root loki-assistant-slack--threads)
      (puthash root (make-loki-assistant-slack--page) loki-assistant-slack--threads)))

(defun loki-assistant-slack--non-null (value)
  "把 JSON 空值 VALUE 转成 nil。"
  (unless (eq value :null) value))

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

(defun loki-assistant-slack--uuid ()
  "为一次主动提交生成 UUID。"
  (let ((hash (secure-hash 'sha256 (format "%S:%S:%S:%S" (current-time)
                                          (random) (emacs-pid) (current-buffer)))))
    (format "%s-%s-4%s-8%s-%s" (substring hash 0 8) (substring hash 8 12)
            (substring hash 13 16) (substring hash 17 20) (substring hash 20 32))))

(defun loki-assistant-slack--load (page thread &optional older)
  "在 PAGE 中加载 THREAD 的消息。OLDER 表示读取下一页。"
  (unless (loki-assistant-slack--page-loading page)
    (let ((buffer (current-buffer))
          (room loki-assistant-slack--room)
          (cursor (and older (loki-assistant-slack--page-cursor page))))
      (setf (loki-assistant-slack--page-loading page) t)
      (loki-assistant-slack--request
       "GET" (loki-assistant-slack--path room thread)
       (lambda (response)
         (when (buffer-live-p buffer)
           (with-current-buffer buffer
             (when (eq page (if thread (gethash thread loki-assistant-slack--threads)
                              loki-assistant-slack--timeline))
               (setf (loki-assistant-slack--page-loading page) nil)
               (let ((before (copy-loki-assistant-slack--page page)))
                 (if (plist-get response :ok)
                     (let ((data (plist-get response :data)))
                       (when (or older (not (loki-assistant-slack--page-loaded page)))
                         (let ((next (alist-get 'next_cursor data)))
                           (setf (loki-assistant-slack--page-cursor page) (and (stringp next) next))))
                       (setf (loki-assistant-slack--page-loaded page) t
                             (loki-assistant-slack--page-error page) nil
                             (loki-assistant-slack--page-items page)
                             (loki-assistant-slack--merge (loki-assistant-slack--page-items page) data)))
                   (setf (loki-assistant-slack--page-error page)
                         (or (plist-get response :error-message) "Slack 消息读取失败")))
                 (unless (equal before page) (loki-assistant-slack--render)))))))
       :room room :params (when cursor (list (cons 'cursor cursor)))))))

(defun loki-assistant-slack-refresh ()
  "刷新主会话和已展开的线程。"
  (interactive)
  (loki-assistant-slack--load loki-assistant-slack--timeline nil)
  (maphash (lambda (thread page)
             (when (loki-assistant-slack--page-expanded page)
               (loki-assistant-slack--load page thread)))
           loki-assistant-slack--threads))

(defun loki-assistant-slack--toggle (root)
  "在主会话中展开或折叠 ROOT 的线程。"
  (let ((page (loki-assistant-slack--thread root)))
    (setf (loki-assistant-slack--page-expanded page)
          (not (loki-assistant-slack--page-expanded page)))
    (loki-assistant-slack--render)
    (when (loki-assistant-slack--page-expanded page)
      (loki-assistant-slack--load page root))))

(defun loki-assistant-slack--draft-start ()
  "返回当前会话输入的开始位置。"
  (if-let ((process (get-buffer-process (current-buffer))))
      (marker-position (process-mark process))
    (user-error "请重新打开此 Slack 会话")))

(defun loki-assistant-slack--target (root)
  "将输入目标切换到 ROOT 线程或主会话。"
  (when (and (not (equal root loki-assistant-slack--reply))
             (< (loki-assistant-slack--draft-start) (point-max)))
    (user-error "请先发送或清空当前草稿"))
  (setq loki-assistant-slack--reply root)
  (when root
    (let ((page (loki-assistant-slack--thread root)))
      (setf (loki-assistant-slack--page-expanded page) t)
      (loki-assistant-slack--load page root)))
  (loki-assistant-slack--render)
  (goto-char (point-max)))

(defun loki-assistant-slack--main-target ()
  "切换到主会话输入。"
  (interactive)
  (loki-assistant-slack--target nil))

(defun loki-assistant-slack--message (message)
  "插入消息 MESSAGE。"
  (let ((start (point))
        (ts (alist-get 'ts message))
        (author (alist-get 'name (loki-assistant-slack--non-null (alist-get 'author message)))))
    (insert (propertize (or author "Slack") 'face 'bold)
            (if (stringp ts) (format-time-string "  %H:%M\n" (seconds-to-time (string-to-number ts))) "\n")
            (or (alist-get 'text message) "") "\n")
    (when (stringp ts)
      (put-text-property start (point) 'loki-slack-message ts))))

(defun loki-assistant-slack--pagination (page thread)
  "为 PAGE 插入错误或翻页按钮。"
  (when-let ((error (loki-assistant-slack--page-error page)))
    (insert (propertize (concat error "\n") 'face 'error)))
  (when (loki-assistant-slack--page-cursor page)
    (insert-text-button (if thread "更早的回复" "更早的消息") 'follow-link t
                        'action (lambda (_) (loki-assistant-slack--load page thread t)))
    (insert "\n\n")))

(defun loki-assistant-slack--history (timeline threads)
  "渲染 TIMELINE 及 THREADS 中已经展开的线程。"
  (loki-assistant-slack--pagination timeline nil)
  (dolist (message (loki-assistant-slack--page-items timeline))
    (let* ((start (point))
           (root (alist-get 'ts message))
           (count (or (alist-get 'reply_count message) 0))
           (page (gethash root threads))
           (expanded (and page (loki-assistant-slack--page-expanded page))))
      (loki-assistant-slack--message message)
      (when (or expanded (> count 0))
        (insert-text-button (if expanded "折叠回复" (format "展开 %d 条回复" count))
                            'follow-link t
                            'action (lambda (_) (loki-assistant-slack--toggle root)))
        (insert "  "))
      (insert-text-button "回复" 'follow-link t
                          'action (lambda (_) (loki-assistant-slack--target root)))
      (insert "\n")
      (when expanded
        (let ((start (point)))
          (loki-assistant-slack--pagination page root)
          (dolist (reply (loki-assistant-slack--page-items page))
            (loki-assistant-slack--message reply))
          (when (and (not (loki-assistant-slack--page-loaded page))
                     (not (loki-assistant-slack--page-error page)))
            (insert "正在读取回复…\n"))
          (add-text-properties start (point) '(line-prefix "    " wrap-prefix "    "))))
      (insert "\n")
      (put-text-property start (point) 'loki-slack-root root))))

(defun loki-assistant-slack--render ()
  "更新提示符上方的历史，保留当前输入和编辑位置。"
  (let* ((timeline loki-assistant-slack--timeline)
         (threads loki-assistant-slack--threads)
         (message (get-text-property (point) 'loki-slack-message))
         (root (get-text-property (point) 'loki-slack-root))
         (history (with-temp-buffer (loki-assistant-slack--history timeline threads) (buffer-string)))
         (modified (buffer-modified-p))
         (inhibit-read-only t)
         (buffer-undo-list t))
    (setq header-line-format
          (replace-regexp-in-string
           "%" "%%"
           (concat " " (loki-assistant-slack--label loki-assistant-slack--room)
                   (when loki-assistant-slack--reply
                     (format " · 回复线程 %s · C-c C-k 切回会话" loki-assistant-slack--reply)))))
    (save-excursion
      (delete-region (point-min) loki-assistant-slack--history-end)
      (goto-char (point-min))
      (insert history)
      (add-text-properties (point-min) (point) '(read-only t rear-nonsticky t))
      (set-marker loki-assistant-slack--history-end (point)))
    (when-let ((process (get-buffer-process (current-buffer))))
      (let ((start (marker-position loki-assistant-slack--history-end))
            (end (marker-position (process-mark process))))
        (with-silent-modifications
          (if loki-assistant-slack--reply
              (put-text-property start end 'display
                                 (propertize "线程回复> " 'face 'comint-highlight-prompt))
            (remove-text-properties start end '(display nil))))))
    (if-let ((position (loki-assistant-slack--message-position message)))
        (goto-char position)
      (when root
      (let ((anchor (save-excursion
                      (goto-char (point-min))
                      (text-property-search-forward 'loki-slack-root root t))))
          (when anchor (goto-char (prop-match-beginning anchor))))))
    (restore-buffer-modified-p modified)))

(defun loki-assistant-slack--submit ()
  "激活历史中的按钮，或发送当前输入。"
  (interactive)
  (if (button-at (point))
      (push-button)
    (when (< (point) (loki-assistant-slack--draft-start))
      (user-error "请在消息输入区发送"))
    (when (plist-get loki-assistant-slack--send-request :busy)
      (user-error "正在等待 Slack 发送结果"))
    (let* ((text (buffer-substring-no-properties (loki-assistant-slack--draft-start) (point-max)))
           (previous loki-assistant-slack--send-request)
           (room loki-assistant-slack--room)
           (thread loki-assistant-slack--reply)
           (same-draft (and (equal text (plist-get previous :text))
                            (equal thread (plist-get previous :thread)))))
      (when (string-empty-p (string-trim text)) (user-error "请输入 Slack 消息"))
      (setq loki-assistant-slack--send-request
            (list :id (if same-draft (plist-get previous :id) (loki-assistant-slack--uuid))
                  :text text :thread thread :busy t))
      (let ((request loki-assistant-slack--send-request) (buffer (current-buffer)))
        (message "正在发送 Slack 消息…")
        (loki-assistant-slack--request
         "POST" (loki-assistant-slack--path room thread)
         (lambda (response) (loki-assistant-slack--sent buffer room request response 0))
         :room room :body (list (cons 'text text)) :key (plist-get request :id))))))

(defun loki-assistant-slack--sent (buffer room request response polls)
  "处理 BUFFER 中 REQUEST 的发送响应。"
  (when (buffer-live-p buffer)
    (with-current-buffer buffer
      (when (and (comint-check-proc buffer) (eq request loki-assistant-slack--send-request))
        (let* ((ok (plist-get response :ok))
               (data (plist-get response :data))
               (state (alist-get 'state data)))
          (cond
           ((and ok (equal state "sent"))
            (setq loki-assistant-slack--send-request nil)
            (let ((start (loki-assistant-slack--draft-start)))
              (when (and (equal loki-assistant-slack--reply (plist-get request :thread))
                         (equal (buffer-substring-no-properties start (point-max))
                                (plist-get request :text)))
                (delete-region start (point-max))))
            (loki-assistant-slack-refresh)
            (message "Slack 消息已发送"))
           ((and ok (equal state "pending") (< polls 20))
            (run-at-time
             1 nil
             (lambda ()
               (when (and (buffer-live-p buffer)
                          (eq request (buffer-local-value 'loki-assistant-slack--send-request buffer)))
                 (loki-assistant-slack--request
                  "GET" (concat "/slack/send-requests/" (plist-get request :id))
                  (lambda (result) (loki-assistant-slack--sent buffer room request result (1+ polls)))
                  :room room :params (list (cons 'team_id (alist-get 'team_id room))))))))
           (t
            (setf (plist-get request :busy) nil)
            (when (or (equal state "failed")
                      (and (not ok) (zerop polls)
                           (integerp (plist-get response :status))
                           (<= 400 (plist-get response :status) 499)))
              (setq loki-assistant-slack--send-request nil))
            (message "%s" (or (alist-get 'message (alist-get 'error data))
                              (plist-get response :error-message)
                              "发送结果待确认，草稿已保留。")))))))))

(defvar loki-assistant-slack-input-mode-map
  (let ((map (make-sparse-keymap)))
    (define-key map (kbd "RET") #'loki-assistant-slack--submit)
    (define-key map (kbd "<return>") #'loki-assistant-slack--submit)
    (define-key map (kbd "S-<return>") #'newline)
    (define-key map (kbd "C-c C-r") #'loki-assistant-slack-refresh)
    (define-key map (kbd "C-c C-k") #'loki-assistant-slack--main-target)
    map))

(define-minor-mode loki-assistant-slack-input-mode
  "在 shell-maker 缓冲区中编辑和发送 Slack 消息。"
  :keymap loki-assistant-slack-input-mode-map)

(provide 'loki-assistant-slack-chat)
;;; loki-assistant-slack-chat.el ends here
