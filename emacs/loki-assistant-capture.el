;;; loki-assistant-capture.el --- services 网页全文捕获 -*- lexical-binding: t; -*-

;;; Commentary:

;; 接收 services 通过 emacsclient gateway 转交的浏览器 Org 文本。
;; 本模块不启动 HTTP 服务，也不修改用户的 org-roam-capture-templates。

;;; Code:

(require 'cl-lib)
(require 'json)
(require 'org-roam)
(require 'org-roam-capture)
(require 'seq)
(require 'subr-x)
(require 'url-parse)
(require 'url-util)

(defgroup loki-assistant-capture nil
  "Loki Assistant 的网页全文捕获。"
  :group 'loki-assistant)

(defcustom loki-assistant-capture-templates
  nil
  "Loki Assistant 全文捕获使用的 org-roam 模板。
模板结构与 `org-roam-capture-templates' 相同，只由本模块传给单次
`org-roam-capture-' 调用，不写入 org-roam 的全局模板变量。全文捕获固定
使用 key 为 `f' 的模板。"
  :type '(repeat sexp)
  :group 'loki-assistant-capture)

(defconst loki-assistant-capture--max-body-bytes (* 16 1024 1024))
(defconst loki-assistant-capture--template-key "f")
(defvar loki-assistant-capture--pending (make-hash-table :test #'equal))

(defun loki-assistant-capture--json (object)
  "将 OBJECT 编码成紧凑 JSON 字符串。"
  (let ((json-encoding-pretty-print nil))
    (json-serialize object)))

(defun loki-assistant-capture--error (code message)
  "返回带 CODE 和 MESSAGE 的捕获错误 JSON。"
  (loki-assistant-capture--json
   `((error . ((code . ,code) (message . ,message))))))

(defun loki-assistant-capture--success (status url &optional document)
  "返回 STATUS、URL 和可选 DOCUMENT 的成功 JSON。"
  (loki-assistant-capture--json
   `((status . ,status) (url . ,url)
     ,@(when document `((document . ,document))))))

(defun loki-assistant-capture--required-string (data key)
  "从 DATA 读取 KEY 的非空字符串，否则返回 nil。"
  (let ((value (gethash key data)))
    (and (stringp value)
         (not (string-empty-p (string-trim value)))
         value)))

(defun loki-assistant-capture--parse (text)
  "校验服务传入的 JSON TEXT，返回捕获字段 plist。"
  (let ((data (condition-case nil
                  (json-parse-string text :object-type 'hash-table)
                (error nil))))
    (unless (hash-table-p data)
      (user-error "capture 请求不是有效 JSON 对象"))
    (maphash
     (lambda (key _)
       (unless (member key '("title" "url" "captured_at" "body"))
         (user-error "capture 请求包含未知字段：%s" key)))
     data)
    (dolist (key '("title" "url" "captured_at" "body"))
      (unless (loki-assistant-capture--required-string data key)
        (user-error "capture 字段 %s 必须是非空字符串" key)))
    (let ((title (gethash "title" data))
          (url (gethash "url" data))
          (captured-at (gethash "captured_at" data))
          (body (gethash "body" data)))
      (when (or (string-match-p "[\r\n]" title)
                (string-match-p "[[:space:]]" url)
                (not (string-match-p "\\`https?://[^[:space:]]+\\'" url))
                (not (string-match-p
                      "\\`[0-9]\\{4\\}-[0-9]\\{2\\}-[0-9]\\{2\\}T[0-9]\\{2\\}:[0-9]\\{2\\}:[0-9]\\{2\\}\\(?:\\.[0-9]+\\)?Z\\'"
                      captured-at))
                (not (ignore-errors (date-to-time captured-at)))
                (> (string-bytes body) loki-assistant-capture--max-body-bytes))
        (user-error "capture 请求字段格式无效"))
      (list :title title :url url :captured-at captured-at :body body))))

(defun loki-assistant-capture--url-key (url)
  "统一 URL 中的 Unicode 与百分号编码大小写，保留 URL 分隔符。"
  (replace-regexp-in-string
   "%[[:xdigit:]]\\{2\\}" #'upcase
   (mapconcat (lambda (char)
                (if (> char 127) (url-hexify-string (char-to-string char))
                  (char-to-string char))) url "") t t))

(defun loki-assistant-capture--find-document (url)
  "查询 URL 对应的 org-roam 文档，核对原始引用。"
  (let* ((parsed (url-generic-parse-url url))
         (type (url-type parsed))
         (path (progn (setf (url-type parsed) nil)
                      (org-link-decode (url-recreate-url parsed))))
         (rows (org-roam-db-query
                [:select [nodes:id nodes:file nodes:title nodes:properties]
                 :from refs :inner-join nodes :on (= refs:node-id nodes:id)
                 :where (and (= refs:type $s1) (= refs:ref $s2))] type path))
         (row (seq-find
               (lambda (item)
                 (seq-some (lambda (ref)
                             (equal (loki-assistant-capture--url-key url)
                                    (loki-assistant-capture--url-key ref)))
                           (split-string-and-unquote
                            (or (cdr (assoc "ROAM_REFS" (nth 3 item))) ""))))
               rows)))
    (when row
      (unless (file-exists-p (nth 1 row))
        (user-error "org-roam 索引中的文件已不存在，请先同步索引"))
      `((id . ,(nth 0 row))
        (title . ,(nth 2 row))
        (relative_path . ,(file-relative-name (nth 1 row) org-roam-directory))))))

(defun loki-assistant-capture--activate ()
  "激活一个已有的 Emacs 图形窗口。"
  (let ((frame (seq-find (lambda (item) (display-graphic-p item)) (frame-list))))
    (unless frame
      (user-error "请先打开 Emacs 图形窗口，再重新提交"))
    (select-frame-set-input-focus frame)
    (raise-frame frame)))

(defun loki-assistant-capture--template-key ()
  "返回全文模板的 key。"
  (unless (and (listp loki-assistant-capture-templates)
               (assoc loki-assistant-capture--template-key
                      loki-assistant-capture-templates))
    (user-error "loki-assistant-capture-templates 缺少 key 为 f 的全文模板"))
  loki-assistant-capture--template-key)

(defun loki-assistant-capture--capture (data)
  "将 DATA 按字面插入 Loki Assistant 的 org-roam 捕获缓冲区。"
  (unless (and (listp loki-assistant-capture-templates)
               loki-assistant-capture-templates)
    (user-error "loki-assistant-capture-templates 不能为空"))
  (unless (and (boundp 'org-roam-directory)
               (stringp org-roam-directory)
               (file-directory-p org-roam-directory)
               (file-writable-p org-roam-directory))
    (user-error "org-roam-directory 不存在或不可写"))
  (let* ((org-capture-link-is-already-stored t)
         (org-store-link-plist (list :initial ""))
         (title (plist-get data :title))
         (title-marker (concat "loki-assistant-capture-title-" (org-id-new)))
         (fill-template (symbol-function 'org-roam-capture--fill-template)))
    ;; 标题交给用户配置的 Loki 模板；替换发生在模板展开后，避免标题中的
    ;; %()、${} 等文本被当作模板表达式求值。
    (cl-letf (((symbol-function 'org-roam-capture--fill-template)
               (lambda (template &optional ensure-newline)
                 (replace-regexp-in-string
                  (regexp-quote title-marker) (lambda (_) title)
                  (funcall fill-template
                           (if (stringp template)
                               (string-replace "${title}" title-marker template)
                             template)
                           ensure-newline)
                  t t))))
      (org-roam-capture-
       :keys (loki-assistant-capture--template-key)
       :node (org-roam-node-create :title title)
       :templates loki-assistant-capture-templates))
    (make-directory (file-name-directory (buffer-file-name (buffer-base-buffer))) t)
    (org-capture-put :loki-assistant-capture-url (plist-get data :url)
                     :loki-assistant-capture-captured-at
                     (plist-get data :captured-at))
    (setq org-capture-current-plist org-capture-plist)
    ;; 网页内容不经过模板求值，保留 %()、${...} 等字面文本。
    (let ((start (copy-marker (point))))
      (insert (plist-get data :body) "\n")
      (save-excursion
        (goto-char (point-min))
        (org-entry-put nil "ROAM_REFS" (plist-get data :url)))
      (goto-char start)
      (set-marker start nil)))
  (current-buffer))

(defun loki-assistant-capture--submit (data)
  "按 URL 去重并处理 DATA，返回成功 JSON。"
  (let* ((url (plist-get data :url))
         (key (loki-assistant-capture--url-key url))
         (state (gethash key loki-assistant-capture--pending)))
    (cond
     ((plist-get state :error)
      (loki-assistant-capture--error
       "index-failed" "该来源仍有未解决的保存或索引错误，请在 Emacs 中检查。"))
     (state
      (unless (buffer-live-p (plist-get state :buffer))
        (user-error "该来源的捕获状态待处理，请在 Emacs 中检查"))
      (loki-assistant-capture--activate)
      (pop-to-buffer-same-window (plist-get state :buffer))
      (loki-assistant-capture--success "pending" url))
     (t
      (unless (and (boundp 'org-roam-directory)
                   (stringp org-roam-directory)
                   (file-directory-p org-roam-directory)
                   (file-writable-p org-roam-directory))
        (user-error "org-roam-directory 不存在或不可写"))
      (let ((document (loki-assistant-capture--find-document url)))
        (if document
            (progn
              (loki-assistant-capture--activate)
              (org-roam-node-visit
               (org-roam-populate
                (org-roam-node-create :id (cdr (assq 'id document)))))
              (loki-assistant-capture--success "existing" url document))
          (when (seq-some (lambda (buffer)
                            (and (buffer-live-p buffer)
                                 (buffer-local-value 'org-capture-mode buffer)))
                          (buffer-list))
            (user-error "请先在 Emacs 中完成或取消当前捕获"))
          (puthash key (list :buffer nil) loki-assistant-capture--pending)
          (condition-case err
              (progn
                (loki-assistant-capture--activate)
                (let* ((buffer (loki-assistant-capture--capture data))
                       (file (buffer-file-name (buffer-base-buffer buffer))))
                  (puthash key (list :buffer buffer :file file)
                           loki-assistant-capture--pending))
                (loki-assistant-capture--success "pending" url))
            (error
             (remhash key loki-assistant-capture--pending)
             (signal (car err) (cdr err))))))))))

(defun loki-assistant-capture--after-finalize ()
  "在 org-roam 完成保存或取消后更新索引并清理来源状态。"
  (when-let* ((url (org-capture-get :loki-assistant-capture-url))
              (key (loki-assistant-capture--url-key url))
              (state (gethash key loki-assistant-capture--pending)))
    (let ((file (plist-get state :file)))
      (condition-case err
          (progn
            (when (and file (file-exists-p file)) (org-roam-db-update-file file))
            (remhash key loki-assistant-capture--pending))
        (error
         (puthash key (plist-put state :error (error-message-string err))
                  loki-assistant-capture--pending)
         (display-warning
          'loki-assistant-capture
          (format "索引更新失败，请修复后同步 org-roam：%s"
                  (error-message-string err))
          :error))))))

(defun loki-assistant-capture-full (request-json)
  "处理 services 转交的浏览器全文捕获 REQUEST-JSON。
成功返回 JSON 字符串；业务错误也编码为 JSON，避免 gateway 将用户可处理的
捕获冲突转换成无法区分的 Lisp 错误。"
  (condition-case err
      (loki-assistant-capture--submit
       (loki-assistant-capture--parse request-json))
    (user-error
     (let ((message (error-message-string err)))
       (loki-assistant-capture--error
        (cond
         ((string-match-p "图形窗口" message) "emacs-ui-unavailable")
         ((string-match-p "完成或取消当前捕获" message) "capture-busy")
         ((string-match-p "org-roam-directory\|模板\|不可写" message)
          "capture-invalid")
         (t "capture-invalid"))
        message)))
    (error
     (loki-assistant-capture--error
      "capture-failed" (error-message-string err)))))

(add-hook 'org-capture-after-finalize-hook
          #'loki-assistant-capture--after-finalize 90)

(provide 'loki-assistant-capture)
;;; loki-assistant-capture.el ends here
