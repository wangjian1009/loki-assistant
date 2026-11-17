;;; loki-assistant-agent-shell-tests.el --- agent-shell 扩展测试  -*- lexical-binding: t; -*-

;;; Code:

(require 'cl-lib)
(require 'ert)
(require 'loki-assistant-agent-shell)

(ert-deftest loki-assistant-agent-shell-dropped-screenshot-preview ()
  "验证截图拖放与图片粘贴的预览、重名避让及会话项目缓存。"
  (skip-unless (display-graphic-p))
  (let* ((directory (make-temp-file "loki-assistant-screenshot-test-" t))
         (source-directory (expand-file-name "NSIRD_screencaptureui_test" directory))
         (source (expand-file-name "Shot.png" source-directory))
         (cache (expand-file-name "cache" directory))
         (shell-buffer (generate-new-buffer " *loki-assistant-screenshot-shell*"))
         (timestamp (encode-time 56 34 12 17 11 2026))
         (format-time (symbol-function 'format-time-string))
         (loki-assistant-agent-shell-file-copy-policy 'always-original)
         (png (base64-decode-string
               "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aS1sAAAAASUVORK5CYII="))
         inserted)
    (unwind-protect
        (progn
          (make-directory source-directory)
          (make-directory cache)
          (with-current-buffer shell-buffer
            (setq default-directory (file-name-as-directory directory)))
          (with-temp-file source
            (set-buffer-multibyte nil)
            (insert png))
          (cl-letf (((symbol-function 'format-time-string)
                     (lambda (format &optional time zone)
                       (funcall format-time format (or time timestamp) zone)))
                    ((symbol-function 'loki-assistant-agent-shell--data-dir)
                     (lambda () (expand-file-name "cache" default-directory)))
                    ((symbol-function 'agent-shell--shell-buffer)
                     (lambda (&rest _) shell-buffer))
                    ((symbol-function 'agent-shell-insert)
                     (lambda (&rest args)
                       (should (eq (plist-get args :shell-buffer) shell-buffer))
                       (push (plist-get args :text) inserted))))
            (with-temp-buffer
              (setq default-directory "/")
              (should (eq (loki-assistant-agent-shell--dnd-handler
                           (concat "file:" source) 'copy)
                          'private))
              (should (eq (loki-assistant-agent-shell--dnd-multi-handler
                           (list (concat "file://" source)) 'copy)
                          'private))
              (cl-letf (((symbol-function 'gui-get-selection)
                         (lambda (selection type)
                           (should (eq selection 'CLIPBOARD))
                           (when (eq type 'image/png) png))))
                (dolist (mode '(agent-shell-mode
                                agent-shell-viewport-edit-mode
                                agent-shell-viewport-view-mode))
                  (setq major-mode mode)
                  (let ((buffer-read-only (eq mode 'agent-shell-viewport-view-mode)))
                    (loki-assistant-agent-shell-yank-image))))))
          (delete-file source)
          (should (= (length inserted) 5))
          (cl-loop for text in (nreverse inserted)
                   for name in '("截图 - 2026-11-17 12-34-56.png"
                                 "截图 - 2026-11-17 12-34-56-1.png"
                                 "截图 - 2026-11-17 12-34-56-2.png"
                                 "截图 - 2026-11-17 12-34-56-3.png"
                                 "截图 - 2026-11-17 12-34-56-4.png")
                   for path = (expand-file-name name cache)
                   do (should (get-text-property 0 'agent-shell-context-image text))
                   do (should (eq (car (get-text-property 0 'display text)) 'image))
                   do (should (equal (image-size (get-text-property 0 'display text) t)
                                     '(1 . 1)))
                   do (should (equal (substring-no-properties text)
                                     (concat "@\"" path "\"")))
                   do (should (equal (with-temp-buffer
                                       (set-buffer-multibyte nil)
                                       (insert-file-contents-literally path)
                                       (buffer-string))
                                     png))))
      (kill-buffer shell-buffer)
      (delete-directory directory t))))

(ert-deftest loki-assistant-agent-shell-yank-image-without-image ()
  "验证剪贴板没有图片时不写缓存或插入文本，也不访问会话。"
  (with-temp-buffer
    (setq major-mode 'agent-shell-mode)
    (dolist (data '(nil "" "最终接口修改"))
      (cl-letf (((symbol-function 'gui-get-selection)
                 (lambda (&rest _) data))
                ((symbol-function 'agent-shell--shell-buffer)
                 (lambda (&rest _) (ert-fail "没有图片时不应访问会话")))
                ((symbol-function 'agent-shell-insert)
                 (lambda (&rest _) (ert-fail "没有图片时不应插入内容"))))
        (should-not (loki-assistant-agent-shell--clipboard-image))
        (should-error (loki-assistant-agent-shell-yank-image) :type 'user-error)))))

(ert-deftest loki-assistant-agent-shell-clipboard-image-fallback ()
  "验证 PNG 不可用时读取其他格式，并保留图片格式与原始字节。"
  (let ((bytes (unibyte-string ?I ?I 42 0 8 0 0 0)))
    (cl-letf (((symbol-function 'gui-get-selection)
               (lambda (_selection type)
                 (pcase type
                   ('image/png (error "不支持该剪贴板类型"))
                   ('image/tiff bytes)))))
      (should (equal (loki-assistant-agent-shell--clipboard-image)
                     (cons ".tiff" bytes))))))

(ert-deftest loki-assistant-agent-shell-screenshot-file-names ()
  "验证中英文截图文件名识别，普通图片和非图片保持原规则。"
  (dolist (name '("Screenshot 2026-11-17 at 12.34.56.png"
                  "Screen Shot 2026-11-17 at 12.34.56.png"
                  "截屏2026-11-17 12.34.56.png"
                  "截图 - 2026-11-17 12-34-56.png"
                  "屏幕快照 2026-11-17.png"
                  "螢幕截圖 2026-11-17.png"))
    (should (loki-assistant-agent-shell--screenshot-file-p name)))
  (should-not (loki-assistant-agent-shell--screenshot-file-p "design.png"))
  (should-not (loki-assistant-agent-shell--screenshot-file-p "截图说明.txt")))

(ert-deftest loki-assistant-agent-shell-viewport-dnd-hooks ()
  "验证 viewport 的编辑与查看模式接入拖放，重复设置不会叠加规则。"
  (require 'loki-assistant)
  (let ((agent-shell-mode-hook agent-shell-mode-hook)
        (agent-shell-viewport-edit-mode-hook agent-shell-viewport-edit-mode-hook)
        (agent-shell-viewport-view-mode-hook agent-shell-viewport-view-mode-hook)
        (loki-assistant-agent-shell-enabled t))
    (loki-assistant--setup-agent-shell)
    (dolist (hook '(agent-shell-viewport-edit-mode-hook
                    agent-shell-viewport-view-mode-hook))
      (should (memq #'loki-assistant-agent-shell--setup-dnd (symbol-value hook))))
    (with-temp-buffer
      (loki-assistant-agent-shell--setup-dnd)
      (let ((rules (copy-sequence dnd-protocol-alist)))
        (loki-assistant-agent-shell--setup-dnd)
        (should (equal rules dnd-protocol-alist))
        (should (eq (cdar rules)
                    (if (>= emacs-major-version 30)
                        #'loki-assistant-agent-shell--dnd-multi-handler
                      #'loki-assistant-agent-shell--dnd-handler)))))))

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
