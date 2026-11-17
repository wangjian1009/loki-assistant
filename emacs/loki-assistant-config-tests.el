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
     (setq loki-assistant-config--generation (list nil))
     ,@body))

(defun loki-assistant-config-test--fixture ()
  "绘制配置快照，用于控件和保存测试。"
  (let* ((json-object-type 'alist) (json-array-type 'vector)
         (json-key-type 'symbol) (json-false :false) (json-null :null)
         (modules (alist-get 'modules (json-read-from-string loki-assistant-config-test--meta)))
         (data (json-read-from-string loki-assistant-config-test--data)))
    (loki-assistant-config--render (list (aref modules 1))
                                   (list (cons "test" (alist-get 'values data))))))

(defun loki-assistant-config-test--entry (key)
  "查找 KEY 对应控件。"
  (cl-find key loki-assistant-config--entries :test #'equal
           :key (lambda (entry) (alist-get 'key (loki-assistant-config--entry-item entry)))))

(ert-deftest loki-assistant-config-test-load-real-http ()
  (loki-assistant-test--with-server
      (lambda (headers _body)
        (if (string-prefix-p "GET /meta " headers)
            (list 200 loki-assistant-config-test--meta)
          (list 200 loki-assistant-config-test--data)))
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
  (let (requests (valid t) (save-ok t))
    (loki-assistant-test--with-server
        (lambda (headers body)
          (push (list headers (decode-coding-string body 'utf-8)) requests)
          (if (string-prefix-p "POST /config/validate " headers)
              (if valid '(200 "{\"valid\":true}")
                '(200 "{\"valid\":false,\"error\":{\"message\":\"值无效\"}}"))
            (if save-ok
                '(200 "{\"module_id\":\"test\",\"key\":\"text\",\"value\":\"新值\",\"source\":\"user\"}")
              '(400 "{\"message\":\"保存失败\",\"code\":\"invalid-config\"}"))))
      (loki-assistant-config-test--with-buffer
        (loki-assistant-config-test--fixture)
        (let ((entry (loki-assistant-config-test--entry "text")))
          (widget-value-set (loki-assistant-config--entry-field entry) "新值")
          (should (loki-assistant-config--dirty-p entry))
          (loki-assistant-config--submit entry nil)
          (loki-assistant-config-test--wait (lambda () (not (loki-assistant-config--entry-busy entry))))
          (should (= (length requests) 1))
          (should (loki-assistant-config--dirty-p entry))
          (setq valid nil)
          (loki-assistant-config--submit entry t)
          (loki-assistant-config-test--wait (lambda () (not (loki-assistant-config--entry-busy entry))))
          (should (equal (loki-assistant-config--entry-error entry) "值无效"))
          (should (= (length requests) 2))
          (setq valid t save-ok nil)
          (loki-assistant-config--submit entry t)
          (loki-assistant-config-test--wait (lambda () (not (loki-assistant-config--entry-busy entry))))
          (should (equal (loki-assistant-config--entry-error entry) "保存失败"))
          (should (equal (widget-value (loki-assistant-config--entry-field entry)) "新值"))
          (should (equal (loki-assistant-config--entry-source entry) "default"))
          (setq save-ok t)
          (loki-assistant-config--submit entry t)
          (loki-assistant-config-test--wait (lambda () (not (loki-assistant-config--entry-busy entry))))
          (should-not (loki-assistant-config--entry-error entry))
          (should-not (loki-assistant-config--dirty-p entry))
          (should (equal (loki-assistant-config--entry-source entry) "user"))
          (should (string-prefix-p "PUT /data/test/text " (caar requests)))
          (should (equal (cadar requests) "\"新值\"")))))))

(ert-deftest loki-assistant-config-test-late-save-keeps-new-input ()
  (loki-assistant-config-test--with-buffer
    (loki-assistant-config-test--fixture)
    (let ((entry (loki-assistant-config-test--entry "text")) pending)
      (cl-letf (((symbol-function 'loki-assistant-request)
                 (lambda (_method _path &rest args) (push (plist-get args :callback) pending))))
        (widget-value-set (loki-assistant-config--entry-field entry) "提交值")
        (loki-assistant-config--submit entry t)
        (funcall (pop pending) '(:ok t :data ((valid . t))))
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
        (funcall (cdar pending)
                 '(:ok t :data ((modules . [((id . "a") (name . "A") (items . [((key . "v") (name . "值") (data_type . "string"))]))
                                            ((id . "b") (name . "B") (items . [((key . "v") (name . "值") (data_type . "string"))]))]))))
        (should-not loki-assistant-config--entries)
        (funcall (cdr (assoc "/data/a" pending))
                 '(:ok t :data ((module_id . "a") (values . ((v . ((value . "a") (source . "user"))))))))
        (should-not loki-assistant-config--entries)
        (funcall (cdr (assoc "/data/b" pending)) '(:ok nil :error-message "无法读取 B"))
        (should-not loki-assistant-config--loading)
        (should-not loki-assistant-config--entries)
        (should (string-match-p "无法读取 B" (buffer-string)))))))

(ert-deftest loki-assistant-config-test-refresh-and-invalid-json ()
  (loki-assistant-config-test--with-buffer
    (loki-assistant-config-test--fixture)
    (let ((entry (loki-assistant-config-test--entry "object")) calls)
      (widget-value-set (loki-assistant-config--entry-field entry) "{invalid")
      (cl-letf (((symbol-function 'loki-assistant-request) (lambda (&rest _) (push t calls)))
                ((symbol-function 'yes-or-no-p) (lambda (&rest _) nil)))
        (loki-assistant-config-refresh)
        (should-not calls)
        (loki-assistant-config--submit entry t)
        (should-not calls)
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
                            '(:ok t :data ((valid . :false) (error . ((message . "未通过")))))))))
        (loki-assistant-config-save)
        (should (equal requests '("/config/validate")))
        (should (loki-assistant-config--dirty-p flag))
        (should (string-match-p "保存已停止" (buffer-string)))))))

(ert-deftest loki-assistant-config-test-autostart-isolated ()
  (let* ((directory (make-temp-file "loki-launch-" t))
         (executable (expand-file-name "含空格 & 托盘" directory))
         (plist (expand-file-name "agents/center.plist" directory))
         (logs (expand-file-name "logs" directory))
         (registered nil) calls)
    (unwind-protect
        (progn
          (with-temp-file executable (insert "测试文件，不执行"))
          (set-file-modes executable #o700)
          (cl-letf (((symbol-function 'loki-assistant-config--launchctl)
                     (lambda (&rest args)
                       (push args calls)
                       (cons (if (equal (car args) "print") (if registered 0 1) 0) ""))))
            (loki-assistant-config--apply-autostart t plist executable logs "gui/123")
            (should (member (list "bootstrap" "gui/123" plist) calls))
            (with-temp-buffer
              (insert-file-contents plist)
              (should (equal (caar (xml-parse-region (point-min) (point-max))) 'plist))
              (should (string-match-p "&amp;" (buffer-string))))
            (setq registered t calls nil)
            (loki-assistant-config--apply-autostart t plist executable logs "gui/123")
            (should (member '("kickstart" "gui/123/cc.loki-assistant.center") calls))
            (should-not (assoc "bootstrap" calls))
            (loki-assistant-config--apply-autostart nil plist executable logs "gui/123")
            (should-not (file-exists-p plist))
            (should (member '("bootout" "gui/123/cc.loki-assistant.center") calls))
            (setq registered nil calls nil)
            (loki-assistant-config--apply-autostart nil plist executable logs "gui/123")
            (should-not (assoc "bootout" calls))
            (should-error
             (loki-assistant-config--apply-autostart t plist "/missing" logs "gui/123"))))
      (delete-directory directory t))))

(ert-deftest loki-assistant-config-test-autostart-failure-is-distinct ()
  (loki-assistant-config-test--with-buffer
    (loki-assistant-config--render
     '(((id . "control-center") (name . "控制中心")
        (items . [((key . "center_auto_start") (name . "登录启动") (data_type . "boolean"))])))
     '(("control-center" . ((center_auto_start . ((value . :false) (source . "default")))))))
    (let ((entry (car loki-assistant-config--entries)))
      (widget-value-set (loki-assistant-config--entry-field entry) t)
      (cl-letf (((symbol-function 'loki-assistant-request)
                 (lambda (method _path &rest args)
                   (funcall (plist-get args :callback)
                            (if (eq method 'post) '(:ok t :data ((valid . t)))
                              '(:ok t :data ((module_id . "control-center") (key . "center_auto_start")
                                              (value . t) (source . "user")))))))
                ((symbol-function 'loki-assistant-config--autostart)
                 (lambda (_) (error "launchctl 失败"))))
        (loki-assistant-config--submit entry t)
        (should (equal (loki-assistant-config--entry-source entry) "user"))
        (should-not (loki-assistant-config--entry-busy entry))
        (should (string-prefix-p "配置已保存，登录启动同步失败" (loki-assistant-config--entry-error entry)))))))

(provide 'loki-assistant-config-tests)
;;; loki-assistant-config-tests.el ends here
