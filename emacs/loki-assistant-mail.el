;;; loki-assistant-mail.el --- 邮件服务的 Emacs 入口 -*- lexical-binding: t; -*-

(require 'json)
(require 'seq)

(declare-function mu4e--server-view "mu4e-server")
(declare-function mu4e--start "mu4e")
(declare-function mu4e-compose-mail "mu4e-compose")
(declare-function mu4e-running-p "mu4e-server")
(declare-function mu4e-update-index-nonlazy "mu4e-update")
(declare-function message-goto-body "message")
(declare-function mml-quote-region "mml")
(defvar mu4e--started)
(defvar mu4e--server-indexing)
(defvar mu4e-mu-home)

(defun loki-assistant-mail--focus ()
  "选中可用于显示邮件的图形 frame。"
  (let ((frame (or (and (display-graphic-p) (selected-frame))
                   (seq-find #'display-graphic-p (frame-list))
                   (make-frame '((window-system . ns))))))
    (select-frame frame)
    (make-frame-visible frame)
    (select-frame-set-input-focus frame)
    frame))

(defun loki-assistant-mail--index (payload)
  "按 PAYLOAD 请求或查询当前 mu4e 进程的索引。"
  (let ((home (alist-get 'home payload))
        (phase (alist-get 'phase payload)))
    (cond
     ((not (and (featurep 'mu4e)
                (mu4e-running-p)
                (file-equal-p home
                              (or mu4e-mu-home (getenv "MUHOME")
                                  (expand-file-name "mu" (or (getenv "XDG_CACHE_HOME")
                                                             "~/.cache"))))))
      'unavailable)
     (mu4e--server-indexing 'busy)
     ((equal phase "start")
      (mu4e-update-index-nonlazy)
      'done)
     (t 'done))))

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

(defun loki-assistant-mail--open (action payload)
  "在图形 frame 中执行邮件 ACTION。"
  (let* ((frame (loki-assistant-mail--focus))
         (open (lambda ()
                 (condition-case err
                     (when (frame-live-p frame)
                       (with-selected-frame frame
                         (if (equal action "view")
                             (mu4e--server-view payload)
                           (loki-assistant-mail--compose payload))))
                   (error (message "邮件操作失败：%s" (error-message-string err)))))))
    (if (and (mu4e-running-p) mu4e--started)
        (funcall open)
      (mu4e--start open))))

(defun loki-assistant-mail-dispatch (action encoded-payload)
  "从 services 执行邮件 ACTION，ENCODED-PAYLOAD 为 Base64 JSON。"
  (let ((payload (json-parse-string
                  (decode-coding-string (base64-decode-string encoded-payload) 'utf-8)
                  :object-type 'alist)))
    (pcase action
      ("index" (loki-assistant-mail--index payload))
      ("config"
       (loki-assistant-mail--focus)
       (find-file payload)
       t)
      ((or "view" "compose")
       (when (require 'mu4e nil t)
         ;; 账户选择可等待用户输入，先结束 emacsclient 的本次求值。
         (run-at-time 0 nil #'loki-assistant-mail--open action payload)
         t))
      (_ (error "未知的邮件操作")))))

(provide 'loki-assistant-mail)
;;; loki-assistant-mail.el ends here
