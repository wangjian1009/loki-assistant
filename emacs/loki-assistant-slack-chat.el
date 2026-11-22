;;; loki-assistant-slack-chat.el --- Slack 聊天时间线 -*- lexical-binding: t; -*-

;;; Code:

(require 'button)
(require 'cl-lib)
(require 'comint)
(require 'seq)
(require 'time-date)
(require 'url)
(require 'loki-assistant)

(declare-function consult--read "consult")
(declare-function consult--tofu-append "consult")
(declare-function consult--async-sink "consult")
(declare-function shell-maker-start-v2 "shell-maker")
(declare-function make-shell-maker-config "shell-maker")
(declare-function agent-shell-ui-make-fragment-model "agent-shell-ui")
(declare-function agent-shell-ui-update-fragment "agent-shell-ui")
(declare-function agent-shell-markdown-replace-markup "agent-shell-markdown")
(declare-function agent-shell-markdown--linkify-file-references "agent-shell-markdown")
(declare-function emojify-create-emojify-emojis "emojify")
(declare-function emojify-get-emoji "emojify")
(defvar agent-shell-markdown-render-functions)
(defvar shell-maker-prompt-before-killing-buffer)

(defconst loki-assistant-slack--prompt "消息> ")

(defconst loki-assistant-slack--entity-regexp "&\\(?:amp\\|lt\\|gt\\);")

(defconst loki-assistant-slack--markup-regexp
  (concat "```\\|`[^`\n]*`\\|<\\([^<>\n]+\\)>\\|\\(" loki-assistant-slack--entity-regexp "\\)"
          "\\|\\(?:https?\\|slack\\)://[^<[:space:]]+\\|\\(:[[:alnum:]_+-]+:\\)"))

(defface loki-assistant-slack-self-label
  '((t :inherit (bold font-lock-keyword-face) :inverse-video t :box t))
  "本人消息的身份色块。"
  :group 'loki-assistant)

(defface loki-assistant-slack-sender-label
  '((t :inherit (bold font-lock-function-name-face) :inverse-video t :box t))
  "其他发送者的身份色块。"
  :group 'loki-assistant)

(defface loki-assistant-slack-action
  '((t :inherit link :underline nil))
  "聊天链接和操作入口的样式。"
  :group 'loki-assistant)

(defface loki-assistant-slack-message-time
  '((((class color) (background light)) :foreground "#4b4b4b" :background "#e4e4e4" :height 0.85)
    (((class color) (background dark)) :foreground "#e1e1e1" :background "#555555" :height 0.85)
    (t :inverse-video t))
  "消息末尾的发送时间色块。"
  :group 'loki-assistant)

;; 保留槽位顺序，以兼容直接重载时的分页状态。
(cl-defstruct loki-assistant-slack--page items cursor loading loaded expanded error)
(cl-defstruct loki-assistant-slack--choice room group stamp mention title search avatars status sender time preview)
(cl-defstruct loki-assistant-slack--subscription id room self-id (sequence 0) timeout)
(cl-defstruct loki-assistant-slack--read-position acknowledged pending retry-at)

(defvar-local loki-assistant-slack--room nil)
(defvar-local loki-assistant-slack--display-name nil)
(defvar-local loki-assistant-slack--self-id nil)
(defvar-local loki-assistant-slack--timeline nil)
(defvar-local loki-assistant-slack--threads nil)
(defvar-local loki-assistant-slack--thread-buffers nil)
(defvar-local loki-assistant-slack--parent-buffer nil)
(defvar-local loki-assistant-slack--thread-root nil)
(defvar-local loki-assistant-slack--thread-message nil)
(defvar-local loki-assistant-slack--history-end nil)
(defvar-local loki-assistant-slack--reply nil
  "当前输入区的线程回复目标。")
(defvar-local loki-assistant-slack--send-request nil)
(defvar-local loki-assistant-slack--send-timer nil)
(defvar-local loki-assistant-slack--subscription nil)
(defvar-local loki-assistant-slack--pending-subscription nil)
(defvar-local loki-assistant-slack--page-changes nil)
(defvar-local loki-assistant-slack--notification-root nil)
(defvar-local loki-assistant-slack--read-positions nil)
(defvar-local loki-assistant-slack--read-timer nil)
(defvar loki-assistant-slack--subscription-buffers (make-hash-table :test #'equal))
(defvar loki-assistant-slack--refresh-timer nil)
(defvar-local loki-assistant-slack--profiles nil)
(defvar loki-assistant-slack--selection nil)
(defvar loki-assistant-slack--completion-choices nil)

(cl-defun loki-assistant-slack--request (method path callback &key room params body key)
  "通过 services 发起 Slack 请求。"
  (loki-assistant-request
   method path :params params :body body :timeout 20
   :headers (append (when-let ((session (alist-get 'session_id room)))
                      (list (cons "X-Loki-Slack-Session" session)))
                    (when key (list (cons "Idempotency-Key" key))))
   :callback callback))

(defun loki-assistant-slack--path (room thread &optional endpoint)
  "构造 ROOM 的 ENDPOINT 路径。THREAD 非空时指向线程。"
  (concat "/slack/conversations/" (url-hexify-string (alist-get 'id room))
          (when thread (concat "/threads/" (url-hexify-string thread)))
          "/" (or endpoint "messages")))

(defun loki-assistant-slack--label (room)
  "生成 ROOM 的显示名称。"
  (concat (when (member (alist-get 'kind room) '("public_channel" "private_channel")) "#")
          (replace-regexp-in-string "[\n\r]+" " " (or (alist-get 'title room) (alist-get 'id room)))))

(defun loki-assistant-slack--single-line (text)
  "返回压成一行的 TEXT。"
  (when (stringp text) (string-trim (replace-regexp-in-string "[[:space:][:cntrl:]]+" " " text))))

(defun loki-assistant-slack--local-file (path)
  "若 PATH 指向可读的本机缓存文件，则返回 PATH。"
  (when (and (stringp path) (file-name-absolute-p path) (not (file-remote-p path))
             (file-regular-p path) (file-readable-p path))
    path))

(defun loki-assistant-slack--cached-record (entry key expected fields)
  "用 ENTRY 对应的缓存记录更新 FIELDS，核对 KEY 等于 EXPECTED。"
  (condition-case nil
      (if-let ((path (loki-assistant-slack--local-file (alist-get 'cache_path entry))))
          (with-temp-buffer
            (let ((coding-system-for-read 'utf-8-unix)
                  (read-circle nil))
              (insert-file-contents path)
              (goto-char (point-min))
              (let ((record (read (current-buffer))))
                (skip-chars-forward " \t\r\n")
                (if (and (eobp) (equal (alist-get key record) expected))
                    (let ((result (copy-sequence entry)))
                      (dolist (field fields)
                        (when-let ((value (assq field record)))
                          (setq result (cons value (assq-delete-all field result)))))
                      result)
                  entry))))
        entry)
    (error entry)))

(defun loki-assistant-slack--choice-for (room opened)
  "从 ROOM 生成候选，OPENED 表示聊天 buffer 已打开。"
  (let* ((id (alist-get 'id room))
         (room (if (stringp id)
                   (loki-assistant-slack--cached-record
                    room 'id (car (last (split-string id "\\."))) '(kind))
                 room))
         (mention (loki-assistant-slack--non-null (alist-get 'last_unread_mention_ts room)))
         (unread (or (alist-get 'unread_count room) 0))
         (last-message (loki-assistant-slack--non-null (alist-get 'last_message room)))
         (activity (loki-assistant-slack--non-null (alist-get 'ts last-message)))
         (title (loki-assistant-slack--chat-title room)))
    (make-loki-assistant-slack--choice
     :room room :group (cond (opened 0) ((or mention (> unread 0)) 1) (t 2))
     :mention (and (not opened) mention)
     :stamp (or (and (not opened) mention)
                (loki-assistant-slack--non-null (alist-get 'last_activity_ts room)) activity "0.0")
     :title title
     :search (nconc (delq nil (list (downcase title)
                                   (downcase (loki-assistant-slack--label room))
                                   (when-let ((team (loki-assistant-slack--single-line (alist-get 'team_name room))))
                                     (downcase team))))
                    (append (alist-get 'search_terms room) nil))
     :avatars (append (alist-get 'participants room) nil)
     :status (concat (when (> unread 0) (format "%d 未读" unread))
                     (when mention (propertize " @我" 'face 'warning)))
     :sender (or (loki-assistant-slack--single-line (alist-get 'author_name last-message)) "")
     :time (if activity (format-time-string "%m-%d %H:%M" (seconds-to-time (string-to-number activity))) "")
     :preview (if activity
                  (or (loki-assistant-slack--single-line (alist-get 'preview last-message)) "")
                (if (eq (alist-get 'history_loaded room) :false) "消息摘要暂不可用" "暂无消息")))))

(defun loki-assistant-slack--choice< (left right)
  "按默认顺序比较候选 LEFT 和 RIGHT。"
  (let ((a (loki-assistant-slack--choice-group left)) (b (loki-assistant-slack--choice-group right))
        (ma (and (loki-assistant-slack--choice-mention left) t))
        (mb (and (loki-assistant-slack--choice-mention right) t))
        (ta (loki-assistant-slack--choice-stamp left)) (tb (loki-assistant-slack--choice-stamp right))
        (na (loki-assistant-slack--choice-title left)) (nb (loki-assistant-slack--choice-title right)))
    (cond ((/= a b) (< a b))
          ((not (eq ma mb)) ma)
          ((loki-assistant-slack--stamp< tb ta) t)
          ((loki-assistant-slack--stamp< ta tb) nil)
          ((not (equal na nb)) (string-lessp na nb))
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

(defun loki-assistant-slack--file-image (path width height)
  "以 WIDTH 和 HEIGHT 为上限读取 PATH 中的图片。"
  (when (display-graphic-p)
    (when-let ((path (loki-assistant-slack--local-file path)))
      (ignore-errors (create-image path nil nil :max-width width :max-height height :ascent 'center)))))

(defun loki-assistant-slack--profile-avatar (profile)
  "显示 PROFILE 已缓存的本机头像。"
  (when-let ((image (loki-assistant-slack--file-image (alist-get 'avatar_path profile) 20 20)))
    (propertize "○" 'display image)))

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

(defun loki-assistant-slack--choice-row (key choice title-width sender-width window-width)
  "按当前窗口宽度对齐 KEY 对应的 CHOICE 各列。"
  (let* ((avatars (loki-assistant-slack--choice-avatars choice))
         (title-column 7)
         (status-column (+ title-column title-width))
         (sender-column (+ status-column 14))
         (time-column (+ sender-column sender-width))
         (preview-column (+ time-column 14)))
    (list key
          (concat (mapconcat #'loki-assistant-slack--profile-avatar (seq-take avatars 2) "")
                  (loki-assistant-slack--align-to title-column))
          (concat (loki-assistant-slack--align-to status-column)
                  (loki-assistant-slack--fit-column (loki-assistant-slack--choice-status choice) 12)
                  (loki-assistant-slack--align-to sender-column)
                  (propertize (loki-assistant-slack--fit-column (loki-assistant-slack--choice-sender choice)
                                                               (- sender-width 2)) 'face 'shadow)
                  (loki-assistant-slack--align-to time-column)
                  (propertize (loki-assistant-slack--choice-time choice) 'face 'shadow)
                  (loki-assistant-slack--align-to preview-column)
                  (propertize (loki-assistant-slack--fit-column (loki-assistant-slack--choice-preview choice)
                                                               (max 4 (- window-width preview-column 2))) 'face 'shadow)))))

(defun loki-assistant-slack--open-rooms ()
  "按复合会话 ID 收集仍存活的聊天资料。"
  (let ((rooms (make-hash-table :test #'equal)))
    (dolist (buffer (buffer-list) rooms)
      (when (loki-assistant-slack--chat-buffer-p buffer)
        (let ((room (buffer-local-value 'loki-assistant-slack--room buffer)))
          (puthash (alist-get 'id room) room rooms))))))

(defun loki-assistant-slack--choose (window selection rooms &optional status)
  "在 WINDOW 选择当前请求 SELECTION 得到的 ROOMS。"
  (when (and (eq selection loki-assistant-slack--selection) (window-live-p window))
    (let* ((opened (loki-assistant-slack--open-rooms))
           (listed (mapcar (lambda (room) (alist-get 'id room)) rooms)))
      (maphash (lambda (id room) (unless (member id listed) (push room rooms))) opened)
      (cond
       ((active-minibuffer-window) (message "请退出当前 minibuffer 后重新选择 Slack 聊天。"))
       ((null rooms) (message "%s" (or status "没有可用的 Slack 聊天。")))
       (t
        (let* ((sink (consult--async-sink))
               (window-width (window-body-width (minibuffer-window (window-frame window))))
               (title-width (min 38 (max 14 (/ window-width 4))))
               (sender-width (min 18 (max 8 (/ window-width 8))))
               (loki-assistant-slack--completion-choices (make-hash-table :test #'equal))
               (completion-styles-alist (cons '(loki-slack loki-assistant-slack--try-completion
                                                           loki-assistant-slack--all-completions "Slack 模糊匹配。") completion-styles-alist))
               (completion-category-overrides (cons '(loki-slack (styles loki-slack)) completion-category-overrides))
               (completion-ignore-case t)
               (choices-table loki-assistant-slack--completion-choices)
               (items (cl-loop for room in rooms for index from 0
                               for choice = (loki-assistant-slack--choice-for room (gethash (alist-get 'id room) opened))
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
                              (aref ["已打开" "未读" "其他"]
                                    (loki-assistant-slack--choice-group (gethash key choices-table)))))
                   :annotate (lambda (key)
                               (loki-assistant-slack--choice-row
                                key (gethash key choices-table)
                                title-width sender-width window-width)))))
          (when room (loki-assistant-slack--open room))))))))

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

(defun loki-assistant-slack--chat-buffer-p (buffer)
  "判断 BUFFER 是否为仍存活的 Slack 主聊天。"
  (and (buffer-local-value 'loki-assistant-slack--room buffer)
       (not (buffer-local-value 'loki-assistant-slack--thread-root buffer))
       (comint-check-proc buffer)))

(defun loki-assistant-slack--find-buffer (id)
  "根据复合会话 ID 查找现有聊天 buffer。"
  (seq-find (lambda (buffer)
              (and (loki-assistant-slack--chat-buffer-p buffer)
                   (equal id (alist-get 'id (buffer-local-value 'loki-assistant-slack--room buffer)))))
            (buffer-list)))

(defun loki-assistant-slack--chat-title (room)
  "根据 ROOM 的工作区和聊天名称生成标题。"
  (let ((team (or (loki-assistant-slack--single-line (alist-get 'team_name room))
                  (loki-assistant-slack--single-line (alist-get 'team_id room)))))
    (concat (if (or (null team) (string-empty-p team))
                (car (split-string (alist-get 'id room) "[.]"))
              team)
            " · " (loki-assistant-slack--label room))))

(defun loki-assistant-slack--update-title ()
  "更新当前 Slack buffer 的名字及窗口标题。"
  (let* ((thread loki-assistant-slack--thread-root)
         (title (if thread
                    (loki-assistant-slack--thread-title loki-assistant-slack--room
                                                        loki-assistant-slack--thread-message)
                  (loki-assistant-slack--chat-title loki-assistant-slack--room)))
         (name (format "*Slack %s*" title))
         (escaped (replace-regexp-in-string "%" "%%" title t t)))
    (unless (equal name loki-assistant-slack--display-name)
      (unless (equal name (buffer-name)) (rename-buffer name t))
      (setq loki-assistant-slack--display-name name))
    (setq header-line-format (concat " " escaped))
    (setq-local frame-title-format escaped)))

(defun loki-assistant-slack--thread-title (room message)
  "将根消息 MESSAGE 的摘要追加到 ROOM 的聊天标题。"
  (let* ((body (or (loki-assistant-slack--non-null (alist-get 'text message)) ""))
         (body (if (string-blank-p body)
                   (seq-some (lambda (block)
                               (alist-get 'text (car (loki-assistant-slack--section-texts block))))
                             (alist-get 'blocks message))
                 body))
         (text (when body
                 (loki-assistant-slack--single-line
                  (replace-regexp-in-string "<[^<>|]+|\\([^<>]+\\)>" "\\1" body)))))
    (concat (loki-assistant-slack--chat-title room) " · "
            (if (and text (not (string-empty-p text)))
                (truncate-string-to-width text 48 nil nil "...")
              "线程"))))

(defun loki-assistant-slack--make-chat-buffer (name)
  "以 NAME 创建聊天共用的 shell-maker buffer。"
  (shell-maker-start-v2
   :config (make-shell-maker-config
            :name "loki-slack" :prompt loki-assistant-slack--prompt
            :prompt-regexp (concat "^" (regexp-quote loki-assistant-slack--prompt)))
   :no-focus t :buffer-name (generate-new-buffer-name name) :alias-commands nil))

(defun loki-assistant-slack--initialize-input ()
  "初始化聊天输入和清理钩子。"
  (setq-local shell-maker-prompt-before-killing-buffer nil)
  (unless loki-assistant-slack--history-end
    (setq loki-assistant-slack--history-end (copy-marker (loki-assistant-slack--prompt-start)))
    (loki-assistant-slack-input-mode 1)
    (add-hook 'kill-buffer-hook #'loki-assistant-slack--stop-refresh nil t)))

(defun loki-assistant-slack--open (room &optional defer-refresh)
  "在独立窗口中显示 ROOM。DEFER-REFRESH 非空时延后刷新。"
  (let* ((id (alist-get 'id room))
         (session (alist-get 'session_id room))
         (name (format "*Slack %s*" (loki-assistant-slack--chat-title room)))
         (buffer (or (loki-assistant-slack--find-buffer id)
                     (loki-assistant-slack--make-chat-buffer name))))
    (with-current-buffer buffer
      (loki-assistant-slack--initialize-input)
      (unless loki-assistant-slack--timeline
        (setq loki-assistant-slack--timeline (make-loki-assistant-slack--page)
              loki-assistant-slack--profiles (make-hash-table :test #'equal)))
      (if loki-assistant-slack--room
          (dolist (key '(team_id team_name title kind))
            (when-let ((value (loki-assistant-slack--non-null (alist-get key room))))
              (setf (alist-get key loki-assistant-slack--room) value)))
        (setq loki-assistant-slack--room (copy-tree room))))
    (let* ((existing (get-buffer-window buffer t))
           (window (or existing
                      (display-buffer buffer
                                      '((display-buffer-pop-up-window display-buffer-pop-up-frame)
                                        (inhibit-same-window . t))))))
      (unless (window-live-p window) (user-error "当前无法打开 Slack 聊天窗口"))
      (set-window-dedicated-p window t)
      (make-frame-visible (window-frame window))
      (select-frame-set-input-focus (window-frame window))
      (select-window window)
      (with-current-buffer buffer
        (loki-assistant-slack--render)
        (unless existing (goto-char (point-max)))
        (unless (or defer-refresh
                    (and loki-assistant-slack--subscription
                         (equal session (alist-get 'session_id loki-assistant-slack--room))))
          (loki-assistant-slack-refresh)))
      buffer)))

(defun loki-assistant-slack--graphical-frame ()
  "取得可显示 Slack 聊天的图形 frame。"
  (or (and (display-graphic-p (selected-frame)) (selected-frame))
      (seq-find #'display-graphic-p (frame-list))
      (make-frame '((window-system . ns)))))

(defun loki-assistant-slack-activate ()
  "激活最近使用的聊天，或在图形窗口中打开聊天选择器。"
  (let* ((buffer (seq-find #'loki-assistant-slack--chat-buffer-p (buffer-list)))
         (frame (or (when-let ((window (and buffer (get-buffer-window buffer t))))
                      (window-frame window))
                    (loki-assistant-slack--graphical-frame))))
    (make-frame-visible frame)
    (select-frame-set-input-focus frame)
    (if buffer
        (loki-assistant-slack--open (buffer-local-value 'loki-assistant-slack--room buffer))
      (loki-assistant-slack-select-chat))))

(defun loki-assistant-slack--message-position (ts)
  "返回消息 TS 在聊天窗口中的位置。"
  (when ts
    (save-excursion
      (goto-char (point-min))
      (when-let* ((match (text-property-search-forward 'loki-slack-message ts t))
                  (start (prop-match-beginning match))
                  ((not (invisible-p start))))
        start))))

(defun loki-assistant-slack-open-notification (data)
  "打开 services 提供的通知目标 DATA，返回实际打开的层级。"
  (unless (require 'shell-maker nil t) (user-error "Slack 聊天需要 shell-maker"))
  (let* ((target (json-parse-string data :object-type 'alist :null-object nil))
         (room (alist-get 'room target))
         (root (alist-get 'root target))
         (reply (alist-get 'reply target))
         (thread (alist-get 'thread_ts target))
         (message (alist-get 'message_ts target))
         (frame (loki-assistant-slack--graphical-frame))
         page result)
    (select-frame frame)
    (with-current-buffer (loki-assistant-slack--open room t)
      (when root
        (unless loki-assistant-slack--subscription
          (setq loki-assistant-slack--notification-root root))
        (setf (loki-assistant-slack--page-items loki-assistant-slack--timeline)
              (loki-assistant-slack--merge
               (loki-assistant-slack--page-items loki-assistant-slack--timeline)
               `((items . (,root))))))
      (when (and thread (loki-assistant-slack--root-message thread))
        (setq page (loki-assistant-slack--thread-page thread))
        (setf (loki-assistant-slack--page-expanded page) t)
        (when reply
          (setf (loki-assistant-slack--page-items page)
                (loki-assistant-slack--merge
                 (loki-assistant-slack--page-items page) `((items . (,reply))) thread))))
      (loki-assistant-slack--render)
      (let ((message-position (loki-assistant-slack--message-position message))
            (thread-position (loki-assistant-slack--message-position thread)))
        (when-let ((position (or message-position thread-position)))
          (goto-char position)
          (recenter))
        (setq result (cond (message-position 'message) (thread-position 'thread) (t 'conversation))))
      (unless (and loki-assistant-slack--subscription
                   (equal (alist-get 'session_id room)
                          (alist-get 'session_id loki-assistant-slack--room)))
        (loki-assistant-slack-refresh)))
    result))

(defun loki-assistant-slack--stop-refresh ()
  "清理当前 buffer 的订阅、发送动画和线程视图登记。"
  (when (timerp loki-assistant-slack--read-timer)
    (cancel-timer loki-assistant-slack--read-timer)
    (setq loki-assistant-slack--read-timer nil))
  (when (buffer-live-p loki-assistant-slack--parent-buffer)
    (let ((buffer (current-buffer)) (root loki-assistant-slack--thread-root))
      (with-current-buffer loki-assistant-slack--parent-buffer
        (when (eq buffer (gethash root loki-assistant-slack--thread-buffers))
          (remhash root loki-assistant-slack--thread-buffers)))))
  (loki-assistant-slack--stop-send-timer)
  (loki-assistant-slack--cancel-subscription loki-assistant-slack--pending-subscription)
  (loki-assistant-slack--cancel-subscription loki-assistant-slack--subscription)
  (setq loki-assistant-slack--pending-subscription nil
        loki-assistant-slack--subscription nil)
  (when (timerp loki-assistant-slack--refresh-timer)
    (cancel-timer loki-assistant-slack--refresh-timer)
    (setq loki-assistant-slack--refresh-timer nil)))

;; 直接重载时清理旧版本的消息轮询，保留现有聊天 buffer。
(dolist (buffer (buffer-list))
  (with-current-buffer buffer
    (when (timerp loki-assistant-slack--refresh-timer)
      (cancel-timer loki-assistant-slack--refresh-timer)
      (setq loki-assistant-slack--refresh-timer nil))))

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

(defun loki-assistant-slack--visible-read-positions (window)
  "取得 WINDOW 中各时间线最后一条已显示到末尾的消息。"
  (let ((limit (or (window-end window t) (window-start window)))
        (positions (make-hash-table :test #'equal)))
    (save-excursion
      (goto-char (window-start window))
      (while (< (point) limit)
        (let* ((start (point))
               (ts (get-text-property start 'loki-slack-message))
               (end (next-single-property-change start 'loki-slack-message nil limit)))
          (when (and ts (not (invisible-p start))
                     (or (< end limit) (not (equal ts (get-text-property end 'loki-slack-message))))
                     (pos-visible-in-window-p (1- end) window))
            (let* ((thread (or (get-text-property start 'loki-slack-root)
                               (unless (equal ts loki-assistant-slack--thread-root)
                                 loki-assistant-slack--thread-root)))
                   (previous (gethash thread positions)))
              (when (or (null previous) (loki-assistant-slack--stamp< previous ts))
                (puthash thread ts positions))))
          (goto-char end))))
    positions))

(defun loki-assistant-slack--foreground-window-p (window)
  "判断 WINDOW 是否是当前获得焦点的聊天窗口。"
  (and (eq window (selected-window))
       (eq (frame-focus-state (window-frame window)) t)
       (not (active-minibuffer-window))))

(defun loki-assistant-slack--read-window (buffer window)
  "提交 BUFFER 在 WINDOW 中已显示的消息位置。"
  (when (buffer-live-p buffer)
    (with-current-buffer buffer
      (setq loki-assistant-slack--read-timer nil)
      (when (and (loki-assistant-slack--foreground-window-p window)
                 (eq (window-buffer window) buffer))
        (let ((positions (loki-assistant-slack--visible-read-positions window))
              (owner (or loki-assistant-slack--parent-buffer buffer)))
          (when (buffer-live-p owner)
            (with-current-buffer owner
              (maphash #'loki-assistant-slack--mark-read positions))))))))

(defun loki-assistant-slack--schedule-read (window)
  "等待重绘完成后，检查 WINDOW 已显示的消息。"
  (when (and (bound-and-true-p loki-assistant-slack-input-mode)
             (loki-assistant-slack--foreground-window-p window)
             (not (timerp loki-assistant-slack--read-timer)))
    (setq loki-assistant-slack--read-timer
          (run-at-time 0.25 nil #'loki-assistant-slack--read-window (current-buffer) window))))

(defun loki-assistant-slack--mark-read (thread ts)
  "推进当前会话的 THREAD 已读位置到 TS。"
  (when (and loki-assistant-slack--room (alist-get 'session_id loki-assistant-slack--room))
    (unless loki-assistant-slack--read-positions
      (setq loki-assistant-slack--read-positions (make-hash-table :test #'equal)))
    (let* ((buffer (current-buffer))
           (positions loki-assistant-slack--read-positions)
           (state (or (gethash thread positions)
                      (puthash thread (make-loki-assistant-slack--read-position) positions)))
           (acknowledged (loki-assistant-slack--read-position-acknowledged state)))
      (when (and (not (loki-assistant-slack--read-position-pending state))
                 (or (null acknowledged) (loki-assistant-slack--stamp< acknowledged ts))
                 (>= (float-time) (or (loki-assistant-slack--read-position-retry-at state) 0)))
        (setf (loki-assistant-slack--read-position-pending state) t)
        (loki-assistant-slack--request
         "PUT" (loki-assistant-slack--path loki-assistant-slack--room thread "read-position")
         (lambda (response)
           (when (buffer-live-p buffer)
             (with-current-buffer buffer
               (when (eq positions loki-assistant-slack--read-positions)
                 (setf (loki-assistant-slack--read-position-pending state) nil)
                 (if (plist-get response :ok)
                     (progn
                       (setf (loki-assistant-slack--read-position-acknowledged state) ts)
                       (with-current-buffer (window-buffer (selected-window))
                         (loki-assistant-slack--schedule-read (selected-window))))
                   (setf (loki-assistant-slack--read-position-retry-at state) (+ (float-time) 5)))))))
         :room loki-assistant-slack--room :body `((through_ts . ,ts)))))))

(defun loki-assistant-slack--root-message (ts)
  "返回 TS 对应的顶层消息。"
  (seq-find (lambda (item) (equal ts (alist-get 'ts item)))
            (loki-assistant-slack--page-items loki-assistant-slack--timeline)))

(defun loki-assistant-slack--thread-buffer (root)
  "返回 ROOT 线程仍存活的聊天 buffer。"
  (when-let* ((loki-assistant-slack--thread-buffers)
              (buffer (gethash root loki-assistant-slack--thread-buffers))
              ((comint-check-proc buffer)))
    buffer))

(defun loki-assistant-slack--show-buffer (buffer)
  "在当前聊天窗口显示 BUFFER，保留窗口的专用状态。"
  (let* ((window (selected-window))
         (dedicated (window-dedicated-p window)))
    (unwind-protect
        (progn
          (set-window-dedicated-p window nil)
          (switch-to-buffer buffer))
      (set-window-dedicated-p window dedicated))))

(defun loki-assistant-slack-open-thread ()
  "打开光标所在的 Slack 线程。"
  (interactive)
  (when loki-assistant-slack--thread-root (user-error "当前已在 Slack 线程中"))
  (let* ((root (or (get-text-property (point) 'loki-slack-thread-toggle)
                   (get-text-property (point) 'loki-slack-root)))
         (message (and root (loki-assistant-slack--root-message root)))
         (position-ts (get-text-property (point) 'loki-slack-message))
         (parent (current-buffer)))
    (unless message (user-error "请将光标移到带线程的消息上"))
    (unless loki-assistant-slack--thread-buffers
      (setq loki-assistant-slack--thread-buffers (make-hash-table :test #'equal)))
    (let ((buffer (or (loki-assistant-slack--thread-buffer root)
                      (loki-assistant-slack--make-chat-buffer
                       (format "*Slack %s*" (loki-assistant-slack--thread-title
                                              loki-assistant-slack--room message))))))
      (puthash root buffer loki-assistant-slack--thread-buffers)
      (with-current-buffer buffer
        (setq loki-assistant-slack--parent-buffer parent
              loki-assistant-slack--thread-root root
              loki-assistant-slack--thread-message message)
        (loki-assistant-slack--initialize-input))
      (loki-assistant-slack--render-thread buffer)
      (loki-assistant-slack--show-buffer buffer)
      (goto-char (or (loki-assistant-slack--message-position position-ts)
                     (loki-assistant-slack--message-position root)
                     (point-max)))
      buffer)))

(defun loki-assistant-slack-quit-thread (count)
  "退出线程视图。输入区的 q 按 COUNT 次数正常插入。"
  (interactive "p")
  (cond
   ((and (eq last-command-event ?q)
         (or (not loki-assistant-slack--thread-root)
             (>= (point) (loki-assistant-slack--draft-start))))
    (self-insert-command count))
   ((buffer-live-p loki-assistant-slack--parent-buffer)
    (loki-assistant-slack--show-buffer loki-assistant-slack--parent-buffer))
   (loki-assistant-slack--thread-root (bury-buffer))))

(defun loki-assistant-slack--shift-return ()
  "在输入区换行，或打开历史消息所在的线程。"
  (interactive)
  (if (>= (point) (loki-assistant-slack--draft-start))
      (newline)
    (loki-assistant-slack-open-thread)))

(defun loki-assistant-slack--thread-page (root)
  "取得或建立根消息 ROOT 的回复分页状态。"
  (unless loki-assistant-slack--threads
    (setq loki-assistant-slack--threads (make-hash-table :test #'equal)))
  (or (gethash root loki-assistant-slack--threads)
      (puthash root (make-loki-assistant-slack--page :expanded t)
               loki-assistant-slack--threads)))

(defun loki-assistant-slack--thread-visible-p (root page)
  "判断 ROOT 的 PAGE 是否用于独立线程或主聊天的展开块。"
  (or (loki-assistant-slack--thread-buffer root)
      (and (loki-assistant-slack--page-expanded page)
           (loki-assistant-slack--root-message root))))

(defun loki-assistant-slack--merge (previous data &optional thread)
  "将 DATA 的已核对区间和指定 THREAD 的消息合并到 PREVIOUS。"
  (let* ((incoming (delq nil
                         (seq-map
                          (lambda (message)
                            (let ((ts (alist-get 'ts message))
                                  (parent (loki-assistant-slack--non-null (alist-get 'thread_ts message))))
                              (when (and (stringp ts)
                                         (if thread
                                             (and (equal parent thread) (not (equal ts thread)))
                                           (or (null parent) (equal parent ts))))
                                (loki-assistant-slack--cached-record message 'ts ts '(text tables edited)))))
                          (alist-get 'items data))))
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

(defun loki-assistant-slack--load-older (page)
  "读取主时间线 PAGE 的更早消息。"
  (when (and (eq page loki-assistant-slack--timeline)
             (not (loki-assistant-slack--page-loading page)))
    (let ((buffer (current-buffer))
          (cursor (loki-assistant-slack--page-cursor page)))
      (setf (loki-assistant-slack--page-loading page) t)
      (setq loki-assistant-slack--page-changes nil)
      (loki-assistant-slack--request
       "GET" (loki-assistant-slack--path loki-assistant-slack--room nil)
       (lambda (response)
         (when (buffer-live-p buffer)
           (with-current-buffer buffer
             (when (eq page loki-assistant-slack--timeline)
               (setf (loki-assistant-slack--page-loading page) nil)
               (if (plist-get response :ok)
                   (let* ((data (plist-get response :data))
                          (next (alist-get 'next_cursor data))
                          (items (loki-assistant-slack--merge (loki-assistant-slack--page-items page) data))
                          (lower (and (stringp next) (alist-get 'ts (car items)))))
                     (loki-assistant-slack--apply-thread-push data)
                     (dolist (changes (nreverse loki-assistant-slack--page-changes))
                       (setq items (loki-assistant-slack--apply-changes items changes lower))
                       (loki-assistant-slack--apply-thread-push changes))
                     (setf (loki-assistant-slack--page-cursor page) (and (stringp next) next)
                           (loki-assistant-slack--page-error page) nil
                           (loki-assistant-slack--page-items page) items))
                 (setf (loki-assistant-slack--page-error page)
                       (or (plist-get response :error-message) "Slack 消息读取失败")))
               (setq loki-assistant-slack--page-changes nil)
               (loki-assistant-slack--update-history)
               (loki-assistant-slack--load-profiles)))))
       :room loki-assistant-slack--room :params (list (cons 'cursor cursor))))))

(defun loki-assistant-slack--update-history ()
  "为有回复的消息初始化折叠状态并重绘聊天。"
  (dolist (message (loki-assistant-slack--page-items loki-assistant-slack--timeline))
    (when (> (or (loki-assistant-slack--non-null (alist-get 'reply_count message)) 0) 0)
      (loki-assistant-slack--thread-page (alist-get 'ts message))))
  (loki-assistant-slack--render))

(defun loki-assistant-slack--subscription-path (subscription)
  "返回 SUBSCRIPTION 的本机接口路径。"
  (format "/slack/conversations/%s/subscriptions/%s"
          (url-hexify-string
           (alist-get 'id (loki-assistant-slack--subscription-room subscription)))
          (loki-assistant-slack--subscription-id subscription)))

(defun loki-assistant-slack--cancel-subscription (subscription)
  "清理本地 SUBSCRIPTION 并通知 services。"
  (when subscription
    (when-let ((timer (loki-assistant-slack--subscription-timeout subscription)))
      (cancel-timer timer))
    (let ((id (loki-assistant-slack--subscription-id subscription)))
      (when (gethash id loki-assistant-slack--subscription-buffers)
        (remhash id loki-assistant-slack--subscription-buffers)
        (loki-assistant-slack--request
         "DELETE" (loki-assistant-slack--subscription-path subscription) #'ignore)))))

(defun loki-assistant-slack--subscription-failed (subscription reason)
  "清理等待中的 SUBSCRIPTION 并显示 REASON。"
  (when (eq subscription loki-assistant-slack--pending-subscription)
    (setq loki-assistant-slack--pending-subscription nil)
    (loki-assistant-slack--cancel-subscription subscription)
    (message "%s" reason)))

(defun loki-assistant-slack-refresh ()
  "刷新最近一页及已展开的线程，并恢复当前聊天 buffer 的订阅。"
  (interactive)
  (if loki-assistant-slack--thread-root
      (if (buffer-live-p loki-assistant-slack--parent-buffer)
          (with-current-buffer loki-assistant-slack--parent-buffer
            (loki-assistant-slack-refresh))
        (user-error "请重新打开 Slack 主聊天"))
    (when (and loki-assistant-slack--room (null loki-assistant-slack--pending-subscription))
      (let ((subscription (make-loki-assistant-slack--subscription
                           :id (loki-assistant-slack--uuid)
                           :room (copy-tree loki-assistant-slack--room)))
            (buffer (current-buffer)))
        (setq loki-assistant-slack--pending-subscription subscription)
        (setf (loki-assistant-slack--subscription-timeout subscription)
              (run-at-time
               45 nil
               (lambda ()
                 (when (buffer-live-p buffer)
                   (with-current-buffer buffer
                     (loki-assistant-slack--subscription-failed
                      subscription "等待 Slack 消息快照超时"))))))
        (loki-assistant-slack--register-subscription buffer subscription t)))))

(defun loki-assistant-slack--register-subscription (buffer subscription retry)
  "获取 BUFFER 的当前会话并登记 SUBSCRIPTION。RETRY 允许重试会话变更。"
  (loki-assistant-slack--request
   "GET" "/status/slack/connections"
   (lambda (response)
     (when (buffer-live-p buffer)
       (with-current-buffer buffer
         (when (eq subscription loki-assistant-slack--pending-subscription)
           (let* ((room (loki-assistant-slack--subscription-room subscription))
                  (team (or (alist-get 'team_id room)
                            (car (split-string (alist-get 'id room) "[.]"))))
                  (connections (alist-get 'value (plist-get response :data)))
                  (connection (and (sequencep connections)
                                   (seq-find (lambda (item)
                                               (equal team (alist-get 'team_id item)))
                                             connections)))
                  (session (alist-get 'session_id connection)))
             (if (and (plist-get response :ok) (stringp session))
                 (progn
                   (setf (alist-get 'session_id room) session
                         (alist-get 'team_id room) team
                         (loki-assistant-slack--subscription-room subscription) room
                         (loki-assistant-slack--subscription-self-id subscription)
                         (loki-assistant-slack--non-null (alist-get 'user_id connection)))
                   (when-let ((name (loki-assistant-slack--non-null (alist-get 'team_name connection))))
                     (setf (alist-get 'team_name room) name))
                   (puthash (loki-assistant-slack--subscription-id subscription)
                            buffer loki-assistant-slack--subscription-buffers)
                   (loki-assistant-slack--request
                    "PUT" (loki-assistant-slack--subscription-path subscription)
                    (lambda (result)
                      (loki-assistant-slack--registration-result buffer subscription result retry))
                    :room room))
               (loki-assistant-slack--subscription-failed
                subscription (or (plist-get response :error-message)
                                 "Slack 工作区会话暂不可用，请刷新重试"))))))))))

(defun loki-assistant-slack--registration-result (buffer subscription result retry)
  "处理 BUFFER 中 SUBSCRIPTION 的登记结果 RESULT。"
  (let ((live (buffer-live-p buffer)))
    (cond
     ((plist-get result :ok)
      (unless (and live (eq (gethash (loki-assistant-slack--subscription-id subscription)
                                    loki-assistant-slack--subscription-buffers)
                            buffer))
        (loki-assistant-slack--request
         "DELETE" (loki-assistant-slack--subscription-path subscription) #'ignore)))
     (live
      (with-current-buffer buffer
        (when (eq subscription loki-assistant-slack--pending-subscription)
          (if (and retry (equal (plist-get result :error-code) "slack-session-changed"))
              (loki-assistant-slack--register-subscription buffer subscription nil)
            (loki-assistant-slack--subscription-failed
             subscription (or (plist-get result :error-message) "Slack 订阅登记失败")))))))))

(defun loki-assistant-slack--apply-changes (items data &optional lower)
  "将 DATA 中的消息变更应用到 ITEMS，LOWER 为已加载区间的起点。"
  (let ((deleted (append (alist-get 'deleted_ts data) nil)))
    (loki-assistant-slack--merge
     (seq-remove (lambda (item) (member (alist-get 'ts item) deleted)) items)
     (list (cons 'items
                 (seq-filter
                  (lambda (item)
                    (let ((ts (alist-get 'ts item)))
                      (and (stringp ts)
                           (or (null lower) (not (loki-assistant-slack--stamp< ts lower))
                               (seq-some (lambda (entry) (equal ts (alist-get 'ts entry))) items)))))
                  (alist-get 'items data)))))))

(defun loki-assistant-slack--apply-thread-push (data)
  "将 DATA 内同一线程的变更一次性合并到共享回复。"
  (let ((items (make-hash-table :test #'equal))
        (deleted (make-hash-table :test #'equal)))
    (seq-doseq (item (alist-get 'thread_items data))
      (when-let* ((root (alist-get 'thread_ts item)) ((stringp root)))
        (push item (gethash root items))))
    (seq-doseq (item (alist-get 'deleted_thread_messages data))
      (when-let* ((root (alist-get 'thread_ts item)) ((stringp root)))
        (push (alist-get 'ts item) (gethash root deleted))))
    (maphash (lambda (root timestamps)
               (when-let ((page (and loki-assistant-slack--threads
                                     (gethash root loki-assistant-slack--threads))))
                 (setf (loki-assistant-slack--page-items page)
                       (seq-remove (lambda (item) (member (alist-get 'ts item) timestamps))
                                   (loki-assistant-slack--page-items page)))))
             deleted)
    (maphash (lambda (root replies)
               (let ((page (loki-assistant-slack--thread-page root)))
                 (setf (loki-assistant-slack--page-items page)
                       (loki-assistant-slack--merge (loki-assistant-slack--page-items page)
                                                    `((items . ,replies)) root))))
             items)))

(defun loki-assistant-slack--apply-snapshot (subscription snapshot)
  "将 SUBSCRIPTION 的 SNAPSHOT 安装到当前 buffer。"
  (let ((previous loki-assistant-slack--subscription)
        (room (loki-assistant-slack--subscription-room subscription))
        (cursor (alist-get 'next_cursor snapshot)))
    (unless (equal (alist-get 'session_id room)
                   (alist-get 'session_id loki-assistant-slack--room))
      (setq loki-assistant-slack--profiles (make-hash-table :test #'equal)
            loki-assistant-slack--read-positions nil
            loki-assistant-slack--send-request nil))
    (when (and loki-assistant-slack--threads (assq 'thread_items snapshot))
      (maphash (lambda (root page)
                 (unless (equal root (alist-get 'ts loki-assistant-slack--notification-root))
                   (setf (loki-assistant-slack--page-items page) nil)))
               loki-assistant-slack--threads))
    (loki-assistant-slack--apply-thread-push snapshot)
    (setq loki-assistant-slack--timeline
          (make-loki-assistant-slack--page
           :items (loki-assistant-slack--merge
                   (and loki-assistant-slack--notification-root
                        (list loki-assistant-slack--notification-root))
                   snapshot)
           :cursor (and (stringp cursor) cursor)
           :loaded t)
          loki-assistant-slack--page-changes nil
          loki-assistant-slack--notification-root nil
          loki-assistant-slack--room room
          loki-assistant-slack--self-id (loki-assistant-slack--subscription-self-id subscription))
    (when (eq subscription loki-assistant-slack--pending-subscription)
      (when-let ((timer (loki-assistant-slack--subscription-timeout subscription)))
        (cancel-timer timer)
        (setf (loki-assistant-slack--subscription-timeout subscription) nil))
      (setq loki-assistant-slack--subscription subscription
            loki-assistant-slack--pending-subscription nil)
      ;; 回调返回后再向 services 取消旧订阅。
      (when previous
        (remhash (loki-assistant-slack--subscription-id previous)
                 loki-assistant-slack--subscription-buffers)
        (run-at-time
         0 nil #'loki-assistant-slack--request "DELETE"
         (loki-assistant-slack--subscription-path previous) #'ignore)))
    (loki-assistant-slack--update-history)))

(defun loki-assistant-slack-dispatch-messages (payload-file)
  "应用 services 的 PAYLOAD-FILE，返回 applied 或 buffer-closed。"
  (let* ((data (with-temp-buffer
                 (let ((coding-system-for-read 'utf-8))
                   (insert-file-contents payload-file))
                 (json-parse-buffer :object-type 'alist :array-type 'list :null-object nil)))
         (id (alist-get 'subscription_id data))
         (buffer (gethash id loki-assistant-slack--subscription-buffers)))
    (unless (equal (alist-get 'api_version data) "1")
      (error "Slack 投递协议版本无效"))
    (if (not (buffer-live-p buffer))
        'buffer-closed
      (with-current-buffer buffer
        (let* ((pending loki-assistant-slack--pending-subscription)
               (active loki-assistant-slack--subscription)
               (subscription (seq-find
                              (lambda (entry)
                                (and entry (equal id (loki-assistant-slack--subscription-id entry))))
                              (list pending active)))
               (room (and subscription (loki-assistant-slack--subscription-room subscription)))
               (kind (alist-get 'kind data))
               (sequence (alist-get 'sequence data)))
          (cond
           ((not (and subscription (equal (alist-get 'session_id data) (alist-get 'session_id room))
                      (equal (alist-get 'conversation_id data) (alist-get 'id room))))
            'buffer-closed)
           ((equal kind "probe") 'applied)
           ((<= sequence (loki-assistant-slack--subscription-sequence subscription)) 'applied)
           ((/= sequence (1+ (loki-assistant-slack--subscription-sequence subscription)))
            (error "Slack 消息序号不连续"))
           (t
            (pcase kind
              ("snapshot"
               (loki-assistant-slack--apply-snapshot subscription (alist-get 'snapshot data)))
              ("changes"
               (unless (eq subscription active) (error "Slack 订阅缺少快照"))
               (let* ((page loki-assistant-slack--timeline)
                      (items (loki-assistant-slack--page-items page))
                      (lower (and (loki-assistant-slack--page-cursor page)
                                  (alist-get 'ts (car items)))))
                 (setf (loki-assistant-slack--page-items page)
                       (loki-assistant-slack--apply-changes items data lower))
                 (when (loki-assistant-slack--page-loading page)
                   (push data loki-assistant-slack--page-changes))
                 (when loki-assistant-slack--threads
                   (seq-doseq (root (alist-get 'deleted_ts data))
                     (when-let ((child (loki-assistant-slack--thread-buffer root)))
                       (with-current-buffer child (setq loki-assistant-slack--thread-message nil)))
                     (remhash root loki-assistant-slack--threads)))
                 (loki-assistant-slack--apply-thread-push data)
                 (loki-assistant-slack--update-history)))
              (_ (error "未知的 Slack 消息投递类型")))
            (setf (loki-assistant-slack--subscription-sequence subscription) sequence)
            (run-at-time 0 nil
                         (lambda ()
                           (when (buffer-live-p buffer)
                             (with-current-buffer buffer (loki-assistant-slack--load-profiles)))))
            'applied)))))))

(defun loki-assistant-slack--section-texts (block)
  "返回 BLOCK 中有序的、可显示的 section 文本对象。"
  (when (equal (alist-get 'type block) "section")
    (seq-filter (lambda (part)
                  (and (member (alist-get 'type part) '("mrkdwn" "plain_text"))
                       (stringp (alist-get 'text part))
                       (not (string-blank-p (alist-get 'text part)))))
                (cons (alist-get 'text block) (append (alist-get 'fields block) nil)))))

(defun loki-assistant-slack--thinking-placeholder-p (message)
  "判断 MESSAGE 是否只包含 Slackbot 的思考占位文本。"
  (let* ((author (loki-assistant-slack--non-null (alist-get 'author message)))
         (id (alist-get 'id author))
         (blocks (alist-get 'blocks message)))
    (when (and (stringp id)
               (or (equal id "USLACKBOT") (string-suffix-p ".USLACKBOT" id))
               (seq-empty-p (alist-get 'attachments message))
               (seq-empty-p (alist-get 'tables message))
               (not (seq-some (lambda (block) (equal (alist-get 'type block) "table")) blocks)))
      (let* ((sections (seq-mapcat #'loki-assistant-slack--section-texts blocks))
             (text (if sections
                       (unless (cdr sections) (alist-get 'text (car sections)))
                     (alist-get 'text message)))
             (trim "[_*[:space:]]+"))
        (and (stringp text)
             (string-match-p "\\`思考中[.。…]*\\'" (string-trim text trim trim)))))))

(defun loki-assistant-slack--block-users (message team)
  "返回 MESSAGE 的 mrkdwn 区块中提及的 TEAM 用户。"
  (let (users)
    (seq-doseq (block (alist-get 'blocks message))
      (dolist (part (loki-assistant-slack--section-texts block))
        (when (equal (alist-get 'type part) "mrkdwn")
          (with-temp-buffer
            (insert (alist-get 'text part))
            (goto-char (point-min))
            (while (re-search-forward loki-assistant-slack--markup-regexp nil t)
              (if (equal (match-string 0) "```")
                  (unless (search-forward "```" nil t) (goto-char (point-max)))
                (when-let ((tag (match-string 1)))
                  (when (string-match "\\`@\\([UW][A-Z0-9]+\\)\\(?:|.*\\)?\\'" tag)
                    (push (concat team "." (match-string 1 tag)) users)))))))))
    users))

(defun loki-assistant-slack--load-profiles ()
  "批量补齐当前聊天中可见的发送者、提及和本人资料。"
  (let ((profiles loki-assistant-slack--profiles)
        (buffer (current-buffer))
        (team (alist-get 'team_id loki-assistant-slack--room))
        (items (loki-assistant-slack--page-items loki-assistant-slack--timeline))
        ids)
    (when loki-assistant-slack--threads
      (maphash (lambda (root page)
                 (when (loki-assistant-slack--thread-visible-p root page)
                   (setq items (append (loki-assistant-slack--page-items page) items))))
               loki-assistant-slack--threads))
    (dolist (id (cons loki-assistant-slack--self-id
                      (seq-mapcat
                       (lambda (message)
                         (cons (alist-get 'id (loki-assistant-slack--non-null (alist-get 'author message)))
                               (loki-assistant-slack--block-users message team)))
                       items)))
      (when (and (stringp id) (stringp team) (string-prefix-p (concat team ".") id)
                 (null (gethash id profiles)))
        (puthash id 'pending profiles)
        (push id ids)))
    (while ids
      (let ((batch (seq-take ids 100)))
        (setq ids (nthcdr (length batch) ids))
        (loki-assistant-slack--request
         "GET" "/slack/users"
         (lambda (response)
           (dolist (id batch) (puthash id 'unavailable profiles))
           (when (plist-get response :ok)
             (seq-doseq (profile (alist-get 'items (plist-get response :data)))
               (let ((id (alist-get 'id profile)))
                 (puthash id (loki-assistant-slack--cached-record
                              profile 'id id '(name display_name username is_bot)) profiles)))
             (when (buffer-live-p buffer)
               (with-current-buffer buffer
                 (when (eq profiles loki-assistant-slack--profiles)
                   (loki-assistant-slack--render))))))
         :params (mapcar (lambda (id) (cons 'id id)) batch))))))

(defun loki-assistant-slack--draft-start ()
  "返回当前会话输入的开始位置。"
  (if-let ((process (get-buffer-process (current-buffer))))
      (marker-position (process-mark process))
    (user-error "请重新打开此 Slack 会话")))

(defun loki-assistant-slack--prompt-start ()
  "从进程标记定位实际的 Slack 输入提示符。"
  (let* ((end (loki-assistant-slack--draft-start))
         (start (- end (length loki-assistant-slack--prompt))))
    (unless (and (>= start (point-min))
                 (equal (buffer-substring-no-properties start end) loki-assistant-slack--prompt))
      (user-error "Slack 输入提示符已失效，请重新打开此会话"))
    start))

(defun loki-assistant-slack--move-item (backward count)
  "按 COUNT 移动消息或按钮，BACKWARD 非空时向上移动。
在末尾输入区按字母键时插入该字母。"
  (let ((draft-start (loki-assistant-slack--draft-start)))
    (if (and (>= (point) draft-start) (memq last-command-event '(?n ?p)))
        (self-insert-command count)
      (dotimes (_ (abs count))
        (let* ((origin (point))
               (reverse (if (< count 0) (not backward) backward))
               (search-message (if reverse #'text-property-search-backward
                                 #'text-property-search-forward))
               (search-button (if reverse #'previous-button #'next-button))
               (message (save-excursion
                          (let ((match (funcall search-message 'loki-slack-message nil nil (not reverse))))
                            (while (and match (invisible-p (prop-match-beginning match)))
                              (setq match (funcall search-message 'loki-slack-message)))
                            match)))
               (button (funcall search-button origin)))
          (while (and button (invisible-p (button-start button)))
            (setq button (funcall search-button (button-start button))))
          (let ((positions (delq nil
                                 (list (and message (prop-match-beginning message))
                                       (and button (/= origin (button-start button)) (button-start button))
                                       (and (not reverse) (< origin draft-start) draft-start)))))
            (when positions
              (deactivate-mark)
              (goto-char (apply (if reverse #'max #'min) positions)))))))))

(defun loki-assistant-slack-next-item (count)
  "向下移动 COUNT 个消息或按钮。"
  (interactive "p")
  (loki-assistant-slack--move-item nil count))

(defun loki-assistant-slack-previous-item (count)
  "向上移动 COUNT 个消息或按钮。"
  (interactive "p")
  (loki-assistant-slack--move-item t count))

(defun loki-assistant-slack--toggle-thread (root)
  "展开或折叠根消息 ROOT 下的回复。"
  (let ((page (loki-assistant-slack--thread-page root)))
    (setf (loki-assistant-slack--page-expanded page)
          (not (loki-assistant-slack--page-expanded page)))
    (loki-assistant-slack--render)))

(defun loki-assistant-slack--click-thread (event)
  "在鼠标 EVENT 指向的根消息处展开或折叠线程。"
  (interactive "e")
  (mouse-set-point event)
  (when-let ((root (get-text-property (point) 'loki-slack-thread-toggle)))
    (loki-assistant-slack--toggle-thread root)))

(defvar loki-assistant-slack--thread-map
  (let ((map (make-sparse-keymap)))
    (define-key map [mouse-1] #'loki-assistant-slack--click-thread)
    map)
  "根消息上切换线程的按键映射。")

(defun loki-assistant-slack--main-target ()
  "切换到主会话输入。"
  (interactive)
  (when loki-assistant-slack--reply
    (when (< (loki-assistant-slack--draft-start) (point-max))
      (user-error "请先发送或清空当前草稿"))
    (setq loki-assistant-slack--reply nil)
    (loki-assistant-slack--render))
  (goto-char (point-max)))

(defun loki-assistant-slack--link-p (url)
  "判断 URL 是否为可打开的网页、Slack 应用或邮件链接。"
  (and (stringp url)
       (or (string-match-p "\\`\\(?:https?\\|slack\\)://[^/?#[:space:]]+" url)
           (string-match-p "\\`mailto:[^[:space:]?#]+\\(?:\\?[^[:space:]#]*\\)?\\'" url))))

(defun loki-assistant-slack--emoji (code)
  "返回 CODE 对应的 Unicode 表情。"
  (or (and (equal code ":wave:") "👋")
      (and (equal code ":bar_chart:") "📊")
      (ignore-errors
        (save-match-data
          (when (require 'emojify nil t)
            (emojify-create-emojify-emojis)
            (when-let ((data (emojify-get-emoji code)))
              (gethash "unicode" data)))))))

(defun loki-assistant-slack--link (label url)
  "插入指向 URL 的 LABEL。"
  (if (loki-assistant-slack--link-p url)
      (insert-text-button label 'face 'loki-assistant-slack-action 'follow-link t 'help-echo url
                          'action (lambda (_) (browse-url url)))
    (insert label)))

(defun loki-assistant-slack--decode-entity (entity)
  "将 Slack 字符实体 ENTITY 解码一次。"
  (pcase entity ("&amp;" "&") ("&lt;" "<") ("&gt;" ">") (_ entity)))

(defun loki-assistant-slack--mrkdwn-emphasis (delimiter openers)
  "将当前位置的 Slack DELIMITER 转为 Markdown，返回更新后的 OPENERS。"
  (let* ((kind (aref delimiter 0))
         (start (match-beginning 0))
         (opener (alist-get kind openers)))
    (cond
     ((/= (length delimiter) 1) openers)
     ((and opener (>= opener (line-beginning-position))
           (not (memq (char-before start) '(?\s ?\t))))
      (insert delimiter)
      (save-excursion
        (goto-char opener)
        (insert delimiter))
      (assq-delete-all kind openers))
     ((looking-at-p "[^[:space:]]")
      (setf (alist-get kind openers) start)
      openers)
     (t openers))))

(defun loki-assistant-slack--markdown (text team profiles tables &optional mrkdwn)
  "渲染 TEXT 和 TABLES。MRKDWN 启用 Slack 区块文本的强调语法。"
  (let* ((markup (if mrkdwn
                     (concat loki-assistant-slack--markup-regexp "\\|\\\\[*~]\\|\\([*]+\\|~+\\)")
                   loki-assistant-slack--markup-regexp))
         openers
         (content
         (with-temp-buffer
           (insert text "\n")
           (goto-char (point-min))
           (while (re-search-forward markup nil t)
             (cond
              ((equal (match-string 0) "```") (unless (search-forward "```" nil t) (goto-char (point-max))))
              ((match-string 2)
               (replace-match (loki-assistant-slack--decode-entity (match-string 2)) t t))
              ((match-string 3)
               (when-let ((emoji (loki-assistant-slack--emoji (match-string 3))))
                 (replace-match emoji t t)))
              ((match-string 1)
               (let* ((markup (match-string 1))
                      (separator (string-search "|" markup))
                      (target (substring markup 0 separator))
                      (label (when separator (substring markup (1+ separator))))
                      (replacement
                       (cond
                        ((string-match-p "\\`[A-Za-z][A-Za-z0-9+.-]*:" target)
                         (propertize (or label target) 'agent-shell-markdown-url target))
                        ((string-prefix-p "@" target)
                         (let ((profile (gethash (concat team "." (substring target 1)) profiles)))
                           (concat "@" (or (and (listp profile) (alist-get 'name profile))
                                           label (substring target 1)))))
                        ((string-prefix-p "#" target)
                         (concat "#" (or label (substring target 1)))))))
                 (when replacement (replace-match replacement t t))))
              ((and mrkdwn (match-string 4))
               (setq openers (loki-assistant-slack--mrkdwn-emphasis (match-string 4) openers)))))
           (goto-char (point-max))
           (seq-doseq (table tables)
             (when (> (length table) 0)
               (insert "\n")
               (seq-doseq (row table)
                 (insert "| " (mapconcat (lambda (cell)
                                             (string-replace "|" "\\|" (or (loki-assistant-slack--single-line cell) "")))
                                           row " | ") " |\n")
                 (when (eq row (elt table 0))
                   (insert "| " (mapconcat (lambda (_) "---") row " | ") " |\n")))))
           (when (require 'agent-shell-markdown nil t)
             (let ((agent-shell-markdown-render-functions nil))
               (cl-letf (((symbol-function 'agent-shell-markdown--linkify-file-references) #'ignore))
                 (agent-shell-markdown-replace-markup :complete t :render-images nil :highlight-blocks nil))))
           (let ((start (point-min)))
             (while (< start (point-max))
               (let ((end (next-single-property-change start 'agent-shell-markdown-url nil (point-max)))
                     (url (get-text-property start 'agent-shell-markdown-url)))
                 (when url
                   (remove-text-properties start end '(keymap nil help-echo nil pointer nil browse-url-data nil cursor-sensor-functions nil
                                                               agent-shell-markdown-url nil))
                   (when (loki-assistant-slack--link-p url)
                     (make-text-button start end 'follow-link t 'help-echo url
                                       'action (lambda (_) (browse-url url)))
                     (add-face-text-property start end 'loki-assistant-slack-action)))
                 (setq start end))))
           (buffer-string))))
    (insert content)))

(defun loki-assistant-slack--message-body (message team profiles)
  "按顺序渲染 MESSAGE 的正文区块，使用 TEAM 和 PROFILES 解析提及。"
  (let* ((text (or (alist-get 'text message) ""))
         (blocks (alist-get 'blocks message))
         (legacy-tables (unless (seq-some (lambda (block) (equal (alist-get 'type block) "table")) blocks)
                          (alist-get 'tables message))))
    (unless (seq-some #'loki-assistant-slack--section-texts blocks)
      (when (or (not (string-empty-p text)) (not (seq-empty-p legacy-tables)))
        (loki-assistant-slack--markdown text team profiles legacy-tables)))
    (seq-doseq (block blocks)
      (pcase (alist-get 'type block)
        ("section"
         (dolist (part (loki-assistant-slack--section-texts block))
           (let ((body (string-remove-suffix "\n" (alist-get 'text part))))
             (if (equal (alist-get 'type part) "plain_text")
                 (insert (replace-regexp-in-string loki-assistant-slack--entity-regexp
                                                   #'loki-assistant-slack--decode-entity body t t) "\n")
               (loki-assistant-slack--markdown body team profiles nil t)))))
        ("table"
         (loki-assistant-slack--markdown "" team profiles (list (alist-get 'rows block))))))))

(defun loki-assistant-slack--attachment (attachment index ts room)
  "插入 ROOM 中消息 TS 的第 INDEX 个 ATTACHMENT。"
  (let ((title (or (loki-assistant-slack--single-line (alist-get 'title attachment)) "附件"))
        (url (alist-get 'url attachment))
        (size (alist-get 'size attachment))
        (text (alist-get 'text attachment))
        (endpoint (and (stringp ts)
                       (format "%s/%s/attachments/%s" (loki-assistant-slack--path room nil)
                               (url-hexify-string ts) index))))
    (if (and endpoint (eq (alist-get 'original_available attachment) t))
        (insert-text-button title 'face 'loki-assistant-slack-action 'follow-link t
                            'action (lambda (_)
                                      (loki-assistant-slack--open-attachment attachment endpoint room)))
      (loki-assistant-slack--link title url))
    (when (numberp size)
      (insert (propertize (format " (%s)" (file-size-human-readable size)) 'face 'shadow)))
    (insert "\n")
    (when (and (stringp text) (not (string-empty-p text)))
      (let ((start (point)))
        (loki-assistant-slack--markdown text (alist-get 'team_id room) (make-hash-table :test #'equal) nil)
        (add-text-properties start (point) '(face shadow))))
    (when-let ((image (loki-assistant-slack--file-image (alist-get 'preview_path attachment) 560 320)))
      (insert (propertize title 'display image) "\n"))))

(defun loki-assistant-slack--open-attachment (attachment endpoint room)
  "打开 ROOM 中 ENDPOINT 对应的 ATTACHMENT。"
  (let ((open (lambda (path)
                (when-let ((path (loki-assistant-slack--local-file path)))
                  (let ((enable-local-variables nil)
                        (enable-local-eval nil))
                    (find-file path))
                  t))))
    (unless (funcall open (alist-get 'original_path attachment))
      (loki-assistant-slack--request
       "GET" (concat endpoint "/original")
       (lambda (response)
         (unless (and (plist-get response :ok)
                      (funcall open (alist-get 'cache_path (plist-get response :data))))
           (let ((url (alist-get 'url attachment)))
             (if (loki-assistant-slack--link-p url)
                 (browse-url url)
               (message "%s" (or (plist-get response :error-message) "Slack 附件暂不可用"))))))
       :room room))))

(defun loki-assistant-slack--insert-label (name face profile)
  "为 NAME 的身份色块保留一行，并按 FACE 显示姓名和 PROFILE 的头像。"
  (let* ((avatar (when (consp profile)
                   (loki-assistant-slack--profile-avatar profile)))
         (label (concat " " (when (and avatar (get-text-property 0 'display avatar))
                              (concat avatar " "))
                        name " ")))
    (insert (propertize "\n" 'loki-slack-label (propertize label 'face face)))))

(defun loki-assistant-slack--message-time (time)
  "按本地日期格式化消息发送时间 TIME。"
  (let ((now (current-time)))
    (format-time-string
     (pcase (- (time-to-days now) (time-to-days time))
       (0 "今天 %H:%M")
       (1 "昨天 %H:%M")
       (_ (if (equal (format-time-string "%Y" now) (format-time-string "%Y" time))
              "%m月%d日 %H:%M"
            "%Y年%m月%d日 %H:%M")))
     time)))

(defun loki-assistant-slack--insert-message-time (start time)
  "为从 START 开始的消息添加时间色块 TIME。"
  (let ((label (propertize (concat " " (loki-assistant-slack--message-time time) " ")
                           'face 'loki-assistant-slack-message-time))
        (tail (save-excursion
                (skip-chars-backward "\n" start)
                (point))))
    (if (and (< start tail)
             (not (or (get-text-property (1- tail) 'display)
                      (get-text-property (1- tail) 'agent-shell-markdown-table-source)
                      (get-text-property (1- tail) 'agent-shell-markdown-frozen))))
        (if (= tail (point))
            (insert "  " label "\n")
          (save-excursion
            (goto-char tail)
            (insert "  " label)))
      (unless (bolp) (insert "\n"))
      (insert label "\n"))))

(defun loki-assistant-slack--message (message room profiles)
  "渲染 ROOM 中的 MESSAGE 正文和附件。"
  (let* ((start (point))
         (ts (alist-get 'ts message))
         (time (seconds-to-time (string-to-number ts)))
         (timestamp (format-time-string "%Y-%m-%d %H:%M" time))
         (help (if (eq (alist-get 'edited message) t) (concat timestamp "（已编辑）") timestamp)))
    (when-let ((icon (or (when-let ((image (loki-assistant-slack--file-image
                                          (alist-get 'icon_path message) 20 20)))
                           (propertize " " 'display image))
                         (when-let ((emoji (alist-get 'icon_emoji message)))
                           (loki-assistant-slack--emoji emoji)))))
      (insert icon " "))
    (loki-assistant-slack--message-body message (alist-get 'team_id room) profiles)
    (cl-loop for attachment across (vconcat (alist-get 'attachments message)) for index from 0
             do (loki-assistant-slack--attachment attachment index ts room))
    (loki-assistant-slack--insert-message-time start time)
    (add-text-properties start (point) (list 'line-prefix "  " 'wrap-prefix "  " 'loki-slack-message ts))
    (let ((position start))
      (while (< position (point))
        (let ((end (next-single-property-change position 'help-echo nil (point))))
          (unless (get-text-property position 'help-echo)
            (put-text-property position end 'help-echo help))
          (setq position end))))))

(defun loki-assistant-slack--pagination (page)
  "为主时间线 PAGE 插入错误或翻页按钮。"
  (when-let ((error (loki-assistant-slack--page-error page)))
    (insert (propertize (concat error "\n") 'face 'error)))
  (when (loki-assistant-slack--page-cursor page)
    (insert-text-button "更早的消息" 'face 'loki-assistant-slack-action 'follow-link t
                        'action (lambda (_) (loki-assistant-slack--load-older page)))
    (insert "\n\n")))

(defun loki-assistant-slack--insert-author-label (author profiles)
  "显示 AUTHOR 的身份色块，使用 PROFILES 中已缓存的资料。"
  (let* ((id (alist-get 'id author))
         (entry (gethash id profiles))
         (profile (and (listp entry) entry))
         (name (or (alist-get 'name profile) (alist-get 'name author)))
         (self (and id (equal id loki-assistant-slack--self-id))))
    (loki-assistant-slack--insert-label
     (if self "我" (or (loki-assistant-slack--single-line name) "Slack"))
     (if self 'loki-assistant-slack-self-label 'loki-assistant-slack-sender-label)
     (if (alist-get 'avatar_path author) author profile))))

(defun loki-assistant-slack--thread-block (message room profiles message-start)
  "以从 MESSAGE-START 开始的消息正文作为线程折叠块的标题。"
  (let* ((root (alist-get 'ts message))
         (page (and loki-assistant-slack--threads (gethash root loki-assistant-slack--threads)))
         (expanded (and page (loki-assistant-slack--page-expanded page)))
         (replies (and page (loki-assistant-slack--page-items page))))
    (when (and page (or replies (> (or (loki-assistant-slack--non-null
                                          (alist-get 'reply_count message)) 0) 0)))
      (let ((native (require 'agent-shell-ui nil t))
            (start (point)))
        (when (or native expanded)
          (loki-assistant-slack--insert-messages replies room profiles)
          (put-text-property start (point) 'loki-slack-root root))
        ;; agent-shell 会裁去正文开头的换行。保留第一条回复的身份色块锚点。
        (when (and native (get-text-property start 'loki-slack-label))
          (save-excursion
            (goto-char start)
            (insert (propertize " " 'display ""))))
        (let* ((body (delete-and-extract-region start (point)))
               (label (buffer-substring message-start (1- start)))
               (label (if (string-empty-p label) " " label))
               (header-end (+ message-start 2 (length label))))
          (delete-region message-start (point))
          (if native
              (let ((fragment (agent-shell-ui-update-fragment
                               (agent-shell-ui-make-fragment-model
                                :namespace-id "loki-slack" :block-id root
                                :label-left label :body (if (string-empty-p body) " " body))
                               :create-new t :expanded expanded :no-undo t)))
                (goto-char (point-max))
                (delete-char -1)
                (delete-region message-start (alist-get :start (alist-get :block fragment))))
            (insert (if expanded "▼ " "▶ ") label "\n" body))
          ;; 消息正文切换线程，正文内的链接和附件沿用各自的按钮。
          (remove-text-properties message-start header-end '(keymap nil cursor-sensor-functions nil))
          (add-text-properties message-start header-end
                               (list 'loki-slack-message root 'loki-slack-thread-toggle root))
          (let ((position message-start))
            (while (< position header-end)
              (let ((end (next-single-property-change position 'button nil header-end)))
                (unless (button-at position)
                  (add-text-properties position end
                                       (list 'keymap loki-assistant-slack--thread-map 'pointer 'hand)))
                (setq position end))))))
      expanded)))

(defun loki-assistant-slack--insert-messages (items room profiles &optional inline-threads)
  "显示 ITEMS，按发送者分组。INLINE-THREADS 非空时内嵌回复。"
  (let (sender)
    (dolist (message (seq-remove #'loki-assistant-slack--thinking-placeholder-p items))
      (let* ((author (loki-assistant-slack--non-null (alist-get 'author message)))
           (id (alist-get 'id author)))
        (unless (and (stringp id) (not (string-empty-p id)) (equal sender id))
          (loki-assistant-slack--insert-author-label author profiles))
        (let ((start (point)))
          (loki-assistant-slack--message message room profiles)
          (setq sender (unless (and inline-threads
                                    (loki-assistant-slack--thread-block message room profiles start))
                         id)))
        (insert "\n")))))

(defun loki-assistant-slack--history (timeline room profiles &optional thread root)
  "显示 TIMELINE 或 THREAD 中 ROOT 消息及其回复，再显示输入区。"
  (let ((inhibit-read-only t))
    (cond
     ((not thread)
      (loki-assistant-slack--pagination timeline)
      (loki-assistant-slack--insert-messages (loki-assistant-slack--page-items timeline)
                                             room profiles t))
     (root
      (loki-assistant-slack--insert-messages (list root) room profiles)
      (when-let ((page (gethash thread loki-assistant-slack--threads)))
        (loki-assistant-slack--insert-messages (loki-assistant-slack--page-items page) room profiles)))
     (t (insert (propertize "线程已不可用\n\n" 'face 'shadow))))
    (loki-assistant-slack--insert-label (if loki-assistant-slack--reply "我 · 线程回复" "我")
                                       'loki-assistant-slack-self-label
                                       (gethash loki-assistant-slack--self-id profiles))))

(defun loki-assistant-slack--decorate (prompt input)
  "用 overlay 显示身份色块和 PROMPT 到 INPUT 之间的输入提示符。"
  (save-excursion
    (goto-char (point-min))
    ;; 色块锚定在正文前的换行，避免光标落在显示字符串里。
    (while-let ((label (text-property-search-forward 'loki-slack-label)))
      (unless (invisible-p (prop-match-beginning label))
        (let ((overlay (make-overlay (prop-match-beginning label) (prop-match-end label))))
          (overlay-put overlay 'loki-slack-chat-display t)
          (overlay-put overlay 'before-string (concat (prop-match-value label) "\n")))))
    (let ((overlay (make-overlay prompt input)))
      (overlay-put overlay 'loki-slack-chat-display t)
      (overlay-put overlay 'display (propertize "  ❯ " 'face 'default))))
  (loki-assistant-slack--update-send-prompt (current-buffer)))

(defun loki-assistant-slack--stop-send-timer ()
  "停止当前聊天的发送动画。"
  (when (timerp loki-assistant-slack--send-timer)
    (cancel-timer loki-assistant-slack--send-timer)
    (setq loki-assistant-slack--send-timer nil)))

(defun loki-assistant-slack--update-send-prompt (buffer)
  "根据 BUFFER 的发送状态更新输入提示符。"
  (when (buffer-live-p buffer)
    (with-current-buffer buffer
      (let ((busy (and (plist-get loki-assistant-slack--send-request :busy)
                       (comint-check-proc buffer))))
        (if busy
            (unless (timerp loki-assistant-slack--send-timer)
              (setq loki-assistant-slack--send-timer
                    (run-at-time 0.125 0.125 #'loki-assistant-slack--update-send-prompt buffer)))
          (loki-assistant-slack--stop-send-timer))
        (when-let* ((start (and loki-assistant-slack--history-end
                                 (marker-position loki-assistant-slack--history-end)))
                    (prompt (seq-find (lambda (overlay)
                                        (and (overlay-get overlay 'loki-slack-chat-display)
                                             (overlay-get overlay 'display)))
                                      (overlays-at start))))
          (let ((frame (if busy (elt ["|" "/" "-" "\\"] (mod (floor (* 8 (float-time))) 4)) " ")))
            (overlay-put prompt 'display
                         (concat (propertize frame 'face 'shadow)
                                 (propertize " ❯ " 'face 'default)))))))))

(defun loki-assistant-slack--render ()
  "更新当前聊天和关联线程的界面。"
  (loki-assistant-slack--render-buffer)
  (when loki-assistant-slack--thread-buffers
    (maphash (lambda (_ buffer)
               (when (and (buffer-live-p buffer) (comint-check-proc buffer))
                 (loki-assistant-slack--render-thread buffer)))
             loki-assistant-slack--thread-buffers)))

(defun loki-assistant-slack--render-thread (buffer)
  "用主聊天的数据更新线程 BUFFER。"
  (let* ((room loki-assistant-slack--room)
         (self-id loki-assistant-slack--self-id)
         (profiles loki-assistant-slack--profiles)
         (threads loki-assistant-slack--threads)
         (root (loki-assistant-slack--root-message
                (buffer-local-value 'loki-assistant-slack--thread-root buffer))))
    (with-current-buffer buffer
      (setq loki-assistant-slack--room room
            loki-assistant-slack--self-id self-id
            loki-assistant-slack--profiles profiles
            loki-assistant-slack--threads threads
            loki-assistant-slack--thread-message (or root loki-assistant-slack--thread-message))
      (loki-assistant-slack--render-buffer))))

(defun loki-assistant-slack--render-buffer ()
  "更新提示符上方的历史，保留当前输入和编辑位置。"
  (add-hook 'pre-redisplay-functions #'loki-assistant-slack--schedule-read nil t)
  ;; 历史已带有正文、链接和时间的 face，避免 Font Lock 在显示时清除它们。
  (when font-lock-mode (font-lock-mode -1))
  (let* ((timeline loki-assistant-slack--timeline)
         (message (get-text-property (point) 'loki-slack-message))
         (root (get-text-property (point) 'loki-slack-root))
         (offset (when message
                   (- (next-single-property-change (point) 'loki-slack-message nil (point-max))
                      (point))))
         (room loki-assistant-slack--room)
         (self-id loki-assistant-slack--self-id)
         (reply loki-assistant-slack--reply)
         (thread loki-assistant-slack--thread-root)
         (thread-message loki-assistant-slack--thread-message)
         (threads loki-assistant-slack--threads)
         (profiles loki-assistant-slack--profiles)
         (history (with-temp-buffer
                    (let ((loki-assistant-slack--self-id self-id)
                          (loki-assistant-slack--reply reply)
                          (loki-assistant-slack--threads threads))
                      (loki-assistant-slack--history timeline room profiles thread thread-message))
                    (buffer-string)))
         (modified (buffer-modified-p))
         (inhibit-read-only t)
         (buffer-undo-list t))
    (loki-assistant-slack--update-title)
    (save-excursion
      (remove-overlays (point-min) (point-max) 'loki-slack-chat-display t)
      (delete-region (point-min) (loki-assistant-slack--prompt-start))
      (goto-char (point-min))
      (insert history)
      (add-text-properties (point-min) (point)
                           '(read-only t front-sticky (read-only) rear-nonsticky t))
      (set-marker loki-assistant-slack--history-end (point)))
    (when-let ((process (get-buffer-process (current-buffer))))
      (let ((start (marker-position loki-assistant-slack--history-end))
            (end (marker-position (process-mark process))))
        (when (and (markerp (car-safe comint-last-prompt)) (markerp (cdr-safe comint-last-prompt)))
          (set-marker (car comint-last-prompt) start)
          (set-marker (cdr comint-last-prompt) end))
        (with-silent-modifications
          (remove-text-properties start end '(display nil)))
        (loki-assistant-slack--decorate start end)))
    (let ((anchor (when message
                    (save-excursion
                      (goto-char (point-min))
                      (text-property-search-forward 'loki-slack-message message t)))))
      (if (and anchor (not (invisible-p (prop-match-beginning anchor))))
          (goto-char (max (prop-match-beginning anchor) (- (prop-match-end anchor) offset)))
        (when-let ((position (loki-assistant-slack--message-position root)))
          (goto-char position))))
    (restore-buffer-modified-p modified)))

(defun loki-assistant-slack--submit ()
  "激活按钮、切换线程折叠状态或发送当前输入。"
  (interactive)
  (cond
   ((button-at (point)) (push-button))
   ((get-text-property (point) 'loki-slack-thread-toggle)
    (loki-assistant-slack--toggle-thread (get-text-property (point) 'loki-slack-thread-toggle)))
   (t
    (when (< (point) (loki-assistant-slack--draft-start))
      (user-error "请在消息输入区发送"))
    (when (and (or loki-assistant-slack--thread-root loki-assistant-slack--parent-buffer)
               (not (and loki-assistant-slack--thread-root
                         (equal (alist-get 'ts loki-assistant-slack--thread-message)
                                loki-assistant-slack--thread-root)
                         (buffer-live-p loki-assistant-slack--parent-buffer))))
      (user-error "线程不可用，请从 Slack 主聊天重新打开"))
    (when (plist-get loki-assistant-slack--send-request :busy)
      (user-error "正在等待 Slack 发送结果"))
    (let* ((text (buffer-substring-no-properties (loki-assistant-slack--draft-start) (point-max)))
           (previous loki-assistant-slack--send-request)
           (room loki-assistant-slack--room)
           (thread (or loki-assistant-slack--thread-root loki-assistant-slack--reply))
           (same-draft (and (equal text (plist-get previous :text))
                            (equal thread (plist-get previous :thread)))))
      (when (string-empty-p (string-trim text)) (user-error "请输入 Slack 消息"))
      (setq loki-assistant-slack--send-request
            (list :id (if same-draft (plist-get previous :id) (loki-assistant-slack--uuid))
                  :text text :thread thread :busy t))
      (let ((request loki-assistant-slack--send-request) (buffer (current-buffer)))
        (loki-assistant-slack--update-send-prompt buffer)
        (loki-assistant-slack--request
         "POST" (loki-assistant-slack--path room thread)
         (lambda (response) (loki-assistant-slack--sent buffer room request response 0))
         :room room :body (list (cons 'text text)) :key (plist-get request :id)))))))

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
            (loki-assistant-slack--update-send-prompt buffer)
            (let ((start (loki-assistant-slack--draft-start)))
              (when (and (equal (or loki-assistant-slack--thread-root loki-assistant-slack--reply)
                                (plist-get request :thread))
                         (equal (buffer-substring-no-properties start (point-max))
                                (plist-get request :text)))
                (delete-region start (point-max))
                (when loki-assistant-slack--reply
                  (setq loki-assistant-slack--reply nil)
                  (loki-assistant-slack--render))))
            (message (if (plist-get request :thread)
                         "Slack 线程回复已发送"
                       "Slack 消息已发送")))
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
            (loki-assistant-slack--update-send-prompt buffer)
            (when (or (equal state "failed")
                      (and (not ok) (zerop polls)
                           (integerp (plist-get response :status))
                           (<= 400 (plist-get response :status) 499)))
              (setq loki-assistant-slack--send-request nil))
            (message "%s" (or (alist-get 'message
                                         (loki-assistant-slack--non-null (alist-get 'error data)))
                              (plist-get response :error-message)
                              "发送结果待确认，草稿已保留。")))))))))

(defvar loki-assistant-slack-input-mode-map nil
  "Slack 聊天输入和历史导航的按键映射。")

(setq loki-assistant-slack-input-mode-map
  (let ((map (make-sparse-keymap)))
    (define-key map (kbd "n") #'loki-assistant-slack-next-item)
    (define-key map (kbd "p") #'loki-assistant-slack-previous-item)
    (define-key map (kbd "TAB") #'loki-assistant-slack-next-item)
    (define-key map (kbd "<tab>") #'loki-assistant-slack-next-item)
    (define-key map (kbd "<backtab>") #'loki-assistant-slack-previous-item)
    (define-key map [remap forward-button] #'loki-assistant-slack-next-item)
    (define-key map [remap backward-button] #'loki-assistant-slack-previous-item)
    (define-key map (kbd "RET") #'loki-assistant-slack--submit)
    (define-key map (kbd "<return>") #'loki-assistant-slack--submit)
    (define-key map (kbd "S-<return>") #'loki-assistant-slack--shift-return)
    (define-key map (kbd "q") #'loki-assistant-slack-quit-thread)
    (define-key map (kbd "C-c C-q") #'loki-assistant-slack-quit-thread)
    (define-key map (kbd "C-c C-r") #'loki-assistant-slack-refresh)
    (define-key map (kbd "C-c C-k") #'loki-assistant-slack--main-target)
    map))

(define-minor-mode loki-assistant-slack-input-mode
  "在 shell-maker 缓冲区中编辑和发送 Slack 消息。"
  :keymap loki-assistant-slack-input-mode-map)

(dolist (buffer (buffer-list))
  (when (loki-assistant-slack--chat-buffer-p buffer)
    (with-current-buffer buffer (loki-assistant-slack--update-history))))

(provide 'loki-assistant-slack-chat)
;;; loki-assistant-slack-chat.el ends here
