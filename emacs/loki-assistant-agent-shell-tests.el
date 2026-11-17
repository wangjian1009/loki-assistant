;;; loki-assistant-agent-shell-tests.el --- agent-shell 扩展测试  -*- lexical-binding: t; -*-

;;; Code:

(require 'cl-lib)
(require 'ert)
(require 'loki-assistant-agent-shell)

(ert-deftest loki-assistant-agent-shell-insert-files-preserves-cached-images ()
  "验证重复插入同名图片时使用递增序号，并保留已有缓存内容。"
  (let* ((directory (make-temp-file "loki-assistant-image-test-" t))
         (source (expand-file-name "image.png" directory))
         (cache (expand-file-name "cache" directory))
         (loki-assistant-agent-shell-file-copy-policy 'always-copy)
         inserted)
    (unwind-protect
        (progn
          (make-directory cache)
          (cl-letf (((symbol-function 'loki-assistant-agent-shell--data-dir)
                     (lambda () cache))
                    ((symbol-function 'agent-shell--shell-buffer)
                     #'current-buffer)
                    ((symbol-function 'agent-shell--get-files-context)
                     (lambda (&rest args) (plist-get args :files)))
                    ((symbol-function 'agent-shell-insert)
                     (lambda (&rest args)
                       (push (plist-get args :text) inserted))))
            (dotimes (number 3)
              (with-temp-file source
                (insert (format "image %d" number)))
              (loki-assistant-agent-shell--insert-files (list source))))
          (should (equal (nreverse inserted)
                         (mapcar (lambda (name)
                                   (list (expand-file-name name cache)))
                                 '("image.png" "image-1.png" "image-2.png"))))
          (cl-loop for name in '("image.png" "image-1.png" "image-2.png")
                   for number from 0
                   do (should (equal (with-temp-buffer
                                       (insert-file-contents
                                        (expand-file-name name cache))
                                       (buffer-string))
                                     (format "image %d" number)))))
      (delete-directory directory t))))

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
