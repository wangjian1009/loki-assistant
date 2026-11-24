;;; loki-assistant-read-later.el --- 日记中的稍后阅读 -*- lexical-binding: t; -*-

;;; Code:

(require 'loki-assistant-doc)
(require 'browse-url)
(require 'org-capture)
(require 'org-id)
(require 'tabulated-list)

(declare-function org-roam-dailies-capture-today "org-roam-dailies" (&optional goto keys))
(declare-function org-roam-db-clear-file "org-roam-db" (&optional file))
(defvar org-roam-dailies-capture-templates)
(defvar org-roam-db-location)

(defcustom loki-assistant-read-later-daily-template-key "d"
  "用于确定稍后阅读所在日记文件的 Org-roam 日记模板键。"
  :type 'string
  :group 'loki-assistant-doc)

(defconst loki-assistant-read-later--tag "稍后阅读")
(defconst loki-assistant-read-later--legacy-captured-at "LOKI_READ_LATER_CAPTURED_AT")
(defconst loki-assistant-read-later--read-at "READED")
(defvar loki-assistant-read-later--pending-index (make-hash-table :test #'equal)
  "已保存但尚待索引的日记文件，按索引路径与文件名分组。")

(defun loki-assistant-read-later--web-url-p (url)
  "判断 URL 是否为绝对 HTTP(S) 网页链接。"
  (and (stringp url)
       (string-match-p "\\`https?://" url)
       (not (string-match-p "[[:space:][:cntrl:]]" url))
       (let ((parsed (ignore-errors (url-generic-parse-url url))))
         (and parsed (not (string-empty-p (or (url-host parsed) "")))))))

(defun loki-assistant-read-later--heading-url (heading)
  "从 HEADING 取第一个绝对 HTTP(S) Org 链接。"
  (let ((position 0) url)
    (while (and (not url) (string-match org-link-bracket-re heading position))
      (setq position (match-end 0))
      (let ((target (org-link-unescape (match-string 1 heading))))
        (when (loki-assistant-read-later--web-url-p target)
          (setq url target))))
    url))

(defun loki-assistant-read-later--org-timestamp ()
  "返回当前本地时间的 Org 非激活时间戳。"
  (let ((system-time-locale "C"))
    (format-time-string (org-time-stamp-format t t))))

(defun loki-assistant-read-later--timestamp-utc (stamp)
  "将 Org 时间戳 STAMP 转换为 API 使用的 UTC 时间。"
  (when-let* ((time (ignore-errors (org-time-string-to-time stamp))))
    (format-time-string "%FT%TZ" time t)))

(defun loki-assistant-read-later--captured-at (properties)
  "从 PROPERTIES 提取用于 API 的创建时间，兼容旧捕获属性。"
  (or (loki-assistant-read-later--timestamp-utc (cdr (assoc "CREATED" properties)))
      (cdr (assoc loki-assistant-read-later--legacy-captured-at properties))
      :null))

(defun loki-assistant-read-later--daily-root ()
  "返回日记目录的绝对路径。"
  (expand-file-name org-roam-dailies-directory org-roam-directory))

(defun loki-assistant-read-later--require-roam ()
  "加载日记能力并补齐当前知识库的待更新索引。"
  (loki-assistant-doc--require-roam)
  (unless (ignore-errors (require 'org-roam-dailies nil t))
    (loki-assistant-doc--fail "doc-unavailable"))
  (maphash (lambda (key _)
             (when (equal (car key) org-roam-db-location)
               (loki-assistant-read-later--index (cdr key))))
           loki-assistant-read-later--pending-index))

(defun loki-assistant-read-later--index (file)
  "更新 FILE 的 org-roam 索引，失败时保留重试记录。"
  (let ((key (cons org-roam-db-location file)))
    (puthash key t loki-assistant-read-later--pending-index)
    (condition-case nil
        (let ((org-roam-db-update-on-save nil)
              (enable-local-variables nil)
              (enable-local-eval nil))
          (if (file-regular-p file)
              (progn
                ;; org-roam 更新已访问文件时可能调用 save-buffer。
                (loki-assistant-read-later--check-buffer (org-find-base-buffer-visiting file))
                (org-roam-db-update-file file))
            (org-roam-db-clear-file file))
          (remhash key loki-assistant-read-later--pending-index))
      (error (loki-assistant-doc--fail "read-later-index-failed")))))

(defun loki-assistant-read-later--entries ()
  "从 org-roam 索引返回按捕获时间倒序排列的未读日记标题。"
  (let ((root (loki-assistant-read-later--daily-root))
        entries)
    (pcase-dolist (`(,id ,file ,title ,properties)
                   (org-roam-db-query
                    [:select :distinct [nodes:id nodes:file nodes:title nodes:properties]
                     :from tags :inner-join nodes :on (= tags:node-id nodes:id)
                     :where (and (= tags:tag $s1) (> nodes:level 0))]
                    loki-assistant-read-later--tag))
      (when (and (not (assoc loki-assistant-read-later--read-at properties))
                 (member loki-assistant-read-later--tag
                         (split-string (or (cdr (assoc "TAGS" properties)) "") ":" t))
                 (file-in-directory-p file root)
                 (file-regular-p file))
        (let ((url (loki-assistant-read-later--heading-url (or (cdr (assoc "ITEM" properties)) ""))))
          (push `((id . ,id) (title . ,title) (url . ,(or url :null))
                  (captured_at . ,(loki-assistant-read-later--captured-at properties)))
                entries))))
    (sort entries (lambda (first second)
                    (let ((first-time (alist-get 'captured_at first))
                          (second-time (alist-get 'captured_at second)))
                      (string> (if (stringp first-time) first-time "")
                               (if (stringp second-time) second-time "")))))))

(defun loki-assistant-read-later--check-buffer (buffer)
  "确认 BUFFER 没有尚未保存的编辑或未加载的磁盘修改。"
  (when (and buffer
             (or (buffer-modified-p buffer)
                 (not (verify-visited-file-modtime buffer))))
    (loki-assistant-doc--fail "read-later-daily-conflict")))

(defun loki-assistant-read-later--capture (title url)
  "通过日记捕获保存 TITLE 和 URL，返回新节点的摘要。"
  (let* ((template (assoc loki-assistant-read-later-daily-template-key
                          org-roam-dailies-capture-templates))
         (target (plist-get (nthcdr 4 template) :target))
         (root (file-name-as-directory (file-truename (loki-assistant-read-later--daily-root))))
         (id (org-id-new))
         (created (loki-assistant-read-later--org-timestamp))
         (get-buffer (symbol-function 'org-capture-target-buffer))
         (org-capture-entry nil)
         (org-capture-plist nil)
         (org-capture-after-finalize-hook org-capture-after-finalize-hook)
         (org-capture-link-is-already-stored t)
         (org-store-link-plist nil)
         (org-roam-db-update-on-save nil)
         (enable-local-variables nil)
         (enable-local-eval nil)
         file buffer visiting capture-buffer change-group saved)
    (unless (memq (car-safe target) '(file file+head file+olp file+head+olp file+datetree))
      (loki-assistant-doc--fail "read-later-template-unavailable"))
    (let ((org-roam-dailies-capture-templates
           `(("r" "稍后阅读" entry ,(concat "* 网页 :" loki-assistant-read-later--tag ":")
              :target ,(copy-tree target) :empty-lines-before 1 :immediate-finish t :no-save t
              :prepare-finalize
              ,(lambda ()
                 (setq capture-buffer (current-buffer))
                 (save-excursion
                   (goto-char (point-min))
                   (org-back-to-heading t)
                   ;; 在模板展开之后写入网页数据。
                   (org-edit-headline (org-link-make-string url title))
                   (org-entry-put nil "ID" id)
                   (org-entry-put nil "CREATED" created)))))))
      (save-window-excursion
        (unwind-protect
            (progn
              (cl-letf (((symbol-function 'org-capture-target-buffer)
                         (lambda (path)
                           (unless (string-prefix-p root (file-truename path))
                             (loki-assistant-doc--fail "read-later-template-unavailable"))
                           (setq visiting (org-find-base-buffer-visiting path))
                           (loki-assistant-read-later--check-buffer visiting)
                           (make-directory (file-name-directory path) t)
                           (setq file path
                                 buffer (funcall get-buffer path)
                                 change-group (prepare-change-group buffer))
                           (activate-change-group change-group)
                           buffer)))
                (org-roam-dailies-capture-today nil "r"))
              (with-current-buffer buffer
                (unless (org-find-entry-with-id id)
                  (loki-assistant-doc--fail "read-later-save-failed"))
                (save-buffer))
              (accept-change-group change-group)
              (setq saved t))
          (unless saved
            (when (buffer-live-p capture-buffer) (kill-buffer capture-buffer))
            (when change-group (cancel-change-group change-group))
            (when (and (buffer-live-p buffer) (not visiting)) (kill-buffer buffer))))))
    (unless (and file (file-regular-p file))
      (loki-assistant-doc--fail "read-later-save-failed"))
    (loki-assistant-read-later--index file)
    (unless (org-roam-node-from-id id)
      (loki-assistant-doc--fail "read-later-index-failed"))
    `((id . ,id) (title . ,title) (url . ,url)
      (captured_at . ,(loki-assistant-read-later--captured-at (list (cons "CREATED" created)))))))

(defun loki-assistant-read-later-capture (info)
  "将 INFO 中的网页链接加入当天日记，重复提交时复用未读节点。"
  (catch 'loki-assistant-doc-error
    (let ((title (plist-get info :title))
          (url (plist-get info :url)))
      (unless (and (stringp title) (not (string-blank-p title))
                   (not (string-match-p "[[:cntrl:]]" title))
                   (loki-assistant-read-later--web-url-p url))
        (loki-assistant-doc--fail "invalid-read-later-link"))
      (loki-assistant-read-later--require-roam)
      (let* ((entries (loki-assistant-read-later--list-entries))
             (key (loki-assistant-doc--url-key url))
             (existing (seq-find (lambda (entry)
                                   (let ((entry-url (alist-get 'url entry)))
                                     (and (stringp entry-url)
                                          (equal key (loki-assistant-doc--url-key entry-url)))))
                                 entries))
             (entry (or existing
                        (condition-case nil
                            (loki-assistant-read-later--capture (string-trim title) url)
                          (error (loki-assistant-doc--fail "read-later-save-failed"))))))
        `((result (entry . ,entry)))))))

(defun loki-assistant-read-later--list-entries ()
  "取索引中的未读记录，识别查询错误。"
  (condition-case nil
      (loki-assistant-read-later--entries)
    (error (loki-assistant-doc--fail "document-read-failed"))))

(defun loki-assistant-read-later-list ()
  "查询未读记录，不读取网页或 Org 正文。"
  (catch 'loki-assistant-doc-error
    (loki-assistant-read-later--require-roam)
    `((result (entries . ,(vconcat (loki-assistant-read-later--list-entries)))))))

(defun loki-assistant-read-later--entry-file (id)
  "返回 ID 对应的稍后阅读日记文件。"
  (unless (and (stringp id) (not (string-blank-p id))
               (not (string-match-p "[[:cntrl:]]" id)))
    (loki-assistant-doc--fail "invalid-request"))
  (loki-assistant-read-later--require-roam)
  (let* ((node (condition-case nil
                   (org-roam-node-from-id id)
                 (error (loki-assistant-doc--fail "document-read-failed"))))
         (file (and node (org-roam-node-file node))))
    (unless (and file (file-in-directory-p file (loki-assistant-read-later--daily-root))
                 (file-regular-p file) (> (org-roam-node-level node) 0))
      (loki-assistant-doc--fail "read-later-not-found"))
    file))

(defun loki-assistant-read-later--goto-entry (id)
  "定位当前日记中 ID 对应的稍后阅读标题。"
  (let ((position (org-find-entry-with-id id)))
    (unless position (loki-assistant-doc--fail "read-later-not-found"))
    (goto-char position)
    (unless (and (org-at-heading-p)
                 (member loki-assistant-read-later--tag (org-get-tags nil t)))
      (loki-assistant-doc--fail "read-later-not-found"))))

(defun loki-assistant-read-later--update-entry (id update)
  "在 ID 对应的稍后阅读标题上执行 UPDATE，保存并更新索引后返回其结果。"
  (let ((file (loki-assistant-read-later--entry-file id))
        result)
    (condition-case nil
        (let ((enable-local-variables nil)
              (enable-local-eval nil)
              (org-roam-db-update-on-save nil)
              (visiting (org-find-base-buffer-visiting file)))
          (loki-assistant-read-later--check-buffer visiting)
          (with-current-buffer (or visiting (find-file-noselect file))
            (org-with-wide-buffer
             (loki-assistant-read-later--goto-entry id)
             (atomic-change-group
               (setq result (funcall update))
               (when (buffer-modified-p) (save-buffer))))))
      (error (loki-assistant-doc--fail "read-later-save-failed")))
    (loki-assistant-read-later--index file)
    result))

(defun loki-assistant-read-later-mark-read (id)
  "标记 ID 对应的日记节点为已读，保留首次已读时间。"
  (catch 'loki-assistant-doc-error
    (let ((read-at
           (loki-assistant-read-later--update-entry
            id (lambda ()
                 (or (org-entry-get nil loki-assistant-read-later--read-at)
                     (let ((time (loki-assistant-read-later--org-timestamp)))
                       (org-entry-put nil loki-assistant-read-later--read-at time)
                       time))))))
      `((result (id . ,id)
                (read_at . ,(or (and (string-prefix-p "[" read-at)
                                     (loki-assistant-read-later--timestamp-utc read-at))
                                read-at)))))))

(defun loki-assistant-read-later-delete (id)
  "移除稍后阅读节点的 ID 属性，保留日记条目。"
  (catch 'loki-assistant-doc-error
    (loki-assistant-read-later--update-entry id (lambda () (org-entry-delete nil "ID")))
    (when (condition-case nil
              (org-roam-node-from-id id)
            (error (loki-assistant-doc--fail "read-later-index-failed")))
      (loki-assistant-doc--fail "read-later-index-failed"))
    `((result (id . ,id)))))

(defun loki-assistant-read-later--result (reply)
  "返回 REPLY 中的结果，交互操作失败时显示原因。"
  (if-let* ((code (alist-get 'error reply)))
      (user-error "%s"
                  (pcase code
                    ("doc-unavailable" "请先加载 org-roam 及日记模块")
                    ("document-read-failed" "无法读取稍后阅读，请检查 org-roam 索引和日记文件")
                    ("read-later-not-found" "找不到这条稍后阅读记录，请按 g 刷新")
                    ("read-later-daily-conflict" "日记有未保存或未加载的更改，请处理后重试")
                    ("read-later-save-failed" "无法保存稍后阅读记录，请检查日记文件")
                    ("read-later-index-failed" "无法更新日记索引，请检查 org-roam 后重试")
                    (_ (format "稍后阅读操作失败（%s）" code))))
    (alist-get 'result reply)))

(defun loki-assistant-read-later--load-rows ()
  "将当前未读记录加载到列表。"
  (let* ((result (loki-assistant-read-later--result (loki-assistant-read-later-list)))
         (entries (alist-get 'entries result)))
    (setq tabulated-list-entries
          (mapcar (lambda (entry)
                    (let ((captured-at (alist-get 'captured_at entry))
                          (title (alist-get 'title entry))
                          (url (alist-get 'url entry)))
                      (list (alist-get 'id entry)
                            (vector (if (stringp captured-at) captured-at "")
                                    title
                                    (if (stringp url) url "")))))
                  entries))))

(defun loki-assistant-read-later--id-at-point ()
  "返回当前行的 Org ID。"
  (or (tabulated-list-get-id) (user-error "请先选择一条未读记录")))

(defun loki-assistant-read-later--entry-location (id)
  "返回 ID 对应的日记缓冲区及标题位置，或文档错误。"
  (catch 'loki-assistant-doc-error
    (let* ((file (loki-assistant-read-later--entry-file id))
           (buffer (find-file-noselect file)))
      (with-current-buffer buffer
        (org-with-wide-buffer
         (loki-assistant-read-later--goto-entry id)
         `((result . (,buffer . ,(point)))))))))

(defun loki-assistant-read-later-open ()
  "打开当前记录对应的 Org 日记条目。"
  (interactive)
  (let* ((id (loki-assistant-read-later--id-at-point))
         (location
          (loki-assistant-read-later--result (loki-assistant-read-later--entry-location id))))
    (pop-to-buffer-same-window (car location))
    (unless (<= (point-min) (cdr location) (point-max)) (widen))
    (goto-char (cdr location))
    (org-fold-show-context)
    (org-fold-show-entry)))

(defun loki-assistant-read-later-open-url ()
  "打开当前记录的网页链接。"
  (interactive)
  (loki-assistant-read-later--id-at-point)
  (let ((url (aref (tabulated-list-get-entry) 2)))
    (unless (loki-assistant-read-later--web-url-p url)
      (user-error "此条目没有网页链接，按 RET 可打开日记条目"))
    (browse-url url)))

(defun loki-assistant-read-later-mark-current-read ()
  "标记当前记录为已读并从未读列表移除。"
  (interactive)
  (let ((id (loki-assistant-read-later--id-at-point)))
    (loki-assistant-read-later--result (loki-assistant-read-later-mark-read id))
    (setq tabulated-list-entries (assoc-delete-all id tabulated-list-entries))
    (tabulated-list-delete-entry)
    (when (and (eobp) (not (bobp))) (forward-line -1))
    (message "已标记为已读")))

(defvar loki-assistant-read-later-mode-map
  (let ((map (make-sparse-keymap)))
    (define-key map (kbd "RET") #'loki-assistant-read-later-open)
    (define-key map (kbd "b") #'loki-assistant-read-later-open-url)
    (define-key map (kbd "r") #'loki-assistant-read-later-mark-current-read)
    map)
  "稍后阅读列表的快捷键。")

(define-derived-mode loki-assistant-read-later-mode tabulated-list-mode "稍后阅读"
  "显示 org-roam 日记中的未读记录。"
  (setq tabulated-list-format [("加入时间 (UTC)" 20 t) ("标题" 42 t) ("网页" 0 nil)]
        tabulated-list-padding 2)
  (setq-local mode-line-process '(:eval (unless tabulated-list-entries " · 没有未读记录")))
  (add-hook 'tabulated-list-revert-hook #'loki-assistant-read-later--load-rows nil t)
  (tabulated-list-init-header))

;;;###autoload
(defun loki-assistant-read-later ()
  "打开并刷新稍后阅读未读列表。"
  (interactive)
  (let ((buffer (get-buffer-create "*稍后阅读*")))
    (with-current-buffer buffer
      (unless (derived-mode-p 'loki-assistant-read-later-mode)
        (loki-assistant-read-later-mode))
      (tabulated-list-revert))
    (pop-to-buffer-same-window buffer)))

(provide 'loki-assistant-read-later)
;;; loki-assistant-read-later.el ends here
