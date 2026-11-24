;;; loki-assistant-doc.el --- org-roam 文档查询和捕获 -*- lexical-binding: t; -*-

;;; Code:

(require 'cl-lib)
(require 'epg)
(require 'json)
(require 'org)
(require 'org-element)
(require 'seq)
(require 'subr-x)
(require 'url-parse)
(require 'url-util)

(autoload 'loki-assistant-read-later-capture "loki-assistant-read-later")
(autoload 'loki-assistant-read-later-list "loki-assistant-read-later")
(autoload 'loki-assistant-read-later-mark-read "loki-assistant-read-later")
(autoload 'loki-assistant-read-later-delete "loki-assistant-read-later")

(declare-function org-roam-capture- "org-roam-capture")
(declare-function org-roam-capture--fill-template "org-roam-capture" (template &optional ensure-newline))
(declare-function org-roam-capture--target-truepath "org-roam-capture" (path))
(declare-function org-roam-db-query "org-roam-db" (sql &rest args))
(declare-function org-roam-db-update-file "org-roam-db" (&optional file-path))
(declare-function org-roam-node-create "org-roam-node")
(declare-function org-roam-node-file "org-roam-node" (node))
(declare-function org-roam-node-from-id "org-roam-node" (id))
(declare-function org-roam-node-id "org-roam-node" (node))
(declare-function org-roam-node-level "org-roam-node" (node))
(declare-function org-roam-node-properties "org-roam-node" (node))
(declare-function org-roam-node-tags "org-roam-node" (node))
(declare-function org-roam-node-title "org-roam-node" (node))
(declare-function epa-file-insert-file-contents "epa-file" (file &optional visit beg end replace))
(defvar org-roam-directory)
(defvar org-roam-dailies-directory)
(defvar org-roam-db-update-on-save)
(defvar org-capture-entry)
(defvar org-capture-link-is-already-stored)

(defgroup loki-assistant-doc nil
  "基于 org-roam 的文档服务。"
  :group 'loki-assistant)

(defcustom loki-assistant-doc-capture-templates
  '(("p" "网页全文" plain "%?"
     :target (file+head "article/%<%Y>/%<%Y%m%d%H%M%S>-${slug}.org"
                        "#+SETUPFILE: ../.setup.org\n#+FILETAGS: :%<%Y>:\n")))
  "文档模块使用的 org-roam 模板，网页全文目标 article 对应键 p。
全文模板使用 plain 类型和 file+head 目标，正文、标题与来源由文档模块填入。"
  :type '(repeat sexp)
  :group 'loki-assistant-doc)

(defconst loki-assistant-doc--targets '(("article" . "p")))
(defconst loki-assistant-doc--kinds
  '(("article" . "article") ("literature" . "literature") ("reading" . "reading")
    ("memo" . "memo") ("zettle" . "zettel") ("tasks" . "task")
    ("references" . "reference")))
(defconst loki-assistant-doc--domains
  '(("work" . "work") ("personal" . "personal") ("finance" . "finance")
    ("tony" . "family")))
(defvar loki-assistant-doc--unindexed (make-hash-table :test #'equal)
  "已保存但等待索引的文件，按知识库和来源 URL 查询。")

(define-error 'loki-assistant-doc-locked "文档需要解锁")

(defun loki-assistant-doc--fail (code)
  "结束当前文档操作并返回错误 CODE。"
  (throw 'loki-assistant-doc-error (list (cons 'error code))))

(defun loki-assistant-doc--require-roam ()
  "加载 org-roam 文档能力。"
  (unless (ignore-errors (require 'org-roam nil t))
    (loki-assistant-doc--fail "doc-unavailable")))

(defun loki-assistant-doc--date (text)
  "返回 TEXT 中的有效自然日。"
  (when (string-match-p "\\`[0-9]\\{4\\}-[0-9]\\{2\\}-[0-9]\\{2\\}\\'" text)
    (let ((normalized (ignore-errors
                        (format-time-string "%F" (date-to-time (concat text "T00:00:00Z")) t))))
      (when (equal normalized text) text))))

(defun loki-assistant-doc--summary (node)
  "从 NODE 和其所在目录生成文档摘要。"
  (let* ((file (org-roam-node-file node))
         (relative (file-relative-name file org-roam-directory))
         (directory (and (file-name-directory relative) (car (split-string relative "/"))))
         (daily-directory (or (bound-and-true-p org-roam-dailies-directory) "daily/"))
         (daily (file-in-directory-p file (expand-file-name daily-directory org-roam-directory)))
         (kind (if daily "daily" (or (cdr (assoc directory loki-assistant-doc--kinds)) "note")))
         (org-file (if (member (file-name-extension file) '("gpg" "age"))
                       (file-name-sans-extension file)
                     file))
         (date (and daily (loki-assistant-doc--date (file-name-base org-file)))))
    (list (cons 'id (org-roam-node-id node))
          (cons 'title (or (org-roam-node-title node) ""))
          (cons 'kind kind)
          (cons 'domain (or (cdr (assoc directory loki-assistant-doc--domains)) "general"))
          (cons 'node_type (if (zerop (org-roam-node-level node)) "file" "heading"))
          (cons 'tags (vconcat (org-roam-node-tags node)))
          (cons 'date (or date :null)))))

(defun loki-assistant-doc--read-file (file)
  "将 FILE 的内容解码并插入当前临时缓冲区。"
  (cond
   ((string-suffix-p ".age" file)
    (signal 'loki-assistant-doc-locked nil))
   ((string-suffix-p ".gpg" file)
    (require 'epa-file)
    (let ((epg-pinentry-mode 'error))
      (condition-case nil
          (cl-letf (((symbol-function 'read-passwd)
                     (lambda (&rest _) (signal 'loki-assistant-doc-locked nil))))
            (epa-file-insert-file-contents file))
        (file-error (signal 'loki-assistant-doc-locked nil)))))
   (t (insert-file-contents file))))

(defun loki-assistant-doc--content (node)
  "读取 NODE 对应的已保存 Org 源码并核对 ID。"
  (let ((file (org-roam-node-file node))
        (id (org-roam-node-id node)))
    (when (and (file-in-directory-p file org-roam-directory)
               (file-regular-p file))
      (with-temp-buffer
        (loki-assistant-doc--read-file file)
        (let ((org-inhibit-startup t)
              (enable-local-variables nil)
              (enable-local-eval nil))
          (delay-mode-hooks (org-mode)))
        (let* ((property (org-element-map (org-element-parse-buffer) 'node-property
                           (lambda (entry)
                             (when (and (equal (upcase (org-element-property :key entry)) "ID")
                                        (equal (org-element-property :value entry) id))
                               entry))
                           nil t))
               (heading (and property (org-element-lineage property '(headline)))))
          (when (and property (eq (not heading) (zerop (org-roam-node-level node))))
            (buffer-substring-no-properties
             (if heading (org-element-property :begin heading) (point-min))
             (if heading (org-element-property :end heading) (point-max)))))))))

(defun loki-assistant-doc-get (id)
  "按 ID 查询文档，返回含 result 或 error 的结果。"
  (catch 'loki-assistant-doc-error
    (loki-assistant-doc--require-roam)
    (condition-case nil
        (let* ((node (org-roam-node-from-id id))
               (content (and node (loki-assistant-doc--content node))))
          (unless content (loki-assistant-doc--fail "document-not-found"))
          (list (cons 'result (append (loki-assistant-doc--summary node)
                                      (list (cons 'content content))))))
      (file-missing (loki-assistant-doc--fail "document-not-found"))
      (loki-assistant-doc-locked (loki-assistant-doc--fail "document-locked"))
      (error (loki-assistant-doc--fail "document-read-failed")))))

(defun loki-assistant-doc--validate-capture (info)
  "检查网页全文请求 INFO 的字段。"
  (let* ((title (plist-get info :title))
         (url (plist-get info :url))
         (body (plist-get info :body))
         (captured-at (plist-get info :captured-at))
         (parsed (and (stringp url) (ignore-errors (url-generic-parse-url url)))))
    (unless (and (stringp title) (not (string-blank-p title))
                 (not (string-match-p "[[:cntrl:]]" title))
                 parsed (member (url-type parsed) '("http" "https"))
                 (not (string-empty-p (or (url-host parsed) "")))
                 (not (string-match-p "[[:space:][:cntrl:]]" url))
                 (stringp body) (not (string-blank-p body))
                 (<= (string-bytes body) (* 16 1024 1024))
                 (stringp captured-at)
                 (string-match-p
                  "\\`[0-9]\\{4\\}-[0-9]\\{2\\}-[0-9]\\{2\\}T[0-9]\\{2\\}:[0-9]\\{2\\}:[0-9]\\{2\\}\\(?:\\.[0-9]+\\)?Z\\'"
                  captured-at)
                 (loki-assistant-doc--date (substring captured-at 0 10))
                 (ignore-errors (date-to-time captured-at)))
      (loki-assistant-doc--fail "invalid-request"))))

(defun loki-assistant-doc--url-key (url)
  "统一 URL 中的 Unicode 与百分号编码大小写，保留 URL 分隔符。"
  (replace-regexp-in-string
   "%[[:xdigit:]]\\{2\\}" #'upcase
   (mapconcat (lambda (char)
                (if (> char 127) (url-hexify-string (char-to-string char))
                  (char-to-string char)))
              url "") t t))

(defun loki-assistant-doc--source-matches-p (node key)
  "检查 NODE 的来源是否匹配规范化后的 KEY。"
  (cl-some (lambda (ref) (equal key (loki-assistant-doc--url-key ref)))
           (split-string-and-unquote
            (or (cdr (assoc "ROAM_REFS" (org-roam-node-properties node))) ""))))

(defun loki-assistant-doc--existing-article (url key)
  "从 org-roam 索引查询 URL 对应的唯一文件级文章。"
  (let* ((parsed (url-generic-parse-url url))
         (type (url-type parsed))
         (ref (progn (setf (url-type parsed) nil)
                     (org-link-decode (url-recreate-url parsed))))
         (root (expand-file-name "article/" org-roam-directory))
         (rows (org-roam-db-query
                [:select :distinct [nodes:id nodes:file] :from refs
                 :inner-join nodes :on (= refs:node-id nodes:id)
                 :where (and (= refs:type $s1) (= refs:ref $s2) (= nodes:level 0))]
                type ref))
         matches)
    (dolist (row rows)
      (when (and (file-in-directory-p (cadr row) root) (file-regular-p (cadr row)))
        (let ((node (org-roam-node-from-id (car row))))
          (when (and node (loki-assistant-doc--source-matches-p node key))
            (push node matches)))))
    (when (cdr matches)
      (loki-assistant-doc--fail "capture-source-conflict"))
    (car matches)))

(defun loki-assistant-doc--new-file (desired)
  "为期望路径 DESIRED 分配一个可用的文章文件名。"
  (let* ((root (file-name-as-directory (file-truename org-roam-directory)))
         (articles (file-name-as-directory (file-truename (expand-file-name "article/" root)))))
    (unless (and (string-prefix-p root articles)
                 (string-prefix-p articles (file-truename desired))
                 (string-suffix-p ".org" desired))
      (loki-assistant-doc--fail "capture-template-unavailable")))
  (make-directory (file-name-directory desired) t)
  (let ((file desired)
        (suffix 1))
    (while (or (file-exists-p file) (file-symlink-p file) (find-buffer-visiting file))
      (setq suffix (1+ suffix)
            file (format "%s-%d.org" (file-name-sans-extension desired) suffix)))
    file))

(defun loki-assistant-doc--index (key file id)
  "索引 FILE，并确认来源 KEY 和节点 ID 可供后续查询。"
  (puthash key (cons file id) loki-assistant-doc--unindexed)
  (let ((node (condition-case nil
                  (progn (org-roam-db-update-file file) (org-roam-node-from-id id))
                (error nil))))
    (unless (and node (loki-assistant-doc--source-matches-p node (cdr key)))
      (loki-assistant-doc--fail "capture-index-failed"))
    (remhash key loki-assistant-doc--unindexed)
    node))

(defun loki-assistant-doc--write-article (template info key)
  "按 TEMPLATE 保存 INFO，再为来源 KEY 更新索引。"
  (let* ((title (plist-get info :title))
         (node (org-roam-node-create :title title))
         (title-marker (concat "loki-doc-title-" (org-id-new)))
         (resolve (symbol-function 'org-roam-capture--target-truepath))
         (fill-template (symbol-function 'org-roam-capture--fill-template))
         (options (copy-tree (nthcdr 4 template)))
         (org-capture-entry nil)
         (org-capture-link-is-already-stored t)
         (org-store-link-plist nil)
         (org-roam-db-update-on-save nil)
         (enable-local-variables nil)
         (enable-local-eval nil)
         file)
    (setq options (plist-put options :immediate-finish t)
          options (plist-put options :jump-to-captured nil)
          options (plist-put options :no-save nil)
          options (plist-put options :before-finalize
                             (lambda ()
                               (save-excursion
                                 (goto-char (point-min))
                                 (org-entry-put nil "ROAM_REFS" (plist-get info :url))
                                 (goto-char (point-min))
                                 (re-search-forward "^:END:[ \t]*$")
                                 (forward-line 1)
                                 (unless (save-excursion (re-search-forward "^#\\+TITLE:" nil t))
                                   (insert "#+TITLE: " title "\n"))
                                 (goto-char (point-max))
                                 (unless (bolp) (insert "\n"))
                                 (insert "\n" (plist-get info :body))
                                 (unless (bolp) (insert "\n"))))))
    (condition-case nil
        (cl-letf (((symbol-function 'org-roam-capture--target-truepath)
                   (lambda (path)
                     (setq file (loki-assistant-doc--new-file (funcall resolve path)))))
                  ((symbol-function 'org-roam-capture--fill-template)
                   (lambda (value &optional ensure-newline)
                     ;; 标题中的模板语法在展开结束后才写入。
                     (let ((value (if (functionp value) (funcall value) value)))
                       (replace-regexp-in-string
                        (regexp-quote title-marker) (lambda (_) title)
                        (funcall fill-template (string-replace "${title}" title-marker value)
                                 ensure-newline)
                        t t)))))
          (org-roam-capture- :keys (car template) :node node
                             :templates (list (append (seq-take template 3) '("%?") options))))
      (error
       (when (and file (file-exists-p file))
         (puthash key (cons file (org-roam-node-id node)) loki-assistant-doc--unindexed))
       (loki-assistant-doc--fail "capture-save-failed")))
    (unless (and file (file-regular-p file))
      (loki-assistant-doc--fail "capture-save-failed"))
    (loki-assistant-doc--index key file (org-roam-node-id node))))

(defun loki-assistant-doc-capture-full (target info)
  "将 INFO 中的网页全文保存到 TARGET，返回含 result 或 error 的结果。"
  (catch 'loki-assistant-doc-error
    (let ((key (cdr (assoc target loki-assistant-doc--targets))))
      (unless key (loki-assistant-doc--fail "capture-target-not-found"))
      (loki-assistant-doc--validate-capture info)
      (loki-assistant-doc--require-roam)
      (unless (ignore-errors (require 'org-roam-capture nil t))
        (loki-assistant-doc--fail "doc-unavailable"))
      (condition-case nil
          (let* ((url (plist-get info :url))
                 (source (loki-assistant-doc--url-key url))
                 (source-key (cons (file-truename org-roam-directory) source))
                 (pending (gethash source-key loki-assistant-doc--unindexed))
                 (existing (or (when pending
                                 (if (file-regular-p (car pending))
                                     (loki-assistant-doc--index source-key (car pending) (cdr pending))
                                   (remhash source-key loki-assistant-doc--unindexed)
                                   nil))
                               (loki-assistant-doc--existing-article url source)))
                 (template (assoc key loki-assistant-doc-capture-templates))
                 (node (or existing
                           (progn
                             (unless (and (eq (nth 2 template) 'plain)
                                          (eq (car (plist-get (nthcdr 4 template) :target)) 'file+head))
                               (loki-assistant-doc--fail "capture-template-unavailable"))
                             (loki-assistant-doc--write-article template info source-key)))))
            (list (cons 'result (list (cons 'status (if existing "existing" "saved"))
                                      (cons 'url url)
                                      (cons 'document (loki-assistant-doc--summary node))))))
        (error (loki-assistant-doc--fail "capture-index-failed"))))))

(defun loki-assistant-doc-request (file)
  "处理 gateway 暂存于 FILE 的 JSON 请求，返回 Base64 编码的 JSON。"
  (let ((reply
         (condition-case nil
             (with-temp-buffer
               (insert-file-contents file)
               (let ((request (json-parse-buffer :object-type 'plist)))
                 (pcase (plist-get request :operation)
                   ("get" (loki-assistant-doc-get (plist-get request :id)))
                   ("capture"
                    (let ((info (plist-get request :info)))
                      (loki-assistant-doc-capture-full
                       (plist-get request :target)
                       (list :title (plist-get info :title)
                             :url (plist-get info :url)
                             :captured-at (plist-get info :captured_at)
                             :body (plist-get info :body)))))
                   ("capture_read_later"
                    (loki-assistant-read-later-capture (plist-get request :info)))
                   ("list_read_later" (loki-assistant-read-later-list))
                   ("mark_read_later_read"
                    (loki-assistant-read-later-mark-read (plist-get request :id)))
                   ("delete_read_later"
                    (loki-assistant-read-later-delete (plist-get request :id)))
                   (_ '((error . "invalid-request"))))))
           (error '((error . "invalid-request"))))))
    (base64-encode-string (encode-coding-string (json-serialize reply) 'utf-8) t)))

(provide 'loki-assistant-doc)
;;; loki-assistant-doc.el ends here
