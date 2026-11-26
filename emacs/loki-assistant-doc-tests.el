;;; loki-assistant-doc-tests.el --- 文档服务回归测试 -*- lexical-binding: t; -*-

;;; Code:

(require 'ert)
(require 'loki-assistant-doc)
(require 'loki-assistant-read-later)
(require 'org-roam-capture)
(require 'org-roam-dailies)

(defmacro loki-assistant-doc-test--with-library (&rest body)
  "在临时 org-roam 知识库中执行 BODY。"
  (declare (indent 0))
  `(let* ((root (make-temp-file "loki-doc-test-" t))
          (org-roam-directory root)
          (org-roam-db-location (expand-file-name "roam.db" root))
          (org-roam-db--connection (make-hash-table :test 'equal))
          (org-roam-db-node-include-function (lambda () t))
          (org-id-locations (make-hash-table :test 'equal))
          (org-id-files nil)
          (org-capture-plist nil)
          (org-capture-last-stored-marker (make-marker))
          (loki-assistant-read-later-search-program loki-assistant-read-later--dist-program)
          (loki-assistant-doc--unindexed (make-hash-table :test 'equal))
          (loki-assistant-read-later--pending-index (make-hash-table :test 'equal)))
     (unwind-protect
         (progn ,@body)
       (org-roam-db--close-all)
       (dolist (buffer (buffer-list))
         (let ((file (buffer-local-value 'buffer-file-name buffer)))
           (when (and file (file-in-directory-p file root))
             (with-current-buffer buffer (set-buffer-modified-p nil))
             (kill-buffer buffer))))
       (let ((org-roam-file-exclude-regexp '(".")))
         (delete-directory root t)))))

(ert-deftest loki-assistant-doc-test-read-later-daily-roundtrip ()
  (loki-assistant-doc-test--with-library
    (let* ((org-roam-dailies-directory "journal/")
           (org-roam-dailies-capture-templates
            '(("d" "日记" entry "* %?"
               :target (file+head "%<%Y>/%<%m>/%<%Y-%m-%d>.org" "#+TITLE: 日记\n")
               :jump-to-captured t)))
           (url "https://example.com/文章?x=%5B1%5D#位置")
           (info (list :title "网页 %(error \"不执行\") ${title}" :url url))
           (file (expand-file-name (format-time-string "journal/%Y/%m/%F.org") root))
           entry id)
      (should (equal (loki-assistant-read-later-list) '((result (entries . [])))))
      (should (equal (alist-get 'error
                                (loki-assistant-read-later-capture
                                 '(:title "内部页面" :url "chrome://settings")))
                     "invalid-read-later-link"))
      (cl-letf (((symbol-function 'save-buffer)
                 (lambda (&rest _) (error "日记暂不可写"))))
        (should (equal (alist-get 'error (loki-assistant-read-later-capture info))
                       "read-later-save-failed")))
      (should-not (file-exists-p file))
      (should-not (find-buffer-visiting file))
      (cl-letf (((symbol-function 'org-roam-db-update-file)
                 (lambda (&rest _) (error "索引暂不可用"))))
        (should (equal (alist-get 'error (loki-assistant-read-later-capture info))
                       "read-later-index-failed")))
      (should (file-regular-p file))
      (setq entry (alist-get 'entry (alist-get 'result (loki-assistant-read-later-capture info)))
            id (alist-get 'id entry))
      (should (string-prefix-p "read-later:" id))
      (should (equal entry (alist-get 'entry (alist-get 'result (loki-assistant-read-later-capture info)))))
      (should (equal (alist-get 'title entry) (plist-get info :title)))
      (should (equal (alist-get 'url entry) url))
      (should (equal (alist-get 'entries (alist-get 'result (loki-assistant-read-later-list)))
                     (vector entry)))
      (with-current-buffer (find-buffer-visiting file)
        (set-buffer-modified-p t)
        (unwind-protect
            (should (equal (alist-get 'error (loki-assistant-read-later-mark-read id))
                           "read-later-daily-conflict"))
          (set-buffer-modified-p nil)))
      (cl-letf (((symbol-function 'org-roam-db-update-file)
                 (lambda (&rest _) (error "索引暂不可用"))))
        (should (equal (alist-get 'error (loki-assistant-read-later-mark-read id))
                       "read-later-index-failed")))
      (let ((read (loki-assistant-read-later-mark-read id)))
        (should (equal (alist-get 'id (alist-get 'result read)) id))
        (with-current-buffer (find-buffer-visiting file)
          (org-with-wide-buffer
           (loki-assistant-read-later--goto-entry id)
           (should-not (org-entry-get nil "ID"))
           (let ((stamp (org-entry-get nil "READED")))
             (should (string-match-p (rx string-start "[" (= 4 digit) "-" (= 2 digit) "-" (= 2 digit)
                                         " " (= 3 alpha) " " (= 2 digit) ":" (= 2 digit) "]" string-end)
                                     stamp))
             (should (equal (alist-get 'read_at (alist-get 'result read))
                            (format-time-string "%FT%TZ" (org-time-string-to-time stamp) t))))))
        (should (equal read (loki-assistant-read-later-mark-read id))))
      (should (equal (loki-assistant-read-later-list) '((result (entries . [])))))
      (let ((content (with-temp-buffer
                       (insert-file-contents file)
                       (buffer-string))))
        (should (string-match-p ":稍后阅读:" content))
        (should (string-match-p (regexp-quote (concat "* " (org-link-make-string url (plist-get info :title)))) content))
        (should (= 1 (with-temp-buffer
                       (insert content)
                       (how-many (regexp-quote url) (point-min) (point-max)))))
        (should (string-match-p ":CREATED: +[0-9-]+T[0-9:.]+Z" content))
        (should-not (string-match-p ":ROAM_REFS:\\|:LOKI_READ_LATER_CAPTURED_AT:" content))
        (should (string-match-p "^:READED:" content))))))

(ert-deftest loki-assistant-doc-test-read-later-delete-removes-subtree ()
  (loki-assistant-doc-test--with-library
    (let* ((org-roam-dailies-directory "journal/")
           (file (expand-file-name "journal/2026-06-01.org" root))
           (prefix "#+TITLE: 日记\n\n* 保留的上级\n")
           (suffix "** 保留的兄弟\n内容\n* 另一条记录\n")
           (before (concat prefix "** 网页标题 :稍后阅读:\n"
                           ":PROPERTIES:\n:ID: deletable\n:CREATED: [2026-06-01 Mon 08:30]\n:END:\n"
                           "网页笔记\n"
                           "*** 子标题\n:PROPERTIES:\n:ID: child-id\n:END:\n"
                           suffix)))
      (make-directory (file-name-directory file) t)
      (with-temp-file file (insert before))
      (org-roam-db-update-file file)
      (should (= 1 (length (alist-get 'entries (alist-get 'result (loki-assistant-read-later-list))))))
      (should (equal (alist-get 'error (loki-assistant-read-later-delete "child-id"))
                     "read-later-not-found"))
      (cl-letf (((symbol-function 'save-buffer)
                 (lambda (&rest _) (error "日记暂不可写"))))
        (should (equal (loki-assistant-read-later-delete "deletable")
                       '((error . "read-later-save-failed")))))
      (with-current-buffer (find-buffer-visiting file)
        (should (equal (buffer-string) before)))
      (let ((kill-ring '("保留的剪切内容"))
            (interprogram-cut-function
             (lambda (&rest _) (ert-fail "删除记录写入了系统剪贴板"))))
        (should (equal (loki-assistant-read-later-delete "deletable")
                       '((result (id . "deletable")))))
        (should (equal kill-ring '("保留的剪切内容"))))
      (should (equal (loki-assistant-read-later-delete "deletable") '((error . "read-later-not-found"))))
      (should (equal (loki-assistant-read-later-list) '((result (entries . [])))))
      (should-not (org-roam-node-from-id "deletable"))
      (should (equal (loki-assistant-doc-get "deletable") '((error . "document-not-found"))))
      (should-not (org-roam-node-from-id "child-id"))
      (with-temp-buffer
        (insert-file-contents file)
        (should (equal (buffer-string) (concat prefix suffix)))))))

(ert-deftest loki-assistant-doc-test-read-later-keeps-entries-without-heading-links ()
  (loki-assistant-doc-test--with-library
    (let* ((org-roam-dailies-directory "journal/")
           (file (expand-file-name "journal/2026-06-01.org" root))
           (url "https://example.org/guide#daily"))
      (make-directory (file-name-directory file) t)
      (with-temp-file file
        (insert "* 日记中的网页 :稍后阅读:\n:PROPERTIES:\n:ID: older-link\n"
                ":ROAM_REFS: https://example.org/reference\n:END:\n"
                (org-link-make-string url "网页") "\n"
                "** 子节点\n:PROPERTIES:\n:ID: child-link\n:END:\n"
                "[[https://example.org/child]]\n"
                "* 旧版删除记录 :稍后阅读:\n"
                ":PROPERTIES:\n:CREATED: [2026-06-01 Mon 10:30]\n:END:\n"))
      (org-roam-db-update-file file)
      (should (equal (loki-assistant-read-later-list)
                     '((result (entries . [((id . "older-link") (title . "日记中的网页")
                                            (url . :null) (captured_at . :null))])))))
      (let* ((reply (loki-assistant-read-later-capture (list :title "新标题" :url url)))
             (entry (alist-get 'entry (alist-get 'result reply)))
             (id (alist-get 'id entry)))
        (should (stringp id))
        (should-not (equal id "older-link"))
        (should (equal (loki-assistant-read-later-delete id) (list (list 'result (cons 'id id)))))
        (should (equal (loki-assistant-read-later-delete id) '((error . "read-later-not-found")))))
      (should (alist-get 'result (loki-assistant-read-later-mark-read "older-link")))
      (should (equal (loki-assistant-read-later-list) '((result (entries . []))))))))

(defun loki-assistant-doc-test--wait-for (predicate)
  "等待异步查询满足 PREDICATE。"
  (let ((deadline (+ (float-time) 3)))
    (while (and (not (funcall predicate)) (< (float-time) deadline))
      (accept-process-output nil 0.05))
    (should (funcall predicate))))

(ert-deftest loki-assistant-doc-test-read-later-mode-opens-and-marks-current-entry ()
  (loki-assistant-doc-test--with-library
    (let* ((org-roam-dailies-directory "journal/")
           (file (expand-file-name "journal/inbox.org" root))
           (url "https://example.org/article")
           opened-url)
      (make-directory (file-name-directory file) t)
      (with-temp-file file
        (insert "* 旧条目 :稍后阅读:\n:PROPERTIES:\n:ID: first\n:END:\n"
                "* " (org-link-make-string url "新条目") " :稍后阅读:\n"
                ":PROPERTIES:\n:ID: second\n:CREATED: [2026-06-01 Mon 09:30]\n:END:\n"
                "网页笔记\n"))
      (should-not (org-roam-node-from-id "second"))
      (save-window-excursion
        (with-temp-buffer
          (loki-assistant-read-later-mode)
          (call-interactively (key-binding (kbd "g")))
          (loki-assistant-doc-test--wait-for
           (lambda () (not loki-assistant-read-later--list-process)))
          (should-not loki-assistant-read-later--list-error)
          (goto-char (point-min))
          (should (equal (tabulated-list-get-id) "second"))
          (should (string-match-p "新条目" (buffer-string)))
          (cl-letf (((symbol-function 'browse-url)
                     (lambda (address &rest _) (setq opened-url address))))
            (call-interactively (key-binding (kbd "RET"))))
          (should (equal opened-url url))
          (save-window-excursion
            (save-current-buffer
              (call-interactively (key-binding (kbd "A-<return>")))
              (should (file-equal-p buffer-file-name file))
              (should (equal (org-entry-get nil "ID") "second"))
              (should-not (org-entry-get nil "READED"))))
          (let ((journal (find-buffer-visiting file)))
            (with-current-buffer journal (set-buffer-modified-p t))
            (unwind-protect
                (should-error (call-interactively (key-binding (kbd "r"))) :type 'user-error)
              (with-current-buffer journal (set-buffer-modified-p nil))))
          (should (equal (tabulated-list-get-id) "second"))
          (call-interactively (key-binding (kbd "r")))
          (should (equal (tabulated-list-get-id) "first"))
          (should-error (call-interactively (key-binding (kbd "RET"))) :type 'user-error)
          (call-interactively (key-binding (kbd "g")))
          (loki-assistant-doc-test--wait-for
           (lambda () (not loki-assistant-read-later--list-process)))
          (should (equal (mapcar #'car tabulated-list-entries) '("first")))
          (with-current-buffer (find-buffer-visiting file)
            (goto-char (org-find-entry-with-id "second"))
            (should (org-entry-get nil "READED"))))))))

(ert-deftest loki-assistant-doc-test-read-later-stream-renders-before-exit ()
  (loki-assistant-doc-test--with-library
    (let* ((org-roam-dailies-directory "journal/")
           (program (expand-file-name "org-daily" root))
           (loki-assistant-read-later-search-program program))
      (make-directory (expand-file-name org-roam-dailies-directory root))
      (with-temp-file program
        (insert "#!/bin/sh\nprintf '%s' '{\"title\":\"流式'\n"
                "read -r gate || exit 2\n"
                "printf '%s\\n' '记录\",\"properties\":{\"ID\":\"first\"}}'\n"
                "read -r gate || exit 2\n"
                "printf '%s\\n' '{\"title\":\"较新记录\",\"properties\":{\"ID\":\"second\","
                "\"CREATED\":\"[2026-06-01 Mon 09:30]\"}}'\n"))
      (set-file-modes program #o700)
      (with-temp-buffer
        (loki-assistant-read-later-mode)
        (tabulated-list-revert)
        (let ((cancelled loki-assistant-read-later--list-process))
          (should (process-live-p cancelled))
          (tabulated-list-revert)
          (should-not (process-live-p cancelled)))
        (let ((process loki-assistant-read-later--list-process))
          (should (process-live-p process))
          (should-not tabulated-list-entries)
          (process-send-string process "第一条\n")
          (loki-assistant-doc-test--wait-for
           (lambda () (equal (mapcar #'car tabulated-list-entries) '("first"))))
          (should (process-live-p process))
          (should (string-match-p "流式记录" (buffer-string)))
          (process-send-string process "第二条\n")
          (loki-assistant-doc-test--wait-for
           (lambda () (not loki-assistant-read-later--list-process)))
          (should-not loki-assistant-read-later--list-error)
          (should (equal (mapcar #'car tabulated-list-entries) '("second" "first"))))))))

(ert-deftest loki-assistant-doc-test-capture-and-retry-without-confirmation ()
  (loki-assistant-doc-test--with-library
    (let* ((body "* 中文正文\n%? %(error \"不执行\") ${title}\n")
           (url "https://example.com/article/1?from=chrome#section")
           (info (list :title "中文 %(title) ${title}" :url url
                       :captured-at "2026-11-18T12:30:00.000Z" :body body)))
      (should (equal (alist-get 'error (loki-assistant-doc-capture-full "unknown" info))
                     "capture-target-not-found"))
      (cl-letf (((symbol-function 'select-frame-set-input-focus)
                 (lambda (&rest _) (ert-fail "网页全文捕获尝试激活窗口")))
                ((symbol-function 'read-from-minibuffer)
                 (lambda (&rest _) (ert-fail "网页全文捕获尝试请求输入"))))
        (let* ((reply (loki-assistant-doc-capture-full "article" info))
               (result (alist-get 'result reply))
               (summary (alist-get 'document result))
               (id (alist-get 'id summary)))
          (should-not (alist-get 'error reply))
          (should (equal (alist-get 'status result) "saved"))
          (should (equal (alist-get 'kind summary) "article"))
          (should (equal (alist-get 'title summary) (plist-get info :title)))
          (should (equal (alist-get 'node_type summary) "file"))
          (should (string-search body (alist-get 'content (alist-get 'result (loki-assistant-doc-get id)))))
          (should (equal (org-roam-node-id (org-roam-node-from-ref url)) id))
          (let ((retry (alist-get 'result (loki-assistant-doc-capture-full "article" info))))
            (should (equal (alist-get 'status retry) "existing"))
            (should (equal (alist-get 'id (alist-get 'document retry)) id))))))))

(ert-deftest loki-assistant-doc-test-heading-shows-saved-subtree-and-inherits-daily-date ()
  (loki-assistant-doc-test--with-library
    (let* ((file (expand-file-name "daily/2026/11/2026-11-18.org" root))
           (head ":PROPERTIES:\n:ID: file-id\n:END:\n#+title: 日记\n")
           (subtree "* 标题\n:PROPERTIES:\n:ID: article\n:END:\n内容\n** 子标题\n子内容\n")
           (other "* 其他\n其他正文\n"))
      (make-directory (file-name-directory file) t)
      (write-region (concat head subtree other) nil file nil 'silent)
      (org-roam-db-update-file file)
      (let ((file-doc (alist-get 'result (loki-assistant-doc-get "file-id")))
            (heading (alist-get 'result (loki-assistant-doc-get "article"))))
        (should (equal (alist-get 'content file-doc) (concat head subtree other)))
        (should (equal (alist-get 'content heading) subtree))
        (should (equal (alist-get 'date heading) "2026-11-18"))
        (should (equal (alist-get 'kind heading) "daily"))
        (should (equal (alist-get 'node_type heading) "heading")))
      (write-region (concat head other) nil file nil 'silent)
      (should (equal (alist-get 'error (loki-assistant-doc-get "article")) "document-not-found")))))

(ert-deftest loki-assistant-doc-test-capture-opens-new-and-existing-article ()
  (loki-assistant-doc-test--with-library
    (let* ((payload (expand-file-name "request.json" root))
           (request '((operation . "capture")
                      (target . "article")
                      (info (title . "捕获打开测试")
                            (url . "https://example.com/article/open")
                            (captured_at . "2026-11-18T12:30:00Z")
                            (body . "* 测试正文")
                            (open_after_capture . t)))))
      (write-region (json-serialize request) nil payload nil 'silent)
      (cl-labels ((capture ()
                    (alist-get 'result
                               (json-parse-string
                                (decode-coding-string
                                 (base64-decode-string (loki-assistant-doc-request payload))
                                 'utf-8)
                                :object-type 'alist))))
        (cl-letf (((symbol-function 'select-frame-set-input-focus) #'ignore))
          (save-window-excursion
            (let* ((saved (capture))
                   (id (alist-get 'id (alist-get 'document saved)))
                   (buffer (window-buffer)))
              (should (equal (alist-get 'status saved) "saved"))
              (should (eq (alist-get 'opened saved) t))
              (with-current-buffer buffer
                (should (file-in-directory-p buffer-file-name root))
                (should (equal (org-entry-get (point-min) "ID") id)))
              (should (eq (marker-buffer org-capture-last-stored-marker) buffer))
              (switch-to-buffer (get-buffer-create "*scratch*"))
              (let ((existing (capture)))
                (should (equal (alist-get 'status existing) "existing"))
                (should (eq (alist-get 'opened existing) t))
                (should (eq (window-buffer) buffer)))
              (cl-letf (((symbol-function 'org-roam-node-visit)
                         (lambda (&rest _) (error "模拟打开失败"))))
                (let ((existing (capture)))
                  (should (equal (alist-get 'status existing) "existing"))
                  (should (eq (alist-get 'opened existing) :false))
                  (should (equal (alist-get 'id (alist-get 'document existing)) id)))))))))))

(ert-deftest loki-assistant-doc-test-retry-reindexes-the-saved-file ()
  (loki-assistant-doc-test--with-library
    (let ((loki-assistant-doc-capture-templates
           '(("p" "网页全文" plain "%?"
              :target (file+head "article/2026/retry.org" "#+TITLE: ${title}\n"))))
          (info '(:title "待补索引的文章 %(error \"不执行标题\")"
                  :url "https://example.com/article/index"
                  :captured-at "2026-11-18T12:30:00Z"
                  :body "正文")))
      (cl-letf (((symbol-function 'org-roam-db-update-file)
                 (lambda (&rest _) (error "模拟索引失败"))))
        (should (equal (alist-get 'error (loki-assistant-doc-capture-full "article" info))
                       "capture-index-failed")))
      (let* ((files (directory-files-recursively root "\\.org$"))
             (file (car files))
             (saved-id (with-temp-buffer
                         (insert-file-contents file)
                         (should (re-search-forward "^:ID:[ \t]+\\(.+\\)$" nil t))
                         (match-string 1)))
             (result (alist-get 'result (loki-assistant-doc-capture-full "article" info))))
        (should (equal (alist-get 'status result) "existing"))
        (should (equal (alist-get 'title (alist-get 'document result)) (plist-get info :title)))
        (should (equal (alist-get 'id (alist-get 'document result)) saved-id))
        (should (equal (directory-files-recursively root "\\.org$") files))))))

(provide 'loki-assistant-doc-tests)
;;; loki-assistant-doc-tests.el ends here
