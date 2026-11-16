;;; loki-assistant-agent-shell-tests.el --- agent-shell 扩展测试  -*- lexical-binding: t; -*-

;;; Code:

(require 'cl-lib)
(require 'ert)
(require 'loki-assistant-agent-shell)

(ert-deftest loki-assistant-agent-shell-setup-remaps-viewport-yank ()
  "验证 viewport 编辑缓冲区使用扩展后的 yank 命令。"
  (let ((shell-binding
         (lookup-key agent-shell-mode-map [remap yank]))
        (viewport-binding
         (lookup-key agent-shell-viewport-edit-mode-map [remap yank])))
    (unwind-protect
        (cl-letf (((symbol-function 'loki-assistant-agent-shell--setup-dnd)
                   #'ignore)
                  ((symbol-function 'loki-assistant-agent-shell--setup-notifications)
                   #'ignore))
          (loki-assistant-agent-shell-setup)
          (should
           (eq (lookup-key agent-shell-viewport-edit-mode-map [remap yank])
               #'loki-assistant-agent-shell-yank)))
      (define-key agent-shell-mode-map [remap yank] shell-binding)
      (define-key agent-shell-viewport-edit-mode-map [remap yank]
                  viewport-binding))))

(ert-deftest loki-assistant-agent-shell-notify-uses-loki-assistant ()
  "验证 Agent 事件通知通过 Loki Assistant 发送。"
  (let (arguments)
    (cl-letf (((symbol-function 'loki-assistant-notify)
               (lambda (&rest args)
                 (setq arguments args))))
      (with-temp-buffer
        (loki-assistant-agent-shell--notify (current-buffer) "Agent" "Finished")))
    (should (equal (nth 1 arguments) "Agent"))
    (should (equal (plist-get (cddr arguments) :body) "Finished"))
    (should (equal (plist-get (cddr arguments) :module-id) "emacs"))
    (should (functionp (plist-get (cddr arguments) :callback)))))

(provide 'loki-assistant-agent-shell-tests)

;;; loki-assistant-agent-shell-tests.el ends here
