;;; loki-assistant-log.el --- 浏览 Loki Assistant 日志 -*- lexical-binding: t; -*-

(require 'autorevert)
(require 'seq)

(declare-function logview-mode "logview")

;;;###autoload
(defun loki-assistant-logs (source)
  "在 Emacs 中打开 SOURCE 的日志。"
  (interactive
   (list (completing-read "日志来源：" '("services" "center") nil t nil nil "services")))
  (let* ((filename (pcase source
                     ("services" "services.log")
                     ("center" "center.log")
                     (_ (user-error "未知日志来源"))))
         (path (expand-file-name filename "~/.assistant/logs/"))
         (buffer (find-file-noselect path)))
    (with-current-buffer buffer
      (unless (derived-mode-p 'logview-mode)
        (when (require 'logview nil t)
          (logview-mode)))
      (setq buffer-read-only t)
      (auto-revert-mode 1))
    (pop-to-buffer-same-window buffer)
    (goto-char (point-max))
    buffer))

(defun loki-assistant-log-open-app ()
  "聚焦 Emacs 并打开 services 日志。"
  (let ((frame (or (and (display-graphic-p) (selected-frame))
                   (seq-find #'display-graphic-p (frame-list))
                   (make-frame '((window-system . ns))))))
    (select-frame frame)
    (loki-assistant-logs "services")
    (make-frame-visible frame)
    (select-frame-set-input-focus frame)
    t))

(provide 'loki-assistant-log)
;;; loki-assistant-log.el ends here
