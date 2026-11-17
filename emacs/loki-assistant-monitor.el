;;; loki-assistant-monitor.el --- Emacs 状态主动上报与 buffer 关注 -*- lexical-binding: t; -*-

;;; Commentary:

;; 由主包加载；mode hook 维护关注集合，定时向 services 同步 app/数据项快照。

;;; Code:

(require 'cl-lib)
(require 'json)
(require 'subr-x)
(declare-function loki-assistant-request "loki-assistant")

(defvar loki-assistant-monitor-mode nil)
(defvar loki-assistant-monitor--buffers (make-hash-table :test #'eq))
(cl-defstruct (loki-assistant-status-store
               (:constructor loki-assistant-status-store-create))
  "所有待上报 app/数据项的结构化状态。"
  values)

(defvar loki-assistant-status--store
  (loki-assistant-status-store-create
   :values (make-hash-table :test #'equal)))

(defcustom loki-assistant-status-sync-delay 1
  "状态变化后延迟触发同步的秒数。
延迟期间的多次变化只保留最后一次同步请求。"
  :type 'number
  :group 'loki-assistant)

(defvar loki-assistant-monitor--hooks nil)
(defvar loki-assistant-monitor--timer nil)
(defvar loki-assistant-monitor--pending-timer nil)
(defvar loki-assistant-monitor--inflight nil)
(defvar loki-assistant-monitor--dirty nil)
(defvar loki-assistant-monitor--epoch nil)
(defvar loki-assistant-monitor--last-error nil)
(defvar loki-assistant-monitor--instance
  (secure-hash 'sha256 (format "%s:%s:%s" (emacs-pid) (current-time) (random))))
(defvar loki-assistant-monitor--counter 0)
(defvar-local loki-assistant-monitor--buffer-id nil)

(defun loki-assistant-monitor--set-buffers (symbol value)
  "设置 SYMBOL 为 mode 列表 VALUE，并更新已经安装的 hook。"
  (unless (and (listp value) (cl-every #'symbolp value))
    (error "关注 buffer 配置必须是 mode 符号列表"))
  (set-default symbol value)
  (when loki-assistant-monitor-mode
    (loki-assistant-monitor--configure)))

(defun loki-assistant-monitor--set-interval (symbol value)
  "设置 SYMBOL 为正数秒 VALUE，必要时重新启动定时器。"
  (unless (and (numberp value) (> value 0)) (error "上报间隔必须为正数"))
  (set-default symbol value)
  (when loki-assistant-monitor-mode
    (loki-assistant-monitor--start-timer)))

(defcustom loki-assistant-monitor-buffer '(agent-shell-mode)
  "需要关注的 major mode 列表；对应 mode 的派生 mode 也会被关注。
通过 mode hook 注册 buffer，不依赖 agent-shell 的内部状态。"
  :type '(repeat symbol)
  :set #'loki-assistant-monitor--set-buffers
  :group 'loki-assistant)

(defcustom loki-assistant-monitor-interval 5
  "Emacs 状态上报间隔，单位为秒。
发生注册、注销或状态变更时也会合并触发同步。"
  :type 'number
  :set #'loki-assistant-monitor--set-interval
  :group 'loki-assistant)

(defun loki-assistant-monitor--matches-p (buffer)
  "BUFFER 的 mode 是否在关注范围内。"
  (and (buffer-live-p buffer)
       (with-current-buffer buffer
         (cl-some #'derived-mode-p loki-assistant-monitor-buffer))))

(defun loki-assistant-monitor-register-buffer (&optional buffer)
  "将 BUFFER（默认当前 buffer）注册到统一关注集合并触发同步。"
  (setq buffer (or buffer (current-buffer)))
  (when (and loki-assistant-monitor-mode
             (loki-assistant-monitor--matches-p buffer))
    (with-current-buffer buffer
      (unless loki-assistant-monitor--buffer-id
        (setq loki-assistant-monitor--buffer-id
              (format "%s:%d" loki-assistant-monitor--instance
                      (cl-incf loki-assistant-monitor--counter))))
      (puthash buffer loki-assistant-monitor--buffer-id loki-assistant-monitor--buffers)
      (add-hook 'kill-buffer-hook #'loki-assistant-monitor-unregister-buffer nil t)
      (add-hook 'change-major-mode-hook #'loki-assistant-monitor-unregister-buffer nil t))
    (loki-assistant-monitor--queue)))

(defun loki-assistant-monitor-unregister-buffer (&optional buffer)
  "从关注集合注销 BUFFER（默认当前 buffer），并触发同步。"
  (setq buffer (or buffer (current-buffer)))
  (when (gethash buffer loki-assistant-monitor--buffers)
    (remhash buffer loki-assistant-monitor--buffers)
    (when (buffer-live-p buffer)
      (with-current-buffer buffer
        (remove-hook 'kill-buffer-hook #'loki-assistant-monitor-unregister-buffer t)
        (remove-hook 'change-major-mode-hook #'loki-assistant-monitor-unregister-buffer t)))
    (loki-assistant-monitor--queue)))

(defun loki-assistant-monitor-buffer-by-id (id)
  "返回仍受关注的 ID 对应 buffer，失效时返回 nil。
重命名保持 ID；关闭后新建的同名 buffer 不会匹配旧 ID。"
  (when loki-assistant-monitor-mode
    (let (found)
      (maphash (lambda (buffer token)
                 (when (and (equal token id)
                            (loki-assistant-monitor--matches-p buffer))
                   (setq found buffer)))
               loki-assistant-monitor--buffers)
      found)))

(defun loki-assistant-monitor--configure ()
  "按配置安装 mode hook，兼容加载前已存在的 buffer。"
  (dolist (hook loki-assistant-monitor--hooks)
    (remove-hook hook #'loki-assistant-monitor-register-buffer))
  (setq loki-assistant-monitor--hooks
        (mapcar (lambda (mode) (intern (concat (symbol-name mode) "-hook")))
                (delete-dups (copy-sequence loki-assistant-monitor-buffer))))
  (dolist (hook loki-assistant-monitor--hooks)
    (add-hook hook #'loki-assistant-monitor-register-buffer))
  (let (removed)
    (maphash (lambda (buffer _id)
               (unless (loki-assistant-monitor--matches-p buffer) (push buffer removed)))
             loki-assistant-monitor--buffers)
    (mapc #'loki-assistant-monitor-unregister-buffer removed))
  ;; 仅启用和配置更新时扫描；定时上报只遍历已注册集合。
  (dolist (buffer (buffer-list))
    (when (loki-assistant-monitor--matches-p buffer)
      (loki-assistant-monitor-register-buffer buffer)))
  (loki-assistant-monitor--queue))

(defun loki-assistant-monitor--snapshot-buffers ()
  "返回关注 buffer 的 JSON 数组快照，并清理失效项。"
  (let (entries removed)
    (maphash
     (lambda (buffer id)
       (if (not (loki-assistant-monitor--matches-p buffer)) (push buffer removed)
         (with-current-buffer buffer
           (push `((id . ,id) (name . ,(buffer-name))
                   (mode . ,(symbol-name major-mode))
                   (directory . ,default-directory)
                   (file . ,(or buffer-file-name :null))
                   (modified . ,(if (buffer-modified-p) t :false)))
                 entries))))
     loki-assistant-monitor--buffers)
    (dolist (buffer removed) (remhash buffer loki-assistant-monitor--buffers))
    (vconcat (sort entries (lambda (a b) (string< (alist-get 'id a) (alist-get 'id b)))))))

(defun loki-assistant-status--name (value label)
  "将 VALUE 转为 app 或数据项名称；LABEL 用于错误说明。"
  (let ((name (cond ((symbolp value) (symbol-name value))
                    ((stringp value) value)
                    (t nil))))
    (unless (and name (not (string-empty-p (string-trim name)))
                 (not (string-match-p "/" name))
                 (not (member name '("." ".."))))
      (user-error "%s 必须是非空 symbol 或 string，不能包含 /" label))
    name))

(defun loki-assistant-status--value-json (value)
  "验证 VALUE 可以编码为 JSON，并返回 VALUE。"
  (let ((json-null :null) (json-false :false))
    (condition-case error
        (progn (json-encode value) value)
      (error (user-error "状态值无法编码为 JSON：%s" (error-message-string error))))))

(defun loki-assistant-status--items (app &optional create)
  "返回 APP 的数据项 hash table；CREATE 非 nil 时创建。"
  (let* ((values (loki-assistant-status-store-values loki-assistant-status--store))
         (items (gethash app values)))
    (if (or items create)
        (or items (puthash app (make-hash-table :test #'equal) values))
      nil)))

(defun loki-assistant-status--set-internal (app key value)
  "内部设置 APP/KEY，不触发同步；VALUE 为 nil 时删除。"
  (let ((items (loki-assistant-status--items app (not (null value)))))
    (when items
      (if (null value)
          (progn
            (remhash key items)
            (when (= (hash-table-count items) 0)
              (remhash app (loki-assistant-status-store-values loki-assistant-status--store))))
        (puthash key value items)))))

(defun loki-assistant-status-get-value (app key &optional default)
  "读取 APP/KEY 的状态值；未设置时返回 DEFAULT。"
  (let* ((app-name (loki-assistant-status--name app "app"))
         (key-name (loki-assistant-status--name key "数据项"))
         (items (loki-assistant-status--items app-name)))
    (if (and items (gethash key-name items))
        (gethash key-name items)
      default)))

(defun loki-assistant-status-set-value (app key value)
  "设置 APP/KEY 为 VALUE，并延迟触发一次状态同步。
APP 和 KEY 可以是 symbol 或 string。VALUE 支持 JSON scalar、list、vector、alist、hash table、`:null' 和 `:false'；VALUE 为 nil 时删除数据项。
例如：`(loki-assistant-status-set-value 'emacs 'key1 'value1)'。"
  (let ((app-name (loki-assistant-status--name app "app"))
        (key-name (loki-assistant-status--name key "数据项")))
    (unless (null value) (setq value (loki-assistant-status--value-json value)))
    (loki-assistant-status--set-internal app-name key-name
                                          (and (not (null value)) value))
    (loki-assistant-status-trigger-sync)
    value))

(defun loki-assistant-status-update-value (app key function &rest args)
  "用 FUNCTION 更新 APP/KEY；FUNCTION 返回 nil 时删除数据项。
FUNCTION 的第一个参数是当前值，后续参数为 ARGS；可用于 list/vector 的追加、删除或替换。"
  (unless (functionp function) (user-error "更新函数必须可调用"))
  (let* ((app-name (loki-assistant-status--name app "app"))
         (key-name (loki-assistant-status--name key "数据项"))
         (current (loki-assistant-status-get-value app-name key-name))
         (value (apply function current args)))
    (loki-assistant-status-set-value app-name key-name value)))

(defun loki-assistant-status-trigger-sync ()
  "延迟触发 `loki-assistant-status-sync'。
每次调用都会重新计时；延迟期间的多次调用合并为最后一次同步。若已有请求在途，则在该请求完成后再同步一次。"
  (interactive)
  (setq loki-assistant-monitor--dirty t)
  (when loki-assistant-monitor--pending-timer
    (cancel-timer loki-assistant-monitor--pending-timer)
    (setq loki-assistant-monitor--pending-timer nil))
  (when (and loki-assistant-monitor-mode
             (not loki-assistant-monitor--inflight))
    (setq loki-assistant-monitor--pending-timer
          (run-at-time loki-assistant-status-sync-delay nil
                       #'loki-assistant-status-sync))))

(defun loki-assistant-monitor--queue ()
  "兼容旧内部调用，统一转到状态同步触发器。"
  (loki-assistant-status-trigger-sync))

(defun loki-assistant-status-sync ()
  "异步将 app/数据项快照同步到 services；同一时刻最多一个请求。
失败时保留数据，等待下次定时上报，不立即循环重试。"
  (interactive)
  (when loki-assistant-monitor--pending-timer
    (cancel-timer loki-assistant-monitor--pending-timer)
    (setq loki-assistant-monitor--pending-timer nil))
  (when loki-assistant-monitor-mode
    (if loki-assistant-monitor--inflight
        (setq loki-assistant-monitor--dirty t)
      (loki-assistant-status--set-internal
       "emacs" "runtime"
       `((pid . ,(emacs-pid)) (version . ,emacs-version)
         (started_at . ,(floor (float-time (or before-init-time (current-time)))))
         (reported_at . ,(floor (float-time)))))
      (loki-assistant-status--set-internal
       "emacs" "buffer" (loki-assistant-monitor--snapshot-buffers))
      (let ((epoch loki-assistant-monitor--epoch)
            (json-null :null) (json-false :false))
        (setq loki-assistant-monitor--inflight t loki-assistant-monitor--dirty nil)
        (condition-case err
            (loki-assistant-request
             'post "/emacs/status" :body (json-encode (loki-assistant-status-store-values loki-assistant-status--store))
             :callback
             (lambda (response)
               (when (eq epoch loki-assistant-monitor--epoch)
                 (setq loki-assistant-monitor--inflight nil)
                 (if (plist-get response :ok)
                     (progn
                       (setq loki-assistant-monitor--last-error nil)
                       (when loki-assistant-monitor--dirty (loki-assistant-monitor--queue)))
                   (loki-assistant-monitor--report-error
                    (or (plist-get response :error-message) "状态上报失败"))))))
          (error
           (setq loki-assistant-monitor--inflight nil)
           (loki-assistant-monitor--report-error (error-message-string err))))))))

(defun loki-assistant-monitor--report-error (text)
  "记录 TEXT，相同错误不重复打扰用户。"
  (unless (equal text loki-assistant-monitor--last-error)
    (message "Emacs 状态同步失败：%s" text))
  (setq loki-assistant-monitor--last-error text))

(defun loki-assistant-monitor--start-timer ()
  "启动或更新周期上报定时器。"
  (when loki-assistant-monitor--timer (cancel-timer loki-assistant-monitor--timer))
  (setq loki-assistant-monitor--timer
        (run-at-time loki-assistant-monitor-interval loki-assistant-monitor-interval
                     #'loki-assistant-status-sync)))

(define-minor-mode loki-assistant-monitor-mode
  "主动上报 Emacs 状态与配置关注的 buffer。
关闭后停止上报并移除 hook，services 保留最后收到的数据。"
  :global t :group 'loki-assistant
  (setq loki-assistant-monitor--epoch (list nil)
        loki-assistant-monitor--inflight nil)
  (if loki-assistant-monitor-mode
      (progn (loki-assistant-monitor--configure) (loki-assistant-monitor--start-timer))
    (when loki-assistant-monitor--timer (cancel-timer loki-assistant-monitor--timer))
    (when loki-assistant-monitor--pending-timer (cancel-timer loki-assistant-monitor--pending-timer))
    (setq loki-assistant-monitor--timer nil loki-assistant-monitor--pending-timer nil)
    (dolist (hook loki-assistant-monitor--hooks)
      (remove-hook hook #'loki-assistant-monitor-register-buffer))
    (let (buffers)
      (maphash (lambda (buffer _id) (push buffer buffers)) loki-assistant-monitor--buffers)
      (mapc #'loki-assistant-monitor-unregister-buffer buffers))
    (setq loki-assistant-monitor--hooks nil)))

(defun loki-assistant-monitor-unload-function ()
  "卸载 feature 前清理定时器与 hook。"
  (loki-assistant-monitor-mode -1)
  nil)

(provide 'loki-assistant-monitor)
;;; loki-assistant-monitor.el ends here
