;;; loki-assistant-config-tests.el --- 配置界面验证 -*- lexical-binding: t; -*-

;;; Commentary:

;; 使用临时 HTTP 服务和配置 buffer；不修改用户配置或真实 LaunchAgent。

;;; Code:

(require 'loki-assistant-tests)
(require 'loki-assistant-config)

(defconst loki-assistant-config-test--meta
  "{\"modules\":[{\"id\":\"empty\",\"name\":\"空模块\",\"items\":[]},{\"id\":\"test\",\"name\":\"测试模块\",\"description\":\"测试说明\",\"items\":[{\"key\":\"text\",\"name\":\"文本\",\"data_type\":\"string\"},{\"key\":\"flag\",\"name\":\"开关\",\"data_type\":\"boolean\"},{\"key\":\"object\",\"name\":\"字典\",\"data_type\":\"dictionary\"},{\"key\":\"list\",\"name\":\"列表\",\"data_type\":\"list\"},{\"key\":\"missing\",\"name\":\"程序\",\"data_type\":\"executable\"}]}]}")

(defconst loki-assistant-config-test--data
  "{\"module_id\":\"test\",\"values\":{\"text\":{\"value\":\"原值\",\"source\":\"default\"},\"flag\":{\"value\":false,\"source\":\"default\"},\"object\":{\"value\":{},\"source\":\"user\"},\"list\":{\"value\":[],\"source\":\"user\"}}}")

(defun loki-assistant-config-test--wait (predicate)
  "等待 PREDICATE 成立，最多三秒。"
  (let ((deadline (+ (float-time) 3)))
    (while (and (not (funcall predicate)) (< (float-time) deadline))
      (accept-process-output nil 0.01))
    (should (funcall predicate))))

(defmacro loki-assistant-config-test--with-buffer (&rest body)
  "在临时配置 buffer 中执行 BODY。"
  (declare (indent 0))
  `(with-temp-buffer
     (loki-assistant-config-mode)
     (setq loki-assistant-config--module-id "test"
           loki-assistant-config--generation (list nil))
     (unwind-protect (progn ,@body)
       (loki-assistant-config--stop-wait-timer))))

(defun loki-assistant-config-test--module ()
  "返回测试模块元数据。"
  (aref (alist-get 'modules (json-parse-string loki-assistant-config-test--meta
                                             :object-type 'alist)) 1))

(defun loki-assistant-config-test--fixture ()
  "绘制配置快照，用于控件和保存测试。"
  (loki-assistant-config--render
   (loki-assistant-config-test--module)
   (alist-get 'values (json-parse-string loki-assistant-config-test--data
                                        :object-type 'alist :false-object :false))))

(defun loki-assistant-config-test--entry (key)
  "查找 KEY 对应控件。"
  (cl-find key loki-assistant-config--entries :test #'equal
           :key (lambda (entry) (alist-get 'key (loki-assistant-config--entry-item entry)))))

(ert-deftest loki-assistant-config-test-shortcuts-preserve-text-input ()
  (loki-assistant-config-test--with-buffer
    (loki-assistant-config-test--fixture)
    (goto-char (point-min))
    (should (eq (key-binding (kbd "?")) #'describe-mode))
    (should (eq (key-binding (kbd "C-c C-q")) #'quit-window))
    (should (string-match-p (regexp-quote "? 帮助")
                            (loki-assistant-config--mode-line-keys)))
    (let ((field (loki-assistant-config--entry-field (loki-assistant-config-test--entry "text"))))
      (goto-char (widget-field-start field))
      (should (eq (key-binding (kbd "?")) #'self-insert-command))
      (should (eq (key-binding (kbd "C-c ?")) #'describe-mode))
      (should (eq (key-binding (kbd "C-c C-q")) #'quit-window))
      (should (eq (key-binding (kbd "C-c C-c")) #'loki-assistant-config-save))
      (should (string-match-p (regexp-quote "C-c ? 帮助")
                              (loki-assistant-config--mode-line-keys))))))

(ert-deftest loki-assistant-config-test-load-real-http ()
  (loki-assistant-test--with-server
      (lambda (headers _body)
        (cond
         ((string-prefix-p "GET /meta/test " headers)
          (list 200 (loki-assistant-config--json (loki-assistant-config-test--module))))
         ((string-prefix-p "GET /data/test " headers)
          (list 200 loki-assistant-config-test--data))
         ((string-prefix-p "GET /editing/test " headers)
          (list 200 "{\"module_id\":\"test\",\"data\":{},\"monitor_states\":[]}"))
         (t (list 404 "{}"))))
    (loki-assistant-config-test--with-buffer
      (loki-assistant-config--load)
      (loki-assistant-config-test--wait (lambda () (not loki-assistant-config--loading)))
      (should (= (length loki-assistant-config--entries) 5))
      (should (string-match-p "测试模块" (buffer-string)))
      (should-not (string-match-p "空模块" (buffer-string)))
      (should (equal (widget-value (loki-assistant-config--entry-field
                                    (loki-assistant-config-test--entry "text"))) "原值"))
      (should (eq (loki-assistant-config--value (loki-assistant-config-test--entry "flag")) :false))
      (should (string-match-p "未设置" (buffer-string)))
      (should (string-match-p (regexp-quote "[撤销编辑] 默认值\n") (buffer-string)))
      (should (string-match-p (regexp-quote "[撤销编辑] 用户设置，已保存\n") (buffer-string))))))

(ert-deftest loki-assistant-config-test-picker-keeps-module-buffers-independent ()
  (let* ((alpha '((id . "config-test-alpha") (name . "甲")
                  (items . [((key . "text") (name . "字段") (data_type . "string"))])))
         (beta '((id . "config-test-beta") (name . "乙")
                 (items . [((key . "text") (name . "字段") (data_type . "string"))])))
         (empty '((id . "empty") (name . "空模块") (items . [])))
         (selections '("甲 · config-test-alpha" "乙 · config-test-beta"
                       "甲 · config-test-alpha" "乙 · config-test-beta"))
         requests alpha-buffer beta-buffer beta-updated)
    (save-window-excursion
      (unwind-protect
          (cl-letf (((symbol-function 'completing-read)
                     (lambda (_prompt choices &rest _)
                       (should (equal (mapcar #'car choices)
                                      '("甲 · config-test-alpha" "乙 · config-test-beta")))
                       (pop selections)))
                    ((symbol-function 'loki-assistant-request)
                     (lambda (_method path &rest args)
                       (push path requests)
                       (funcall
                        (plist-get args :callback)
                        (list :ok t :data
                              (pcase path
                                ("/meta" (list (cons 'modules (vector alpha beta empty))))
                                ("/meta/config-test-alpha" alpha)
                                ((or "/editing/config-test-alpha" "/editing/config-test-beta")
                                 `((module_id . ,(substring path (length "/editing/")))
                                   (data . nil) (monitor_states . [])))
                                ((or "/data/config-test-alpha" "/data/config-test-beta")
                                 (let* ((id (substring path (length "/data/")))
                                        (value (if (and beta-updated (equal id "config-test-beta"))
                                                   "乙的新值"
                                                 id)))
                                   `((module_id . ,id)
                                     (values . ((text . ((value . ,value) (source . "user"))))))))
                                (_ (error "意外的配置请求 %s" path))))))))
            (loki-assistant-config)
            (setq alpha-buffer (window-buffer))
            (with-current-buffer alpha-buffer
              (should (equal loki-assistant-config--module-id "config-test-alpha"))
              (widget-value-set (loki-assistant-config--entry-field
                                 (loki-assistant-config-test--entry "text")) "甲的草稿"))
            (loki-assistant-config)
            (setq beta-buffer (window-buffer))
            (with-current-buffer beta-buffer
              (should (equal loki-assistant-config--module-id "config-test-beta"))
              (should (equal (widget-value (loki-assistant-config--entry-field
                                            (loki-assistant-config-test--entry "text")))
                             "config-test-beta")))
            (loki-assistant-config)
            (should (eq (window-buffer) alpha-buffer))
            (with-current-buffer alpha-buffer
              (should (equal (widget-value (loki-assistant-config--entry-field
                                            (loki-assistant-config-test--entry "text")))
                             "甲的草稿")))
            (setq beta-updated t
                  beta '((id . "config-test-beta") (name . "乙")
                         (items . [((key . "text") (name . "新字段") (data_type . "string"))])))
            (loki-assistant-config)
            (should (eq (window-buffer) beta-buffer))
            (with-current-buffer beta-buffer
              (should (string-match-p "新字段" (buffer-string)))
              (should (equal (widget-value (loki-assistant-config--entry-field
                                            (loki-assistant-config-test--entry "text")))
                             "乙的新值")))
            (should (equal (reverse requests)
                           '("/meta" "/data/config-test-alpha" "/editing/config-test-alpha"
                             "/meta" "/data/config-test-beta" "/editing/config-test-beta"
                             "/meta" "/meta" "/data/config-test-beta" "/editing/config-test-beta"))))
        (when (buffer-live-p alpha-buffer) (kill-buffer alpha-buffer))
        (when (buffer-live-p beta-buffer) (kill-buffer beta-buffer))))))

(ert-deftest loki-assistant-config-test-read-only-and-checker-controls ()
  (loki-assistant-config-test--with-buffer
    (loki-assistant-config--render
     '((id . "network") (name . "网络")
        (items . [((key . "relay_token") (name . "中转令牌")
                   (data_type . ((password . ((path . "loki-assistant/network/relay.gpg")))))
                   (read_only . t))
                  ((key . "plain") (name . "普通字段") (data_type . "string") (checker . :null))
                  ((key . "checked") (name . "带校验字段") (data_type . "string")
                   (checker . ((pattern . "^T[0-9]+$") (emacs_pattern . "\\`T[0-9]+\\'"))))]))
     '((relay_token . ((value . :false)))))
    (should-not (loki-assistant-config-test--entry "relay_token"))
    (should (string-match-p "中转令牌：未设置  只读" (buffer-string)))
    (dolist (key '("plain" "checked"))
      (let ((labels (mapcar #'widget-value
                            (loki-assistant-config--entry-buttons
                             (loki-assistant-config-test--entry key)))))
        (should-not (member "校验" labels))
        (should (member "保存" labels))))
    (let* ((entry (loki-assistant-config-test--entry "checked"))
           (field (loki-assistant-config--entry-field entry)))
      (widget-value-set field "t123")
      (widget-apply field :notify)
      (should (eq (widget-get field :value-face) 'loki-assistant-config-invalid-field))
      (widget-value-set field "T123")
      (widget-apply field :notify)
      (should (eq (widget-get field :value-face) 'widget-field)))))

(ert-deftest loki-assistant-config-test-file-chooser-accepts-null-metadata ()
  (loki-assistant-config-test--with-buffer
    (let ((default-directory temporary-file-directory)
          (directory (make-temp-file "loki-config-chooser-" t)))
      (unwind-protect
          (progn
            (loki-assistant-config--render-entry
             "test" '((key . "store") (name . "目录") (data_type . "file_path")
                      (file_path (directory . t)) (executable . :null)) nil)
            (loki-assistant-config--render-entry
             "test" `((key . "program") (name . "程序") (data_type . "executable")
                      (file_path . :null) (executable (search_paths . [,directory]))) nil)
            (widget-setup)
            (cl-letf (((symbol-function 'read-directory-name)
                       (lambda (_prompt start &rest _)
                         (should (equal start default-directory))
                         directory))
                      ((symbol-function 'read-file-name)
                       (lambda (_prompt start &rest _)
                         (should (equal start directory))
                         (expand-file-name "Example.app/Contents/MacOS/example" directory))))
              (dolist (case `(("store" . ,directory)
                              ("program" . ,(expand-file-name "Example.app/Contents/MacOS/example" directory))))
                (let* ((entry (loki-assistant-config-test--entry (car case)))
                       (button (cl-find "选择文件…" (loki-assistant-config--entry-buttons entry)
                                        :key #'widget-value :test #'equal)))
                  (should button)
                  (widget-apply-action button)
                  (should (equal (loki-assistant-config--value entry) (cdr case)))
                  (should (loki-assistant-config--dirty-p entry))))))
        (delete-directory directory)))))

(ert-deftest loki-assistant-config-test-json-roundtrip ()
  (loki-assistant-config-test--with-buffer
    (loki-assistant-config-test--fixture)
    (should (equal (loki-assistant-config--json
                    (loki-assistant-config--value (loki-assistant-config-test--entry "object"))) "{}"))
    (should (equal (loki-assistant-config--json
                    (loki-assistant-config--value (loki-assistant-config-test--entry "list"))) "[]"))
    (dolist (type '("integer" "number"))
      (let ((entry (loki-assistant-config-test--entry "object")))
        (setf (alist-get 'data_type (loki-assistant-config--entry-item entry)) type)
        (widget-value-set (loki-assistant-config--entry-field entry) "12.5")
        (should (= (loki-assistant-config--value entry) 12.5))))))

(ert-deftest loki-assistant-config-test-save-real-http-and-errors ()
  (let (requests save-ok)
    (loki-assistant-test--with-server
        (lambda (headers body)
          (push (list headers (decode-coding-string body 'utf-8)) requests)
          (if save-ok
              (list 200 (loki-assistant-config--json
                         '((module_id . "test") (key . "text") (value . "新值") (source . "user"))))
            (list 400 (loki-assistant-config--json
                       '((message . "保存失败") (code . "invalid-config"))))))
      (loki-assistant-config-test--with-buffer
        (loki-assistant-config-test--fixture)
        (let ((entry (loki-assistant-config-test--entry "text")))
          (widget-value-set (loki-assistant-config--entry-field entry) "新值")
          (should (loki-assistant-config--dirty-p entry))
          (loki-assistant-config--submit entry)
          (loki-assistant-config-test--wait (lambda () (not (loki-assistant-config--entry-busy entry))))
          (should (equal (loki-assistant-config--entry-error entry) "保存失败"))
          (should (= (length requests) 1))
          (should (equal (widget-value (loki-assistant-config--entry-field entry)) "新值"))
          (should (equal (loki-assistant-config--entry-source entry) "default"))
          (setq save-ok t)
          (loki-assistant-config--submit entry)
          (loki-assistant-config-test--wait (lambda () (not (loki-assistant-config--entry-busy entry))))
          (should-not (loki-assistant-config--entry-error entry))
          (should-not (loki-assistant-config--dirty-p entry))
          (should (equal (loki-assistant-config--entry-source entry) "user"))
          (should (= (length requests) 2))
          (should (string-prefix-p "PUT /editing/test/text " (caar requests)))
          (should (equal (json-parse-string (cadar requests)) "新值")))))))

(ert-deftest loki-assistant-config-test-late-save-keeps-new-input ()
  (loki-assistant-config-test--with-buffer
    (loki-assistant-config-test--fixture)
    (let ((entry (loki-assistant-config-test--entry "text")) pending)
      (cl-letf (((symbol-function 'loki-assistant-request)
                 (lambda (_method _path &rest args) (push (plist-get args :callback) pending))))
        (widget-value-set (loki-assistant-config--entry-field entry) "提交值")
        (loki-assistant-config--submit entry)
        (widget-value-set (loki-assistant-config--entry-field entry) "继续编辑")
        (funcall (pop pending)
                 '(:ok t :data ((module_id . "test") (key . "text") (value . "规范化") (source . "user"))))
        (should (equal (widget-value (loki-assistant-config--entry-field entry)) "继续编辑"))
        (should (equal (loki-assistant-config--entry-baseline entry) "规范化"))
        (should (loki-assistant-config--dirty-p entry))))))

(ert-deftest loki-assistant-config-test-load-atomic-failure-and-stale ()
  (loki-assistant-config-test--with-buffer
    (let (pending)
      (cl-letf (((symbol-function 'loki-assistant-request)
                 (lambda (_method path &rest args)
                   (push (cons path (plist-get args :callback)) pending))))
        (loki-assistant-config--load)
        (let ((old (cdar pending)))
          (loki-assistant-config--load)
          (funcall old '(:ok nil :error-message "过期失败"))
          (should-not (string-match-p "过期失败" (buffer-string))))
        (should (equal (caar pending) "/meta/test"))
        (funcall (cdar pending) (list :ok t :data (loki-assistant-config-test--module)))
        (should loki-assistant-config--loading)
        (should-not loki-assistant-config--entries)
        (should (equal (caar pending) "/data/test"))
        (funcall (cdar pending) '(:ok nil :error-message "无法读取测试模块"))
        (should-not loki-assistant-config--loading)
        (should-not loki-assistant-config--entries)
        (should (string-match-p "无法读取测试模块" (buffer-string)))))))

(ert-deftest loki-assistant-config-test-refresh-and-invalid-json ()
  (loki-assistant-config-test--with-buffer
    (loki-assistant-config-test--fixture)
    (let* ((entry (loki-assistant-config-test--entry "object"))
           (field (loki-assistant-config--entry-field entry))
           requests)
      (widget-value-set field "{invalid")
      (widget-apply field :notify)
      (should (eq (widget-get field :value-face) 'loki-assistant-config-invalid-field))
      (cl-letf (((symbol-function 'loki-assistant-request)
                 (lambda (method path &rest args) (push (list method path args) requests)))
                ((symbol-function 'yes-or-no-p) (lambda (&rest _) nil)))
        (loki-assistant-config--submit entry)
        (should (equal (seq-take (car requests) 2) '(put "/editing/test/object")))
        (should (equal (json-parse-string (plist-get (nth 2 (car requests)) :body)) "{invalid"))
        (loki-assistant-config-test--respond
         (pop requests) '(:ok nil :status 400 :error-message "配置值无效"))
        (should (loki-assistant-config--entry-error entry))
        (should-not (loki-assistant-config--entry-busy entry))))))

(ert-deftest loki-assistant-config-test-save-all-stops-after-failure ()
  (loki-assistant-config-test--with-buffer
    (loki-assistant-config-test--fixture)
    (let ((text (loki-assistant-config-test--entry "text"))
          (flag (loki-assistant-config-test--entry "flag")) requests)
      (widget-value-set (loki-assistant-config--entry-field text) "新值")
      (widget-value-set (loki-assistant-config--entry-field flag) t)
      (cl-letf (((symbol-function 'loki-assistant-request)
                 (lambda (_method path &rest args)
                   (push path requests)
                   (funcall (plist-get args :callback)
                            '(:ok nil :error-message "未通过")))))
        (loki-assistant-config-save)
        (should (equal requests '("/editing/test")))
        (should (loki-assistant-config--dirty-p flag))
        (should (string-match-p "未通过" (buffer-string)))))))

(ert-deftest loki-assistant-config-test-change-handler-failure-preserves-input ()
  (loki-assistant-config-test--with-buffer
    (loki-assistant-config--render
     '((id . "control-center") (name . "控制中心")
        (items . [((key . "center_auto_start") (name . "登录启动") (data_type . "boolean"))]))
     '((center_auto_start . ((value . :false) (source . "default")))))
    (let ((entry (car loki-assistant-config--entries)))
      (widget-value-set (loki-assistant-config--entry-field entry) t)
      (cl-letf (((symbol-function 'loki-assistant-request)
                 (lambda (_method _path &rest args)
                   (funcall (plist-get args :callback)
                            '(:ok nil :error-message "登录启动处理失败：launchctl 失败"
                                  :data ((code . "config-change-rejected")
                                         (message . "登录启动处理失败：launchctl 失败")))))))
        (loki-assistant-config--submit entry)
        (should (equal (loki-assistant-config--entry-source entry) "default"))
        (should (widget-value (loki-assistant-config--entry-field entry)))
        (should (loki-assistant-config--dirty-p entry))
        (should-not (loki-assistant-config--entry-busy entry))
        (should (string-match-p "launchctl 失败" (loki-assistant-config--entry-error entry)))))))

(defun loki-assistant-config-test--respond (request response)
  "将 RESPONSE 交给记录的 REQUEST 回调。"
  (funcall (plist-get (nth 2 request) :callback) response))

(ert-deftest loki-assistant-config-test-waits-through-save-with-new-edits ()
  (loki-assistant-config-test--with-buffer
    (loki-assistant-config-test--fixture)
    (let ((entry (loki-assistant-config-test--entry "text"))
          (loki-assistant-config-save-timeout 120)
          (loki-assistant-request-timeout 10) requests)
      (cl-letf (((symbol-function 'loki-assistant-request)
                 (lambda (method path &rest args)
                   (setq requests (nconc requests (list (list method path args)))))))
        (widget-value-set (loki-assistant-config--entry-field entry) "提交值")
        (loki-assistant-config--submit entry)
        (should (timerp loki-assistant-config--wait-timer))
        (dolist (button (append loki-assistant-config--buttons
                                (loki-assistant-config--entry-buttons entry)))
          (should (widget-get button :inactive)))
        (should-error (loki-assistant-config--submit entry) :type 'user-error)
        (should-error (loki-assistant-config-refresh) :type 'user-error)
        (setf (loki-assistant-config--entry-started-at entry) (- (float-time) 4))
        (loki-assistant-config--show-wait)
        (should (string-match-p "已等待 4 秒" (buffer-string)))
        (widget-value-set (loki-assistant-config--entry-field entry) "新的编辑")
        (widget-apply (loki-assistant-config--entry-field entry) :notify)
        (should (string-match-p "正在执行并保存" (buffer-string)))
        (should (= (plist-get (nth 2 (car requests)) :timeout) loki-assistant-config-save-timeout))
        (should (> loki-assistant-config-save-timeout loki-assistant-request-timeout))
        (loki-assistant-config-test--respond
         (pop requests)
         '(:ok t :data ((module_id . "test") (key . "text") (value . "提交值") (source . "user"))))
        (should (equal (widget-value (loki-assistant-config--entry-field entry)) "新的编辑"))
        (should (equal (loki-assistant-config--entry-baseline entry) "提交值"))
        (should (string-match-p "仍有新的编辑尚未保存" (buffer-string)))
        (should-not loki-assistant-config--wait-timer)
        (should-not (loki-assistant-config--entry-busy entry))
        (dolist (button (append loki-assistant-config--buttons
                                (loki-assistant-config--entry-buttons entry)))
          (should-not (widget-get button :inactive)))))))

(ert-deftest loki-assistant-config-test-failed-save-keeps-new-input-and-allows-retry ()
  (loki-assistant-config-test--with-buffer
    (loki-assistant-config-test--fixture)
    (let ((entry (loki-assistant-config-test--entry "text")) requests)
      (cl-letf (((symbol-function 'loki-assistant-request)
                 (lambda (method path &rest args)
                   (setq requests (nconc requests (list (list method path args)))))))
        (widget-value-set (loki-assistant-config--entry-field entry) "提交值")
        (loki-assistant-config--submit entry)
        (widget-value-set (loki-assistant-config--entry-field entry) "保留新输入")
        (loki-assistant-config-test--respond
         (pop requests) '(:ok nil :error-code "config-change-rejected" :error-message "中转注册失败"))
        (should (string-match-p "中转注册失败" (buffer-string)))
        (should (equal (widget-value (loki-assistant-config--entry-field entry)) "保留新输入"))
        (should (equal (loki-assistant-config--entry-baseline entry) "原值"))
        (should (equal (loki-assistant-config--entry-source entry) "default"))
        (should-not loki-assistant-config--wait-timer)
        (loki-assistant-config--submit entry)
        (should-not (loki-assistant-config--entry-error entry))
        (loki-assistant-config-test--respond
         (pop requests) '(:ok nil :error-message "配置值无效"))
        (should (string-match-p "配置值无效" (buffer-string)))
        (should-not requests)
        (should-not loki-assistant-config--wait-timer)))))

(ert-deftest loki-assistant-config-test-failure-is-red-below-field-and-cleared-by-refresh ()
  (loki-assistant-config-test--with-buffer
    (loki-assistant-config-test--fixture)
    (let ((entry (loki-assistant-config-test--entry "text")) requests)
      (cl-letf (((symbol-function 'loki-assistant-request)
                 (lambda (method path &rest args)
                   (setq requests (nconc requests (list (list method path args))))))
                ((symbol-function 'yes-or-no-p) (lambda (&rest _) t)))
        (widget-value-set (loki-assistant-config--entry-field entry) "失败值")
        (loki-assistant-config--submit entry)
        (loki-assistant-config-test--respond
         (pop requests)
         '(:ok nil :error-code "config-change-rejected" :error-message "中转注册失败"))
        (let ((status (loki-assistant-config--entry-status entry)))
          (should (string-match-p "中转注册失败" (buffer-string)))
          (should (eq (get-text-property (widget-get status :from) 'face)
                      'error))
          (should (> (line-number-at-pos (widget-get status :from))
                     (line-number-at-pos (widget-field-start
                                          (loki-assistant-config--entry-field entry))))))
        (loki-assistant-config-refresh)
        (should (equal (nth 1 (car requests)) "/meta/test"))
        (let* ((json-object-type 'alist) (json-array-type 'vector) (json-key-type 'symbol)
               (meta (loki-assistant-config-test--module)))
          (loki-assistant-config-test--respond (pop requests) (list :ok t :data meta)))
        (let* ((json-object-type 'alist) (json-array-type 'vector) (json-key-type 'symbol)
               (json-false :false)
               (data (json-read-from-string loki-assistant-config-test--data)))
          (loki-assistant-config-test--respond (pop requests) (list :ok t :data data)))
        (loki-assistant-config-test--respond
         (pop requests) '(:ok t :data ((module_id . "test") (data . nil) (monitor_states . []))))
        (should-not (string-match-p "中转注册失败" (buffer-string)))
        (should-not (loki-assistant-config--entry-error
                     (loki-assistant-config-test--entry "text")))))))

(ert-deftest loki-assistant-config-test-unknown-save-requires-refresh-before-retry ()
  (dolist (response '((:ok nil :error-code "timeout" :error-message "请求超时")
                      (:ok nil :error-code "transport-error" :error-message "连接中断")
                      (:ok t :data ((unexpected . t)))
                      (:ok t :data ["unexpected"])))
    (loki-assistant-config-test--with-buffer
      (loki-assistant-config-test--fixture)
      (let ((entry (loki-assistant-config-test--entry "text")) requests)
        (cl-letf (((symbol-function 'loki-assistant-request)
                   (lambda (method path &rest args)
                     (setq requests (nconc requests (list (list method path args))))))
                  ((symbol-function 'yes-or-no-p) (lambda (&rest _) t)))
          (widget-value-set (loki-assistant-config--entry-field entry) "未知结果的值")
          (loki-assistant-config--submit entry)
          (loki-assistant-config-test--respond (pop requests) response)
          (should (string-match-p "保存结果未知" (buffer-string)))
          (should (string-match-p "刷新核对" (buffer-string)))
          (should (loki-assistant-config--entry-uncertain entry))
          (should-not loki-assistant-config--wait-timer)
          (should (equal (widget-value (loki-assistant-config--entry-field entry)) "未知结果的值"))
          (should-error (loki-assistant-config--submit entry) :type 'user-error)
          (should-error (loki-assistant-config-save) :type 'user-error)
          (should-not requests)
          (dolist (button (loki-assistant-config--entry-buttons entry))
            (should (widget-get button :inactive)))
          (loki-assistant-config-refresh)
          (should (equal (nth 1 (car requests)) "/meta/test"))
          (let* ((json-object-type 'alist) (json-array-type 'vector) (json-key-type 'symbol)
                 (meta (loki-assistant-config-test--module)))
            (loki-assistant-config-test--respond (pop requests) (list :ok t :data meta)))
          (let* ((json-object-type 'alist) (json-array-type 'vector) (json-key-type 'symbol)
                 (json-false :false)
                 (data (json-read-from-string loki-assistant-config-test--data)))
            (loki-assistant-config-test--respond (pop requests) (list :ok t :data data)))
        (loki-assistant-config-test--respond
         (pop requests) '(:ok t :data ((module_id . "test") (data . nil) (monitor_states . []))))
          (should-not (loki-assistant-config--entry-uncertain
                       (loki-assistant-config-test--entry "text")))
          (should-not loki-assistant-config--loading))))))

(ert-deftest loki-assistant-config-test-batch-sends-one-request-and-retains-input-on-error ()
  (loki-assistant-config-test--with-buffer
    (loki-assistant-config-test--fixture)
    (let ((text (loki-assistant-config-test--entry "text"))
          (flag (loki-assistant-config-test--entry "flag")) requests)
      (cl-letf (((symbol-function 'loki-assistant-request)
                 (lambda (method path &rest args) (push (list method path args) requests))))
        (widget-value-set (loki-assistant-config--entry-field text) "新值")
        (widget-value-set (loki-assistant-config--entry-field flag) t)
        (loki-assistant-config-save)
        (should (string-match-p "正在保存本模块修改" (buffer-string)))
        (should (= (length requests) 1))
        (should (equal (seq-take (car requests) 2) '(patch "/editing/test")))
        (let ((changes (alist-get 'changes (plist-get (nth 2 (car requests)) :body))))
          (should (= (length changes) 2))
          (should (equal (alist-get 'path (aref changes 0)) ["text"])))
        (loki-assistant-config-test--respond
         (pop requests) '(:ok nil :error-code "invalid-config" :error-message "配置值无效"))
        (should (string-match-p "配置值无效" (buffer-string)))
        (should (loki-assistant-config--dirty-p text))
        (should (loki-assistant-config--dirty-p flag))
        (should-not loki-assistant-config--saving-all)
        (should-not requests)))))

(ert-deftest loki-assistant-config-test-closing-buffer-stops-wait-and-ignores-response ()
  ;; with-temp-buffer 会抑制 buffer hooks，使用普通 buffer 验证真实关闭流程。
  (let ((buffer (generate-new-buffer " *loki-config-close-test*")) request timer)
    (unwind-protect
        (progn
          (with-current-buffer buffer
            (loki-assistant-config-mode)
            (setq loki-assistant-config--generation (list nil))
            (loki-assistant-config-test--fixture)
            (cl-letf (((symbol-function 'loki-assistant-request)
                       (lambda (method path &rest args) (setq request (list method path args)))))
              (loki-assistant-config--submit (loki-assistant-config-test--entry "text"))
              (setq timer loki-assistant-config--wait-timer)
              (should (memq timer timer-list))))
          (kill-buffer buffer)
          (should-not (memq timer timer-list))
          (loki-assistant-config-test--respond
           request '(:ok t :data ((module_id . "test") (key . "text")
                                  (value . "新值") (source . "user")))))
      (when (buffer-live-p buffer) (kill-buffer buffer))
      (when timer (cancel-timer timer)))))

(ert-deftest loki-assistant-config-test-real-http-waits-longer-than-read-timeout ()
  (let ((loki-assistant-request-timeout 0.1)
        (loki-assistant-config-save-timeout 1)
        timers)
    (unwind-protect
        (loki-assistant-test--with-server (lambda (&rest _) nil)
          (set-process-filter
           server
           (lambda (process chunk)
             (loki-assistant-test--filter
              (lambda (_headers _body)
                (push
                   (run-at-time
                    0.2 nil
                    (lambda ()
                      (when (process-live-p process)
                        (let ((body "{\"module_id\":\"test\",\"key\":\"text\",\"value\":\"delayed\",\"source\":\"user\"}"))
                          (process-send-string
                           process
                           (concat (format "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: %d\r\nConnection: close\r\n\r\n"
                                           (string-bytes body)) body))
                          (process-send-eof process)))))
                   timers)
                nil)
              process chunk)))
          (loki-assistant-config-test--with-buffer
            (loki-assistant-config-test--fixture)
            (let ((entry (loki-assistant-config-test--entry "text")))
              (widget-value-set (loki-assistant-config--entry-field entry) "delayed")
              (loki-assistant-config--submit entry)
              (loki-assistant-config-test--wait
               (lambda () (eq (loki-assistant-config--entry-busy entry) 'saving)))
              (should (string-match-p "正在执行并保存" (buffer-string)))
              (loki-assistant-config-test--wait
               (lambda () (not (loki-assistant-config--entry-busy entry))))
              (should (string-match-p "保存成功" (buffer-string)))
              (should-not (loki-assistant-config--entry-uncertain entry))
              (should-not (loki-assistant-config--entry-error entry))
              (should-not loki-assistant-config--wait-timer))))
      (mapc #'cancel-timer timers))))

(ert-deftest loki-assistant-config-test-record-form-uses-declared-fields ()
  (let ((item
         '((key . "records") (data_type . "list") (name . "设置")
           (requires_local_auth . t) (save_timeout_seconds . 650)
           (actions . [((id . "old-action") (label . "旧业务操作"))])
           (structure . ((id_field . "slug") (label_field . "name")
                         (add_label . "新增记录")
                         (fields . [((key . "team_id") (label . "工作区") (required . t))
                                    ((key . "slug") (label . "标识") (required . t)
                                     (default_template . "{team_id:lowercase}"))
                                    ((key . "name") (label . "名称") (required . t))])))))
        write)
    (with-temp-buffer
      (loki-assistant-config-mode)
      (cl-letf (((symbol-function 'loki-assistant--module-api-token) (lambda (_) "test-token"))
                ((symbol-function 'loki-assistant-request)
                 (lambda (method path &rest args)
                   (should-not (equal method "GET"))
                   (setq write (list method path args)))))
        (widget-insert "其他配置之前\n")
        (loki-assistant-config--record-insert
         "example" item
         '((records . ((value . [((slug . "alpha") (name . "现有记录") (team_id . "TALPHA"))])))))
        (widget-insert "其他配置之后\n")
        (let ((section (car loki-assistant-config--record-sections)))
          (should (string-match-p "现有记录" (buffer-string)))
          (should-not (string-match-p "旧业务操作" (buffer-string)))
          (loki-assistant-config--record-edit section nil)
          (let ((inputs (loki-assistant-config--record-section-inputs section)))
            (widget-value-set (cdr (assoc "team_id" inputs)) "TBETA")
            (widget-value-set (cdr (assoc "name" inputs)) "待保存"))
          (loki-assistant-config--record-render section)
          (should (equal (widget-value (cdr (assoc "name" (loki-assistant-config--record-section-inputs section))))
                         "待保存"))
          (should (string-prefix-p "其他配置之前\n" (buffer-string)))
          (should (string-suffix-p "其他配置之后\n" (buffer-string)))
          (loki-assistant-config--record-save section)
          (should (equal (cl-subseq write 0 2) '("PUT" "/editing/example/records/tbeta")))
          (should (equal (plist-get (nth 2 write) :headers) '(("If-None-Match" . "*") ("Authorization" . "Bearer test-token"))))
          (should (equal (plist-get (nth 2 write) :timeout) 650))
          (let ((value (plist-get (nth 2 write) :body)))
            (should (equal (gethash "name" value) "待保存"))
            (should-not (gethash "slug" value)))
          (funcall (plist-get (nth 2 write) :callback)
                   '(:ok nil :error-message "记录已存在"))
          (should (string-match-p "记录已存在" (buffer-string)))
          (should (equal (widget-value (cdr (assoc "name" (loki-assistant-config--record-section-inputs section))))
                         "待保存")))))))

(ert-deftest loki-assistant-config-test-inline-record-uses-versioned-snapshot ()
  (loki-assistant-config-test--with-buffer
    (let* ((module '((id . "test")
                     (items . [((key . "teams") (name . "工作区") (data_type . "list")
                                (structure . ((id_field . "team_id")
                                              (fields . [((key . "team_id") (label . "Team ID") (data_type . "key"))
                                                         ((key . "name") (label . "名称") (data_type . "string") (monitor . "team"))
                                                         ((key . "token") (label . "凭据") (data_type . "password") (monitor . "team"))]))))])))
           (snapshot '(:ok t :data ((module_id . "test")
                                    (data . ((teams . [((team_id . "TTEAM")
                                                        (name (accepted (value . "旧值")) (editing (value . "候选值")))
                                                        (token (accepted (value (configured . t))) (editing . :null)))])))
                                    (monitor_states . [((item . "teams") (instance . "TTEAM")
                                                        (status . "failed") (error (message . "校验失败")))]))))
           requests)
      (loki-assistant-config--render module (loki-assistant-config--merge-editing module nil snapshot))
      (let* ((section (car loki-assistant-config--record-sections))
             (row (gethash "TTEAM" (loki-assistant-config--record-section-row-inputs section)))
             (name (cadr (assoc "name" row)))
             (password (cadr (assoc "token" row))))
        (should (string-match-p "旧值 → 候选值" (buffer-string)))
        (should (string-match-p "校验失败" (buffer-string)))
        (should (eq (widget-get password :secret) ?*))
        (widget-value-set name "修正值")
        (widget-value-set password "secret")
        (widget-setup)
        (cl-letf (((symbol-function 'loki-assistant-request)
                   (lambda (method path &rest args) (push (list method path args) requests))))
          (loki-assistant-config--row-save section "TTEAM"))
        (cl-destructuring-bind (method path args) (car requests)
          (should (equal method "PATCH"))
          (should (equal path "/editing/test/teams/TTEAM"))
          (should (equal (gethash "name" (plist-get args :body)) "修正值"))
          (should (equal (gethash "token" (plist-get args :body)) "secret")))))))

(ert-deftest loki-assistant-config-test-clears-rejected-password-fields ()
  (loki-assistant-config-test--with-buffer
    (let (request)
      (cl-letf (((symbol-function 'loki-assistant-request)
                 (lambda (method path &rest args) (setq request (list method path args)))))
        (loki-assistant-config--record-insert
         "slack" '((key . "teams") (name . "工作区") (data_type . "list")
                   (structure . ((id_field . "team_id") (show_all_fields . t)
                                 (fields . [((key . "team_id") (label . "Team ID") (data_type . "key"))
                                            ((key . "name") (label . "名称") (data_type . "string"))
                                            ((key . "token") (label . "令牌")
                                             (data_type . ((password . ((path . "loki-assistant/slack/{key}.gpg"))))))
                                            ((key . "d_cookie") (label . "Cookie d")
                                             (data_type . ((password . ((path . "loki-assistant/slack/{key}.gpg")
                                                                        (key . "d_cookie"))))))]))))
         '((teams . ((value . [((team_id . "TTEAM") (name . "工作区")
                                (token . "pass-ref") (d_cookie . "pass-ref"))])))))
        (let ((section (car loki-assistant-config--record-sections)))
          (loki-assistant-config--record-edit section "TTEAM")
          (dolist (key '("token" "d_cookie"))
            (widget-value-set (cdr (assoc key (loki-assistant-config--record-section-inputs section)))
                              "fixture"))
          (loki-assistant-config--record-save section)
          (loki-assistant-config-test--respond
           request '(:ok nil :status 422 :error-message "凭据无效"
                          :headers "HTTP/1.1 422\r\nX-Loki-Config-Cleared-Fields: token, d_cookie\r\n"))
          (let ((record (cdar (loki-assistant-config--record-section-records section)))
                (inputs (loki-assistant-config--record-section-inputs section)))
            (dolist (key '("token" "d_cookie"))
              (should (equal (widget-value (cdr (assoc key inputs))) ""))
              (should-not (assoc (intern key) record)))
            (should (eq (alist-get 'pending record) t))
            (should (equal (alist-get 'validation_error record) "凭据无效"))
            (should (equal (alist-get 'name record) "工作区"))))))))

(ert-deftest loki-assistant-config-test-deletes-keyed-record ()
  (loki-assistant-config-test--with-buffer
    (let (request confirmed)
      (cl-letf (((symbol-function 'yes-or-no-p) (lambda (&rest _) confirmed))
                ((symbol-function 'loki-assistant-request)
                 (lambda (method path &rest args) (setq request (list method path args)))))
        (loki-assistant-config--record-insert
         "slack" '((key . "teams") (name . "工作区") (data_type . "list")
                   (structure . ((id_field . "team_id"))))
         '((teams . ((value . [((team_id . "TKEEP"))])
                     (pending . [((key . "TTEAM") (value . ((team_id . "TTEAM"))))])))))
        (let* ((section (car loki-assistant-config--record-sections))
               (delete (cl-find "删除" (loki-assistant-config--record-section-widgets section)
                                :key #'widget-value :test #'equal)))
          (should delete)
          (widget-apply-action delete)
          (should-not request)
          (setq confirmed t)
          (loki-assistant-config--record-delete section "TTEAM")
          (should (equal (seq-take request 2) '("DELETE" "/editing/slack/teams/TTEAM")))
          (loki-assistant-config-test--respond request '(:ok nil :status 409 :error-message "正在保存"))
          (should (assoc "TTEAM" (loki-assistant-config--record-section-records section)))
          (should (string-match-p "正在保存" (buffer-string)))
          (loki-assistant-config--record-delete section "TTEAM")
          (loki-assistant-config-test--respond request '(:ok t :status 204))
          (should (equal (mapcar #'car (loki-assistant-config--record-section-records section))
                         '("TKEEP"))))))))

(ert-deftest loki-assistant-config-test-scalar-retry-preserves-draft ()
  (let ((item '((key . "scalar") (name . "标量") (data_type . "integer")))
        requests)
    (with-temp-buffer
      (loki-assistant-config-mode)
      (setq loki-assistant-config--generation (list nil))
      (cl-letf (((symbol-function 'loki-assistant-request)
                 (lambda (method path &rest args) (push (list method path args) requests))))
        (let ((inhibit-read-only t))
          (loki-assistant-config--render-entry
           "example" item '((scalar . ((value . 1) (source . "user") (pending . (((key . "2") (value . 2)))))))))
        (let ((entry (car loki-assistant-config--entries)))
          (should (loki-assistant-config--entry-retry-button entry))
          (widget-value-set (loki-assistant-config--entry-field entry) "3")
          (loki-assistant-config--entry-retry entry)
          (should (equal (seq-take (car requests) 2) '("PUT" "/editing/example/scalar")))
          (should-not (plist-member (nth 2 (car requests)) :body))
          (funcall (plist-get (nth 2 (car requests)) :callback) '(:ok t :data ((status . "confirmed"))))
          (funcall (plist-get (nth 2 (car requests)) :callback) '(:ok t :data ((value . 2) (source . "user"))))
          (should-not (loki-assistant-config--entry-retry-button entry))
          (should (equal (widget-value (loki-assistant-config--entry-field entry)) "3"))
          (should (equal (loki-assistant-config--entry-baseline entry) "2")))))))

(provide 'loki-assistant-config-tests)
;;; loki-assistant-config-tests.el ends here
