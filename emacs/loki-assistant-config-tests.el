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
     (unwind-protect (progn ,@body)
       (loki-assistant-config--stop-wait-timer))))

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

(ert-deftest loki-assistant-config-test-change-handler-failure-preserves-input ()
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
                              '(:ok nil :error-message "登录启动处理失败：launchctl 失败"
                                     :data ((code . "config-change-rejected")
                                               (message . "登录启动处理失败：launchctl 失败"))))))))
        (loki-assistant-config--submit entry t)
        (should (equal (loki-assistant-config--entry-source entry) "default"))
        (should (widget-value (loki-assistant-config--entry-field entry)))
        (should (loki-assistant-config--dirty-p entry))
        (should-not (loki-assistant-config--entry-busy entry))
        (should (string-match-p "launchctl 失败" (loki-assistant-config--entry-error entry)))))))

(defun loki-assistant-config-test--respond (request response)
  "将 RESPONSE 交给记录的 REQUEST 回调。"
  (funcall (plist-get (nth 2 request) :callback) response))

(ert-deftest loki-assistant-config-test-waits-through-validation-and-save-with-new-edits ()
  (loki-assistant-config-test--with-buffer
    (loki-assistant-config-test--fixture)
    (let ((entry (loki-assistant-config-test--entry "text"))
          (loki-assistant-config-save-timeout 120)
          (loki-assistant-request-timeout 10) requests)
      (cl-letf (((symbol-function 'loki-assistant-request)
                 (lambda (method path &rest args)
                   (setq requests (nconc requests (list (list method path args)))))))
        (widget-value-set (loki-assistant-config--entry-field entry) "提交值")
        (loki-assistant-config--submit entry t)
        (should (string-match-p "正在校验" (buffer-string)))
        (should (timerp loki-assistant-config--wait-timer))
        (dolist (button (append loki-assistant-config--buttons
                                (loki-assistant-config--entry-buttons entry)))
          (should (widget-get button :inactive)))
        (should-error (loki-assistant-config--submit entry t) :type 'user-error)
        (should-error (loki-assistant-config-refresh) :type 'user-error)
        (setf (loki-assistant-config--entry-started-at entry) (- (float-time) 4))
        (loki-assistant-config--show-wait)
        (should (string-match-p "已等待 4 秒" (buffer-string)))
        (widget-value-set (loki-assistant-config--entry-field entry) "新的编辑")
        (widget-apply (loki-assistant-config--entry-field entry) :notify)
        (loki-assistant-config-test--respond (pop requests) '(:ok t :data ((valid . t))))
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
        (loki-assistant-config--submit entry t)
        (loki-assistant-config-test--respond (pop requests) '(:ok t :data ((valid . t))))
        (widget-value-set (loki-assistant-config--entry-field entry) "保留新输入")
        (loki-assistant-config-test--respond
         (pop requests) '(:ok nil :error-code "config-change-rejected" :error-message "中转注册失败"))
        (should (string-match-p "中转注册失败" (buffer-string)))
        (should (equal (widget-value (loki-assistant-config--entry-field entry)) "保留新输入"))
        (should (equal (loki-assistant-config--entry-baseline entry) "原值"))
        (should (equal (loki-assistant-config--entry-source entry) "default"))
        (should-not loki-assistant-config--wait-timer)
        (loki-assistant-config--submit entry t)
        (should-not (loki-assistant-config--entry-error entry))
        (loki-assistant-config-test--respond
         (pop requests) '(:ok t :data ((valid . :false) (error . ((message . "预检失败"))))))
        (should (string-match-p "预检失败" (buffer-string)))
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
        (loki-assistant-config--submit entry t)
        (loki-assistant-config-test--respond (pop requests) '(:ok t :data ((valid . t))))
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
        (should (equal (nth 1 (car requests)) "/meta"))
        (let* ((json-object-type 'alist) (json-array-type 'vector) (json-key-type 'symbol)
               (meta (json-read-from-string loki-assistant-config-test--meta)))
          (loki-assistant-config-test--respond (pop requests) (list :ok t :data meta)))
        (let* ((json-object-type 'alist) (json-array-type 'vector) (json-key-type 'symbol)
               (json-false :false)
               (data (json-read-from-string loki-assistant-config-test--data)))
          (loki-assistant-config-test--respond (pop requests) (list :ok t :data data)))
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
          (loki-assistant-config--submit entry t)
          (loki-assistant-config-test--respond (pop requests) '(:ok t :data ((valid . t))))
          (loki-assistant-config-test--respond (pop requests) response)
          (should (string-match-p "保存结果未知" (buffer-string)))
          (should (string-match-p "刷新核对" (buffer-string)))
          (should (loki-assistant-config--entry-uncertain entry))
          (should-not loki-assistant-config--wait-timer)
          (should (equal (widget-value (loki-assistant-config--entry-field entry)) "未知结果的值"))
          (should-error (loki-assistant-config--submit entry t) :type 'user-error)
          (should-error (loki-assistant-config-save) :type 'user-error)
          (should-not requests)
          (dolist (button (loki-assistant-config--entry-buttons entry))
            (should (widget-get button :inactive)))
          (loki-assistant-config-refresh)
          (should (equal (nth 1 (car requests)) "/meta"))
          (let* ((json-object-type 'alist) (json-array-type 'vector) (json-key-type 'symbol)
                 (meta (json-read-from-string loki-assistant-config-test--meta)))
            (loki-assistant-config-test--respond (pop requests) (list :ok t :data meta)))
          (let* ((json-object-type 'alist) (json-array-type 'vector) (json-key-type 'symbol)
                 (json-false :false)
                 (data (json-read-from-string loki-assistant-config-test--data)))
            (loki-assistant-config-test--respond (pop requests) (list :ok t :data data)))
          (should-not (loki-assistant-config--entry-uncertain
                       (loki-assistant-config-test--entry "text")))
          (should-not loki-assistant-config--loading))))))

(ert-deftest loki-assistant-config-test-batch-progress-stops-at-specific-failure ()
  (loki-assistant-config-test--with-buffer
    (loki-assistant-config-test--fixture)
    (let ((text (loki-assistant-config-test--entry "text"))
          (flag (loki-assistant-config-test--entry "flag")) requests)
      (cl-letf (((symbol-function 'loki-assistant-request)
                 (lambda (method path &rest args)
                   (setq requests (nconc requests (list (list method path args)))))))
        (widget-value-set (loki-assistant-config--entry-field text) "新值")
        (widget-value-set (loki-assistant-config--entry-field flag) t)
        (loki-assistant-config-save)
        (should (string-match-p "正在保存 1/2 项：文本" (buffer-string)))
        (loki-assistant-config-test--respond (pop requests) '(:ok t :data ((valid . t))))
        (loki-assistant-config-test--respond
         (pop requests) '(:ok t :data ((module_id . "test") (key . "text") (value . "新值") (source . "user"))))
        (should (string-match-p "正在保存 2/2 项：开关" (buffer-string)))
        (loki-assistant-config-test--respond (pop requests) '(:ok t :data ((valid . t))))
        (loki-assistant-config-test--respond
         (pop requests) '(:ok nil :error-code "config-change-rejected" :error-message "系统拒绝操作"))
        (should (string-match-p "保存已停止（已完成 1/2 项）：开关：系统拒绝操作" (buffer-string)))
        (should-not (loki-assistant-config--dirty-p text))
        (should (loki-assistant-config--dirty-p flag))
        (should-not loki-assistant-config--saving-all)
        (should-not loki-assistant-config--wait-timer)
        (should-not requests)
        (dolist (button loki-assistant-config--buttons)
          (should-not (widget-get button :inactive)))))))

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
              (loki-assistant-config--submit (loki-assistant-config-test--entry "text") t)
              (setq timer loki-assistant-config--wait-timer)
              (should (memq timer timer-list))))
          (kill-buffer buffer)
          (should-not (memq timer timer-list))
          (loki-assistant-config-test--respond request '(:ok t :data ((valid . t)))))
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
              (lambda (headers _body)
                (if (string-prefix-p "POST /config/validate " headers)
                    '(200 "{\"valid\":true}")
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
                  nil))
              process chunk)))
          (loki-assistant-config-test--with-buffer
            (loki-assistant-config-test--fixture)
            (let ((entry (loki-assistant-config-test--entry "text")))
              (widget-value-set (loki-assistant-config--entry-field entry) "delayed")
              (loki-assistant-config--submit entry t)
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

(provide 'loki-assistant-config-tests)
;;; loki-assistant-config-tests.el ends here
