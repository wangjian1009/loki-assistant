;;; loki-assistant-daily.el --- 通过 Org-roam 模板保存日记条目 -*- lexical-binding: t; -*-

(require 'cl-lib)
(require 'org-capture)
(require 'org-roam-dailies)

(defvar org-roam-db-update-on-save)

(defun loki-assistant-daily-check-buffer (buffer fail)
  "确认 BUFFER 已保存且与磁盘一致，冲突时调用 FAIL。"
  (when (and buffer
             (or (buffer-modified-p buffer)
                 (not (verify-visited-file-modtime buffer))))
    (funcall fail 'conflict)))

(defun loki-assistant-daily-capture (key prepare fail)
  "使用模板 KEY 将条目写入当天日记。
在占位标题上调用 PREPARE 填充内容，失败时通过 FAIL 报告原因。
返回已保存的日记路径。"
  (let* ((template (assoc key org-roam-dailies-capture-templates))
         (target (plist-get (nthcdr 4 template) :target))
         (root (file-name-as-directory
                (file-truename (expand-file-name org-roam-dailies-directory org-roam-directory))))
         (get-buffer (symbol-function 'org-capture-target-buffer))
         (org-capture-entry nil)
         (org-capture-plist nil)
         (org-capture-after-finalize-hook org-capture-after-finalize-hook)
         (org-capture-link-is-already-stored t)
         (org-store-link-plist nil)
         (org-roam-db-update-on-save nil)
         (enable-local-variables nil)
         (enable-local-eval nil)
         file buffer visiting capture-buffer change-group prepared saved)
    (unless (memq (car-safe target) '(file file+head file+olp file+head+olp file+datetree))
      (funcall fail 'template))
    (let ((org-roam-dailies-capture-templates
           `(("l" "日记" entry "* 日记条目"
              :target ,(copy-tree target) :empty-lines-before 1 :immediate-finish t :no-save t
              :prepare-finalize
              ,(lambda ()
                 (setq capture-buffer (current-buffer))
                 (save-excursion
                   (goto-char (point-min))
                   (org-back-to-heading t)
                   (funcall prepare)
                   (setq prepared t)))))))
      (save-window-excursion
        (unwind-protect
            (progn
              (cl-letf (((symbol-function 'org-capture-target-buffer)
                         (lambda (path)
                           (unless (string-prefix-p root (file-truename path))
                             (funcall fail 'template))
                           (setq visiting (org-find-base-buffer-visiting path))
                           (loki-assistant-daily-check-buffer visiting fail)
                           (make-directory (file-name-directory path) t)
                           (setq file path
                                 buffer (funcall get-buffer path)
                                 change-group (prepare-change-group buffer))
                           (activate-change-group change-group)
                           buffer)))
                (org-roam-dailies-capture-today nil "l"))
              (with-current-buffer buffer
                (unless prepared
                  (funcall fail 'save))
                (save-buffer))
              (accept-change-group change-group)
              (setq saved t))
          (unless saved
            (when (buffer-live-p capture-buffer) (kill-buffer capture-buffer))
            (when change-group (cancel-change-group change-group))
            (when (and (buffer-live-p buffer) (not visiting)) (kill-buffer buffer))))))
    (unless (and file (file-regular-p file))
      (funcall fail 'save))
    file))

(provide 'loki-assistant-daily)
;;; loki-assistant-daily.el ends here
