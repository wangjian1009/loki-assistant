;;; loki-assistant-mail.el --- 邮件服务的 Emacs 入口 -*- lexical-binding: t; -*-

(require 'json)
(require 'seq)
(require 'subr-x)

(declare-function loki-assistant-request "loki-assistant")
(declare-function mu4e--init-handlers "mu4e")
(declare-function mu4e--start "mu4e")
(declare-function mu4e--server-view "mu4e-server")
(declare-function mu4e-bookmarks "mu4e-bookmarks")
(declare-function mu4e-compose-mail "mu4e-compose")
(declare-function mu4e-database-path "mu4e-server")
(declare-function mu4e-get-headers-buffer "mu4e-window")
(declare-function mu4e-headers-mode "mu4e-headers")
(declare-function mu4e-running-p "mu4e-server")
(declare-function mu4e-search-bookmark "mu4e-search")
(declare-function mu4e-update-index-nonlazy "mu4e-update")
(declare-function message-goto-body "message")
(declare-function mml-quote-region "mml")
(defvar mu4e--started)
(defvar mu4e--server-indexing)
(defvar mu4e--server-process)
(defvar mu4e-index-update-status)
(defvar mu4e-info-func)
(defvar mu4e-remove-func)
(defvar mu4e-update-func)
(defvar mu4e-view-auto-mark-as-read)
(defvar mu4e-view-func)

(defcustom loki-assistant-mail-inbox-bookmark-key ?i
  "托盘打开收件箱时使用的 mu4e 书签快捷键。"
  :type 'character
  :group 'loki-assistant)

(defvar loki-assistant-mail--index-request nil
  "最近一次 services 索引请求的标识、mu 进程及发起前的完成状态。")

(defvar loki-assistant-mail--index-info nil
  "最近一次 services 索引请求收到的 mu 进度消息。")

(defvar loki-assistant-mail--refresh-timer nil
  "合并连续邮件变更回执的定时器。")

(defun loki-assistant-mail-database (&optional index-info)
  "返回当前 mu4e 数据库位置和 mu 可执行文件的 JSON。
INDEX-INFO 非空时附带索引计数。"
  (let ((database (mu4e-database-path)))
    (json-serialize `((program . ,(car (process-command mu4e--server-process)))
                      (home . ,(directory-file-name (file-name-directory database)))
                      ,@(when index-info
                          `((index . ,(loki-assistant-mail--index-counts index-info))))))))

(defun loki-assistant-mail--index-counts (info)
  "将 mu 索引回执 INFO 中的计数转为 JSON 对象可用的 alist。"
  `((checked . ,(plist-get info :checked))
    (updated . ,(plist-get info :updated))
    ,@(when-let* ((removed (plist-get info :cleaned-up)))
        `((cleaned_up . ,removed)))))

(defun loki-assistant-mail--observe-index (info)
  "记录当前 services 索引对应的 mu 回执 INFO。"
  (pcase-let ((`(,_ ,process ,previous-status) loki-assistant-mail--index-request))
    (when (and process (eq process mu4e--server-process)
               (eq previous-status mu4e-index-update-status)
               (eq (plist-get info :info) 'index)
               (numberp (plist-get info :checked))
               (numberp (plist-get info :updated)))
      (setq loki-assistant-mail--index-info info))))

(defun loki-assistant-mail--request-refresh ()
  "异步通知 services 刷新本机邮件状态。"
  (setq loki-assistant-mail--refresh-timer nil)
  (require 'loki-assistant)
  (loki-assistant-request
   "POST" "/mail/status/refresh"
   :callback (lambda (response)
               (unless (plist-get response :ok)
                 (message "请求刷新邮件状态失败：%s"
                          (plist-get response :error-message))))))

(defun loki-assistant-mail--schedule-refresh (&rest _)
  "将 mu4e 的成功回执合并为一次邮件状态刷新。"
  (unless loki-assistant-mail--refresh-timer
    (setq loki-assistant-mail--refresh-timer
          (run-at-time 0.2 nil #'loki-assistant-mail--request-refresh))))

(defun loki-assistant-mail--index-updated ()
  "在 mu4e 索引有变更时请求刷新邮件状态。"
  (when (> (+ (plist-get mu4e-index-update-status :updated)
              (plist-get mu4e-index-update-status :cleaned-up))
           0)
    (loki-assistant-mail--schedule-refresh)))

(defun loki-assistant-mail--observe-replies ()
  "在 mu4e 的邮件操作回执处理完成后安排刷新。"
  (when (default-value 'mu4e-info-func)
    (add-function :before (default-value 'mu4e-info-func)
                  #'loki-assistant-mail--observe-index))
  ;; 打开邮件的回执也可能包含打开时自动标记已读的结果。
  (dolist (handler '(mu4e-update-func mu4e-remove-func mu4e-view-func))
    (when (default-value handler)
      (add-function :after (default-value handler)
                    #'loki-assistant-mail--schedule-refresh))))

(with-eval-after-load 'mu4e
  (advice-add 'mu4e--init-handlers :after #'loki-assistant-mail--observe-replies)
  (loki-assistant-mail--observe-replies)
  (add-hook 'mu4e-index-updated-hook #'loki-assistant-mail--index-updated))

(defun loki-assistant-mail-index (request-id check-only)
  "在已有的 mu4e 进程中发起或查询 REQUEST-ID 对应的索引更新。
CHECK-ONLY 非空时只查询已发起的索引。"
  (pcase-let ((`(,id ,process ,previous-status) loki-assistant-mail--index-request))
    (cond
     ((equal request-id id)
      (unless (and (eq process mu4e--server-process) (mu4e-running-p))
        (error "mu4e 进程已退出，无法确认索引结果"))
      (if (eq previous-status mu4e-index-update-status)
          'running
        (loki-assistant-mail-database mu4e-index-update-status)))
     (check-only (error "找不到此前发起的 mu4e 索引请求"))
     (mu4e--server-indexing 'busy)
     (t
      (let ((status mu4e-index-update-status))
        (setq loki-assistant-mail--index-info nil)
        (mu4e-update-index-nonlazy)
        (setq loki-assistant-mail--index-request (list request-id mu4e--server-process status))
        'running)))))

(defun loki-assistant-mail-index-progress (request-id check-only)
  "发起或查询 REQUEST-ID，并在运行期间返回已收到的索引计数。
CHECK-ONLY 非空时只查询已发起的索引。"
  (let ((result (loki-assistant-mail-index request-id check-only)))
    (if (and (eq result 'running) loki-assistant-mail--index-info)
        (json-serialize (loki-assistant-mail--index-counts loki-assistant-mail--index-info))
      result)))

(defun loki-assistant-mail--focus ()
  "选中可用于显示邮件的图形 frame。"
  (let ((frame (or (and (display-graphic-p) (selected-frame))
                   (seq-find #'display-graphic-p (frame-list))
                   (make-frame '((window-system . ns))))))
    (select-frame frame)
    (make-frame-visible frame)
    (select-frame-set-input-focus frame)
    frame))

(defun loki-assistant-mail--compose (payload)
  "使用 PAYLOAD 打开 mu4e 草稿。"
  (let* ((cc (alist-get 'cc payload))
         (bcc (alist-get 'bcc payload))
         (body (alist-get 'body payload))
         (headers (delq nil (list (and cc (cons "Cc" cc))
                                  (and bcc (cons "Bcc" bcc)))))
         (buffer (mu4e-compose-mail (alist-get 'to payload)
                                    (alist-get 'subject payload)
                                    headers)))
    (when body
      (require 'mml)
      (with-current-buffer buffer
        (save-excursion
          (message-goto-body)
          (let ((start (point)))
            (insert body "\n")
            (mml-quote-region start (point))))))))

(defun loki-assistant-mail--open-inbox ()
  "用 mu4e 当前配置打开收件箱书签。"
  (let* ((key loki-assistant-mail-inbox-bookmark-key)
         (bookmark (seq-find (lambda (item) (eql (plist-get item :key) key))
                             (mu4e-bookmarks)))
         (query (plist-get bookmark :query))
         (query (if (functionp query) (funcall query) query)))
    (unless (and (stringp query) (not (string-blank-p query)))
      (user-error "mu4e 未配置快捷键 %s 对应的收件箱书签" (key-description (vector key))))
    (mu4e-search-bookmark query)))

(defun loki-assistant-mail--open (operation &rest args)
  "启动 mu4e 后在图形 frame 中以 ARGS 执行 OPERATION。"
  (unless (require 'mu4e nil t)
    (user-error "未安装 mu4e"))
  (let* ((frame (loki-assistant-mail--focus))
         (open (lambda ()
                 (condition-case err
                     (when (frame-live-p frame)
                       (with-selected-frame frame
                         (apply operation args)))
                   (error (message "邮件操作失败：%s" (error-message-string err)))))))
    (if (and (mu4e-running-p) mu4e--started)
        (funcall open)
      (mu4e--init-handlers)
      (mu4e--start open))))

(defun loki-assistant-mail--view-message (message-id)
  "在 mu4e 中显示 MESSAGE-ID 对应的邮件。"
  ;; 首次打开正文时，mu4e 需要关联的邮件列表缓冲区。
  (with-current-buffer (mu4e-get-headers-buffer nil t)
    (unless (derived-mode-p 'mu4e-headers-mode)
      (mu4e-headers-mode)))
  (mu4e--server-view message-id (eq mu4e-view-auto-mark-as-read t)))

(defun loki-assistant-mail-open-message (message-id)
  "按 MESSAGE-ID 打开 mu4e 中的邮件。"
  (interactive (list (read-string "邮件 Message-ID：")))
  (unless (and (stringp message-id)
               (not (string-blank-p message-id))
               (not (string-match-p "[[:cntrl:]]" message-id)))
    (user-error "需要提供有效的 Message-ID"))
  (loki-assistant-mail--open #'loki-assistant-mail--view-message message-id))

(defun loki-assistant-mail-open-inbox ()
  "打开 mu4e 中配置的收件箱书签。"
  (interactive)
  (loki-assistant-mail--open #'loki-assistant-mail--open-inbox))

(defun loki-assistant-mail-dispatch (action encoded-payload)
  "从 services 执行邮件 ACTION，ENCODED-PAYLOAD 为 Base64 JSON。"
  (let ((payload (json-parse-string
                  (decode-coding-string (base64-decode-string encoded-payload) 'utf-8)
                  :object-type 'alist)))
    (pcase action
      ("config"
       (loki-assistant-mail--focus)
       (find-file payload)
       t)
      ((or "view" "inbox" "compose")
       (when (require 'mu4e nil t)
         ;; 账户选择可等待用户输入，先结束 emacsclient 的本次求值。
         (pcase action
           ("view" (run-at-time 0 nil #'loki-assistant-mail-open-message payload))
           ("inbox" (run-at-time 0 nil #'loki-assistant-mail-open-inbox))
           ("compose" (run-at-time 0 nil #'loki-assistant-mail--open
                                  #'loki-assistant-mail--compose payload)))
         t))
      (_ (error "未知的邮件操作")))))

(provide 'loki-assistant-mail)
;;; loki-assistant-mail.el ends here
