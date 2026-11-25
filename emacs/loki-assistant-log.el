;;; loki-assistant-log.el --- 浏览 Loki Assistant 日志 -*- lexical-binding: t; -*-

(require 'autorevert)
(require 'loki-assistant)
(require 'seq)

(declare-function logview-mode "logview")

(defvar loki-assistant-log-mode-map
  (let ((map (make-sparse-keymap)))
    (define-key map (kbd "C-c C-q") #'quit-window)
    (define-key map (kbd "?") #'describe-mode)
    (define-key map (kbd "C-c C-r") #'revert-buffer)
    map))

(define-minor-mode loki-assistant-log-mode
  "浏览 Loki Assistant 日志。

按 C-c C-r 重读日志，C-c C-q 退出，? 查看模式帮助。
Logview 识别日志格式后，按 n 或 p 导航，按 l 后选择日志级别。"
  :keymap loki-assistant-log-mode-map
  (if loki-assistant-log-mode
      (setq-local mode-line-process
                  '(:eval
                    (loki-assistant--mode-line-keys
                     (cond
                      ((eq (key-binding (kbd "n")) 'logview-next-entry)
                       "C-c C-r 重读  n/p 导航  l 级别筛选")
                      ((derived-mode-p 'logview-mode)
                       "C-c C-r 重读  C-c C-c 选择日志格式")
                      (t "C-c C-r 重读")))))
    (kill-local-variable 'mode-line-process)))

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
      (read-only-mode 1)
      (auto-revert-mode 1)
      (loki-assistant-log-mode 1))
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
