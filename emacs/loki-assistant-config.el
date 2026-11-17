;;; loki-assistant-config.el --- services 配置管理界面 -*- lexical-binding: t; -*-

;;; Commentary:

;; 使用 Customize 同源的 widget 展示服务元数据；业务校验与持久化交给 services。

;;; Code:

(require 'loki-assistant)
(require 'wid-edit)
(require 'xml)

(defconst loki-assistant-config--directory
  (file-name-directory (or load-file-name buffer-file-name))
  "配置界面源码所在目录。")

(defcustom loki-assistant-config-center-executable
  (expand-file-name "../apps/center/mac/.build/Debug/LokiAssistant.app/Contents/MacOS/LokiAssistant"
                    loki-assistant-config--directory)
  "登录启动使用的菜单栏可执行文件；自定义构建目录时需调整。"
  :type 'file
  :group 'loki-assistant)

(cl-defstruct (loki-assistant-config--entry
               (:constructor loki-assistant-config--entry-create))
  module item value source field baseline status busy error)

(defvar-local loki-assistant-config--entries nil)
(defvar-local loki-assistant-config--generation nil)
(defvar-local loki-assistant-config--loading nil)
(defvar-local loki-assistant-config--saving-all nil)
(defvar-local loki-assistant-config--message nil)

(defvar loki-assistant-config-mode-map
  (let ((map (make-sparse-keymap)))
    (set-keymap-parent map widget-keymap)
    (define-key map (kbd "g") #'loki-assistant-config-refresh)
    (define-key map (kbd "C-c C-c") #'loki-assistant-config-save)
    (define-key map (kbd "q") #'quit-window)
    map)
  "配置管理界面的按键。")

(define-derived-mode loki-assistant-config-mode special-mode "Loki 配置"
  "管理 services 配置，使用 TAB 移动、C-c C-c 保存修改、g 刷新。"
  (setq-local revert-buffer-function
              (lambda (&rest _) (loki-assistant-config-refresh)))
  ;; widget 自行保护非编辑区域。
  (setq buffer-read-only nil))

(defun loki-assistant-config--json (value)
  "将 VALUE 编码为 JSON，保留 false 和 null。"
  (let ((json-null :null) (json-false :false)) (json-encode value)))

(defun loki-assistant-config--display-value (item value)
  "将 ITEM 的 JSON VALUE 转为控件值。"
  (pcase (alist-get 'data_type item)
    ("boolean" (eq value t))
    ((or "string" "file_path" "executable") (if (stringp value) value ""))
    (_ (if (eq value :null) "" (loki-assistant-config--json value)))))

(defun loki-assistant-config--value (entry)
  "读取 ENTRY 的控件值，仅解析 JSON，不实现服务端业务校验。"
  (let ((value (widget-value (loki-assistant-config--entry-field entry))))
    (pcase (alist-get 'data_type (loki-assistant-config--entry-item entry))
      ("boolean" (if value t :false))
      ((or "string" "file_path" "executable") value)
      (_ (condition-case nil
             (json-parse-string value :object-type 'hash-table :array-type 'array
                                :null-object :null :false-object :false)
           (error (user-error "请输入有效的 JSON 值")))))))

(defun loki-assistant-config--dirty-p (entry)
  "ENTRY 是否有未保存的输入。"
  (not (equal (widget-value (loki-assistant-config--entry-field entry))
              (loki-assistant-config--entry-baseline entry))))

(defun loki-assistant-config--status (entry &optional error-text)
  "更新 ENTRY 的来源、修改和错误状态；ERROR-TEXT 为新的错误说明。"
  (setf (loki-assistant-config--entry-error entry) error-text)
  (widget-value-set
   (loki-assistant-config--entry-status entry)
   (cond (error-text (propertize error-text 'face 'error))
         ((loki-assistant-config--entry-busy entry) "正在处理…")
         ((loki-assistant-config--dirty-p entry)
          (propertize "已修改，尚未保存" 'face 'custom-modified))
         ((equal (loki-assistant-config--entry-source entry) "default")
          (propertize "默认值" 'face 'shadow))
         ((equal (loki-assistant-config--entry-source entry) "user") "用户设置，已保存")
         (t "未设置"))))

(defun loki-assistant-config--button (label action)
  "创建 LABEL 按钮，激活时调用 ACTION。"
  (widget-create 'push-button :notify (lambda (&rest _) (funcall action)) label)
  (widget-insert " "))

(defun loki-assistant-config--render-entry (module item values)
  "为 MODULE 的 ITEM 创建控件，VALUES 为生效配置。"
  (let* ((key (alist-get 'key item))
         (stored (alist-get (intern key) values))
         (value (if stored (alist-get 'value stored) :null))
         (type (alist-get 'data_type item))
         (entry (loki-assistant-config--entry-create
                 :module module :item item :value value :source (alist-get 'source stored)))
         (initial (loki-assistant-config--display-value item value)))
    (widget-insert (propertize (concat (alist-get 'name item) ": ") 'face 'bold))
    (setf (loki-assistant-config--entry-field entry)
          (widget-create
           (if (equal type "boolean") 'checkbox 'editable-field)
           :value initial :size (unless (equal type "boolean") 54)
           :format (if (equal type "boolean") "%[%v%]" "%v")
           :notify (lambda (&rest _) (loki-assistant-config--status entry))))
    (setf (loki-assistant-config--entry-baseline entry) initial)
    (when (member type '("file_path" "executable"))
      (widget-insert " ")
      (loki-assistant-config--button
       "选择文件…" (lambda () (loki-assistant-config--choose-file entry))))
    (widget-insert "\n    ")
    (loki-assistant-config--button
     "校验" (lambda () (loki-assistant-config--submit entry nil)))
    (loki-assistant-config--button
     "保存" (lambda () (loki-assistant-config--submit entry t)))
    (loki-assistant-config--button
     "撤销编辑"
     (lambda ()
       (when (loki-assistant-config--entry-busy entry)
         (user-error "请等待当前请求完成"))
       (widget-value-set (loki-assistant-config--entry-field entry)
                         (loki-assistant-config--entry-baseline entry))
       (loki-assistant-config--status entry)))
    (setf (loki-assistant-config--entry-status entry)
          (widget-create 'item :format "%v" "未设置"))
    (widget-insert "\n")
    (let ((description (alist-get 'description item)))
      (unless (or (null description) (string-empty-p description))
        (widget-insert (concat "    " description "\n"))))
    (widget-insert "\n")
    (push entry loki-assistant-config--entries)
    (loki-assistant-config--status entry)))

(defun loki-assistant-config--choose-file (entry)
  "为 ENTRY 选择文件或目录，程序有效性由服务端检查。"
  (let* ((item (loki-assistant-config--entry-item entry))
         (directory (eq (alist-get 'directory (alist-get 'file_path item)) t))
         (current (widget-value (loki-assistant-config--entry-field entry)))
         (suggested (alist-get 'search_paths (alist-get 'executable item)))
         (start (or (and (file-name-absolute-p current)
                         (file-directory-p (file-name-directory current))
                         (file-name-directory current))
                    (cl-find-if #'file-directory-p (append suggested nil))
                    default-directory))
         (selected (if directory (read-directory-name "选择目录：" start)
                     (read-file-name "选择文件（可进入 .app 包）：" start))))
    (widget-value-set (loki-assistant-config--entry-field entry)
                      (expand-file-name selected))
    (loki-assistant-config--status entry)))

(defun loki-assistant-config--clear ()
  "移除当前 buffer 的旧控件。"
  (let ((inhibit-read-only t))
    (mapc #'widget-delete widget-field-list)
    (mapc #'delete-overlay (overlays-in (point-min) (point-max)))
    (erase-buffer)
    (setq widget-field-list nil widget-field-new nil
          loki-assistant-config--entries nil)))

(defun loki-assistant-config--render (modules data &optional error-text)
  "显示 MODULES 和 DATA 的完整快照，或 ERROR-TEXT。"
  (loki-assistant-config--clear)
  (widget-insert (propertize "Loki Assistant 服务配置\n\n" 'face 'custom-group-tag))
  (widget-insert "TAB 移动，RET 激活按钮；C-c C-c 保存修改，g 刷新，q 关闭。\n")
  (widget-insert "配置保存在 services；校验不会保存，撤销编辑回到最近读取或保存的值。\n\n")
  (loki-assistant-config--button "刷新 / 重试" #'loki-assistant-config-refresh)
  (unless error-text
    (loki-assistant-config--button "保存所有修改" #'loki-assistant-config-save))
  (widget-insert "\n\n")
  (setq loki-assistant-config--message (widget-create 'item :format "%v\n\n" "就绪"))
  (cond
   (error-text (widget-value-set loki-assistant-config--message
                                (propertize error-text 'face 'error)))
   ((null modules) (widget-insert "没有可编辑的配置模块。\n"))
   (t
    (dolist (module modules)
      (let ((id (alist-get 'id module)))
        (widget-insert (propertize (concat (alist-get 'name module) "\n")
                                  'face 'custom-group-tag))
        (widget-insert (concat (or (alist-get 'description module) "") "\n\n"))
        (mapc (lambda (item)
                (loki-assistant-config--render-entry id item (cdr (assoc id data))))
              (alist-get 'items module))))))
  (setq loki-assistant-config--entries (nreverse loki-assistant-config--entries))
  (widget-setup)
  (set-buffer-modified-p nil)
  (goto-char (point-min)))

(defun loki-assistant-config--with-current (buffer generation action)
  "BUFFER 仍有效且 GENERATION 未过期时执行 ACTION。"
  (when (buffer-live-p buffer)
    (with-current-buffer buffer
      (when (eq generation loki-assistant-config--generation) (funcall action)))))

(defun loki-assistant-config--error (response)
  "从 RESPONSE 取错误说明。"
  (or (plist-get response :error-message) "services 返回了无效的配置响应"))

(defun loki-assistant-config--valid-value-p (value)
  "VALUE 是否包含有效的值来源包装。"
  (and (listp value) (assq 'value value)
       (member (alist-get 'source value) '("user" "default"))))

(defun loki-assistant-config--modules (response)
  "从 RESPONSE 读取并检查配置模块。"
  (unless (plist-get response :ok) (error "%s" (loki-assistant-config--error response)))
  (let ((raw (alist-get 'modules (plist-get response :data))))
    (unless (vectorp raw) (error "配置元数据缺少 modules 数组"))
    (dolist (module (append raw nil))
      (unless (and (stringp (alist-get 'id module))
                   (stringp (alist-get 'name module))
                   (vectorp (alist-get 'items module))
                   (cl-every
                    (lambda (item)
                      (and (stringp (alist-get 'key item))
                           (stringp (alist-get 'name item))
                           (member (alist-get 'data_type item)
                                   '("string" "integer" "number" "boolean"
                                     "dictionary" "list" "file_path" "executable"))))
                    (alist-get 'items module)))
        (error "配置模块元数据格式无效")))
    (sort (cl-remove-if (lambda (module) (zerop (length (alist-get 'items module))))
                        (append raw nil))
          (lambda (a b) (string< (alist-get 'id a) (alist-get 'id b))))))

(defun loki-assistant-config--load-data (modules buffer generation)
  "为 BUFFER 的 GENERATION 并行读取 MODULES 的实例。"
  (let ((pending (length modules)) data failed)
    (dolist (module modules)
      (let ((id (alist-get 'id module)))
        (loki-assistant-request
         'get (concat "/data/" (url-hexify-string id))
         :callback
         (lambda (response)
           (loki-assistant-config--with-current
            buffer generation
            (lambda ()
              (unless failed
                (condition-case err
                    (let* ((body (plist-get response :data))
                           (values (alist-get 'values body)))
                      (unless (and (plist-get response :ok)
                                   (equal (alist-get 'module_id body) id)
                                   (assq 'values body) (listp values)
                                   (cl-every (lambda (entry)
                                               (loki-assistant-config--valid-value-p (cdr entry)))
                                             values))
                        (error "%s" (loki-assistant-config--error response)))
                      (push (cons id values) data)
                      (when (zerop (cl-decf pending))
                        (setq loki-assistant-config--loading nil)
                        (loki-assistant-config--render modules data)))
                  (error
                   (setq failed t loki-assistant-config--loading nil)
                   (loki-assistant-config--render nil nil (error-message-string err)))))))))))))

(defun loki-assistant-config--load ()
  "异步加载元数据和全部模块实例，只展示完整快照。"
  (let ((buffer (current-buffer))
        (generation (setq loki-assistant-config--generation (list nil))))
    (setq loki-assistant-config--loading t)
    (loki-assistant-config--clear)
    (widget-insert "正在读取 services 配置…\n")
    (loki-assistant-request
     'get "/meta"
     :callback
     (lambda (response)
       (loki-assistant-config--with-current
        buffer generation
        (lambda ()
          (condition-case err
              (let ((modules (loki-assistant-config--modules response)))
                (if modules
                    (loki-assistant-config--load-data modules buffer generation)
                  (setq loki-assistant-config--loading nil)
                  (loki-assistant-config--render nil nil)))
            (error
             (setq loki-assistant-config--loading nil)
             (loki-assistant-config--render nil nil (error-message-string err))))))))))

(defun loki-assistant-config-refresh ()
  "重新获取配置；有未保存输入时先确认。"
  (interactive)
  (when (or loki-assistant-config--loading
            (cl-some #'loki-assistant-config--entry-busy loki-assistant-config--entries))
    (user-error "请等待当前请求完成"))
  (when (or (not (cl-some #'loki-assistant-config--dirty-p loki-assistant-config--entries))
            (yes-or-no-p "刷新会丢弃未保存的编辑，继续？"))
    (loki-assistant-config--load)))

(defun loki-assistant-config--submit (entry save &optional callback)
  "校验 ENTRY，SAVE 非 nil 时保存；完成后 CALLBACK 接收成功标志。"
  (when (and loki-assistant-config--saving-all (not callback))
    (user-error "请等待批量保存完成"))
  (when (loki-assistant-config--entry-busy entry) (user-error "配置项正在处理"))
  (let ((buffer (current-buffer))
        (generation loki-assistant-config--generation)
        (module (loki-assistant-config--entry-module entry))
        (key (alist-get 'key (loki-assistant-config--entry-item entry)))
        (snapshot (widget-value (loki-assistant-config--entry-field entry))))
    (cl-labels
        ((finish (success &optional text)
           (setf (loki-assistant-config--entry-busy entry) nil)
           (if (equal snapshot (widget-value (loki-assistant-config--entry-field entry)))
               (loki-assistant-config--status entry text)
             (loki-assistant-config--status entry)
             (when text (message "提交值处理失败：%s" text)))
           (when callback (funcall callback success)))
         (in-buffer (action)
           (loki-assistant-config--with-current buffer generation action)))
      (condition-case err
          (let ((value (loki-assistant-config--value entry)))
            (setf (loki-assistant-config--entry-busy entry) t)
            (loki-assistant-config--status entry)
            (loki-assistant-request
             'post "/config/validate"
             :body (loki-assistant-config--json
                    `((module_id . ,module) (key . ,key) (value . ,value)))
             :callback
             (lambda (response)
               (in-buffer
                (lambda ()
                  (let ((body (plist-get response :data)))
                    (cond
                     ((not (plist-get response :ok))
                      (finish nil (loki-assistant-config--error response)))
                     ((not (eq (alist-get 'valid body) t))
                      (finish nil (or (alist-get 'message (alist-get 'error body)) "配置值无效")))
                     ((not save)
                      (finish t)
                      (message "提交值校验通过，尚未保存"))
                     (t
                      ;; 提交原始 JSON，PUT 再次执行服务器校验。
                      (loki-assistant-request
                       'put (concat "/data/" (url-hexify-string module) "/" (url-hexify-string key))
                       :body (loki-assistant-config--json value)
                       :callback
                       (lambda (result)
                         ;; 即使界面已关闭，已保存的登录启动选项仍需同步。
                         (let* ((stored (plist-get result :data))
                                (ok (and (plist-get result :ok)
                                         (equal (alist-get 'module_id stored) module)
                                         (equal (alist-get 'key stored) key)
                                         (loki-assistant-config--valid-value-p stored)))
                                (failure (unless ok (loki-assistant-config--error result))))
                           (when (and ok (equal module "control-center") (equal key "center_auto_start"))
                             (condition-case launch-error
                                 (loki-assistant-config--autostart (eq (alist-get 'value stored) t))
                               (error (setq failure (concat "配置已保存，登录启动同步失败："
                                                            (error-message-string launch-error))))))
                           (in-buffer
                            (lambda ()
                              (when ok
                                (setf (loki-assistant-config--entry-value entry) (alist-get 'value stored)
                                      (loki-assistant-config--entry-source entry) (alist-get 'source stored)
                                      (loki-assistant-config--entry-baseline entry)
                                      (loki-assistant-config--display-value
                                       (loki-assistant-config--entry-item entry) (alist-get 'value stored)))
                                ;; 请求期间的新输入保留，避免异步响应覆盖用户编辑。
                                (when (equal snapshot (widget-value (loki-assistant-config--entry-field entry)))
                                  (widget-value-set (loki-assistant-config--entry-field entry)
                                                    (loki-assistant-config--entry-baseline entry))))
                              (finish (and ok (not failure)) failure))))))))))))))
        (error (finish nil (error-message-string err)))))))

(defun loki-assistant-config-save ()
  "逐项校验并保存已修改项；遇到错误停止，保留其他输入。"
  (interactive)
  (when (or loki-assistant-config--loading
            (cl-some #'loki-assistant-config--entry-busy loki-assistant-config--entries))
    (user-error "请等待当前请求完成"))
  (let ((pending (cl-remove-if-not #'loki-assistant-config--dirty-p loki-assistant-config--entries)))
    (setq loki-assistant-config--saving-all t)
    (cl-labels
        ((next (success)
           (cond
            ((not success)
             (setq loki-assistant-config--saving-all nil)
             (widget-value-set loki-assistant-config--message
                               "保存已停止，请修正错误后重试；此前成功项已保存。"))
            (pending
             (let ((entry (pop pending)))
               (if (loki-assistant-config--dirty-p entry)
                   (loki-assistant-config--submit entry t #'next)
                 (next t))))
            (t
             (setq loki-assistant-config--saving-all nil)
             (widget-value-set
              loki-assistant-config--message
              (if (cl-some #'loki-assistant-config--dirty-p loki-assistant-config--entries)
                  "提交的修改已保存；仍有新的编辑尚未保存。"
                "所有修改已保存。"))))))
      (next t))))

(defun loki-assistant-config--launchctl (&rest arguments)
  "运行 launchctl ARGUMENTS，返回状态码与输出，不通过 shell。"
  (with-temp-buffer
    (let ((status (apply #'call-process "/bin/launchctl" nil t nil arguments)))
      (cons status (string-trim (buffer-string))))))

(defun loki-assistant-config--autostart (enabled)
  "同步 ENABLED 到菜单栏用户级 LaunchAgent。"
  (unless (eq system-type 'darwin) (error "登录启动仅支持 macOS"))
  (loki-assistant-config--apply-autostart
   enabled
   (expand-file-name "~/Library/LaunchAgents/cc.loki-assistant.center.plist")
   loki-assistant-config-center-executable
   (expand-file-name "~/Library/Logs/LokiAssistant")
   (format "gui/%d" (user-uid))))

(defun loki-assistant-config--apply-autostart (enabled plist executable logs domain)
  "将 ENABLED 应用到 PLIST、EXECUTABLE、LOGS 和 DOMAIN，便于隔离测试。"
  (let ((label "cc.loki-assistant.center")
        (target (concat domain "/cc.loki-assistant.center")))
    (cl-labels ((checked (&rest arguments)
                  (let ((result (apply #'loki-assistant-config--launchctl arguments)))
                    (unless (eq (car result) 0)
                      (error "launchctl %s：%s" (car arguments) (cdr result))))))
      (if enabled
          (progn
            (unless (and (file-regular-p executable) (file-executable-p executable))
              (error "托盘程序不可执行，请先构建并检查 loki-assistant-config-center-executable"))
            (make-directory (file-name-directory plist) t)
            (make-directory logs t)
            (with-temp-file plist
              (insert "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n")
              (xml-print
               `((plist ((version . "1.0"))
                        (dict nil (key nil "Label") (string nil ,label)
                              (key nil "ProgramArguments")
                              (array nil (string nil ,(expand-file-name executable)))
                              (key nil "RunAtLoad") (true nil)
                              (key nil "KeepAlive") (false nil)
                              (key nil "LimitLoadToSessionType") (string nil "Aqua")
                              (key nil "StandardOutPath") (string nil ,(concat logs "/center.log"))
                              (key nil "StandardErrorPath") (string nil ,(concat logs "/center.error.log")))))))
            (checked "enable" target)
            (if (eq (car (loki-assistant-config--launchctl "print" target)) 0)
                (checked "kickstart" target)
              (checked "bootstrap" domain plist)))
        (when (eq (car (loki-assistant-config--launchctl "print" target)) 0)
          (checked "bootout" target))
        (when (file-exists-p plist) (delete-file plist))))))

;;;###autoload
(defun loki-assistant-config ()
  "打开 Customize 风格的 services 配置管理界面。"
  (interactive)
  (let ((buffer (get-buffer-create "*Loki Assistant 配置*")))
    (pop-to-buffer buffer)
    (unless (derived-mode-p 'loki-assistant-config-mode)
      (loki-assistant-config-mode)
      (loki-assistant-config--load))
    buffer))

(provide 'loki-assistant-config)
;;; loki-assistant-config.el ends here
