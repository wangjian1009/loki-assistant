;;; loki-assistant-doc-tests.el --- 文档服务回归测试 -*- lexical-binding: t; -*-

;;; Code:

(require 'ert)
(require 'loki-assistant-doc)
(require 'org-roam-capture)

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
          (loki-assistant-doc--unindexed (make-hash-table :test 'equal)))
     (unwind-protect
         (progn ,@body)
       (org-roam-db--close-all)
       (dolist (buffer (buffer-list))
         (let ((file (buffer-local-value 'buffer-file-name buffer)))
           (when (and file (file-in-directory-p file root))
             (with-current-buffer buffer (set-buffer-modified-p nil))
             (kill-buffer buffer))))
       (delete-directory root t))))

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
