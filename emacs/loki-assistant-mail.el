;;; loki-assistant-mail.el --- 邮件服务的 Emacs 入口 -*- lexical-binding: t; -*-

(require 'json)
(require 'seq)

(declare-function mu4e--start "mu4e")
(declare-function mu4e--server-view "mu4e-server")
(declare-function mu4e-compose-mail "mu4e-compose")
(declare-function mu4e-get-headers-buffer "mu4e-window")
(declare-function mu4e-headers-mode "mu4e-headers")
(declare-function mu4e-running-p "mu4e-server")
(declare-function mu4e-search "mu4e-search")
(declare-function message-goto-body "message")
(declare-function mml-quote-region "mml")
(defvar mu4e--started)
(defvar mu4e-search-full)
(defvar mu4e-search-include-related)
(defvar mu4e-search-skip-duplicates)
(defvar mu4e-view-auto-mark-as-read)

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

(defun loki-assistant-mail--open (action payload)
  "在图形 frame 中执行邮件 ACTION。"
  (let* ((frame (loki-assistant-mail--focus))
         (open (lambda ()
                 (condition-case err
                     (when (frame-live-p frame)
                       (with-selected-frame frame
                         (pcase action
                           ("view"
                            ;; 首次打开正文时，mu4e 需要关联的邮件列表缓冲区。
                            (with-current-buffer (mu4e-get-headers-buffer nil t)
                              (unless (derived-mode-p 'mu4e-headers-mode)
                                (mu4e-headers-mode)))
                            (mu4e--server-view payload (eq mu4e-view-auto-mark-as-read t)))
                           ("unread"
                            (let ((mu4e-search-full t)
                                  (mu4e-search-include-related nil)
                                  (mu4e-search-skip-duplicates t))
                              (mu4e-search payload)))
                           ("compose" (loki-assistant-mail--compose payload)))))
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
      ("config"
       (loki-assistant-mail--focus)
       (find-file payload)
       t)
      ((or "view" "unread" "compose")
       (when (require 'mu4e nil t)
         ;; 账户选择可等待用户输入，先结束 emacsclient 的本次求值。
         (run-at-time 0 nil #'loki-assistant-mail--open action payload)
         t))
      (_ (error "未知的邮件操作")))))

(provide 'loki-assistant-mail)
;;; loki-assistant-mail.el ends here
