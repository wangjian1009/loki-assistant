;;; loki-assistant-config.el --- services 配置管理界面 -*- lexical-binding: t; -*-

;;; Commentary:

;; 使用 Customize 同源的 widget 展示服务元数据；业务校验与持久化交给 services。

;;; Code:

(require 'loki-assistant)
(require 'cus-edit)
(require 'wid-edit)
(require 'seq)

(defcustom loki-assistant-config-save-timeout 120
  "配置保存等待的最长秒数，包含服务端配置处理器执行时间。
超时后保存结果未知，需要刷新核对，不自动重试。"
  :type 'number
  :group 'loki-assistant)

(cl-defstruct (loki-assistant-config--entry
               (:constructor loki-assistant-config--entry-create))
  module item value source field baseline status busy error result started-at buttons uncertain
  pending retry-point retry-button)

(defvar loki-assistant-config--selection-generation nil)
(defvar-local loki-assistant-config--module-id nil)
(defvar-local loki-assistant-config--entries nil)
(defvar-local loki-assistant-config--generation nil)
(defvar-local loki-assistant-config--loading nil)
(defvar-local loki-assistant-config--saving-all nil)
(defvar-local loki-assistant-config--message nil)
(defvar-local loki-assistant-config--buttons nil)
(defvar-local loki-assistant-config--wait-timer nil)
(defvar-local loki-assistant-config--batch-total 0)
(defvar-local loki-assistant-config--batch-done 0)

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
  (setq buffer-read-only nil)
  (add-hook 'kill-buffer-hook #'loki-assistant-config--stop-wait-timer nil t)
  (add-hook 'kill-buffer-hook #'loki-assistant-config--record-stop nil t))

(defun loki-assistant-config--json (value)
  "将 VALUE 编码为 JSON，保留 false 和 null。"
  (let ((json-null :null) (json-false :false)) (json-encode value)))

(defun loki-assistant-config--data-type (metadata)
  "读取 METADATA 的类型名称。"
  (let ((type (alist-get 'data_type metadata)))
    (if (and (consp type) (assq 'password type)) "password" type)))

(defun loki-assistant-config--display-value (item value)
  "将 ITEM 的 JSON VALUE 转为控件值。"
  (pcase (loki-assistant-config--data-type item)
    ("boolean" (eq value t))
    ("password" "")
    ((or "string" "file_path" "executable") (if (stringp value) value ""))
    (_ (if (eq value :null) "" (loki-assistant-config--json value)))))

(defface loki-assistant-config-invalid-field
  '((((background dark)) :inherit widget-field :background "#56242c")
    (t :inherit widget-field :background "#ffd6db"))
  "输入格式有误时的字段背景。"
  :group 'loki-assistant)

(defun loki-assistant-config--json-input-p (item value)
  "判断 ITEM 中的 VALUE 是否按 JSON 编辑。"
  (let ((type (loki-assistant-config--data-type item)))
    (if (equal type "password")
        (or (string-prefix-p "{" value) (string-prefix-p "\"" value))
      (not (member type '(nil "boolean" "string" "key" "file_path" "executable" "password_reference"))))))

(defun loki-assistant-config--parse-input (item value)
  "读取 ITEM 的 VALUE，无法解析的 JSON 按原字符串提交。"
  (if (loki-assistant-config--json-input-p item value)
      (condition-case nil
          (json-parse-string value :object-type 'hash-table :array-type 'array
                             :null-object :null :false-object :false)
        (json-parse-error value))
    value))

(defun loki-assistant-config--input-face (item value)
  "按 ITEM 的本地格式规则为字符串 VALUE 选择输入框背景。"
  (let* ((checker (alist-get 'checker item))
         (pattern (and (consp checker) (alist-get 'emacs_pattern checker)))
         (case-fold-search nil))
    (if (and (stringp value)
             (if (string-empty-p value)
                 (eq (alist-get 'required item) t)
               (or (and (stringp pattern)
                        (not (condition-case nil
                                 (string-match-p pattern value)
                               (invalid-regexp t))))
                   (and (loki-assistant-config--json-input-p item value)
                        (condition-case nil
                            (progn (json-parse-string value) nil)
                          (json-parse-error t))))))
        'loki-assistant-config-invalid-field
      'widget-field)))

(defun loki-assistant-config--mark-field (widget item)
  "更新 WIDGET 的格式提示，不修改其值。"
  (let ((face (loki-assistant-config--input-face item (widget-value widget)))
        (overlay (widget-get widget :field-overlay)))
    (widget-put widget :value-face face)
    (when (overlayp overlay)
      (overlay-put overlay 'face face))))

(defun loki-assistant-config--value (entry)
  "读取 ENTRY 的控件值，业务校验由 services 执行。"
  (let ((value (widget-value (loki-assistant-config--entry-field entry)))
        (item (loki-assistant-config--entry-item entry)))
    (if (equal (loki-assistant-config--data-type item) "boolean")
        (if value t :false)
      (loki-assistant-config--parse-input item value))))

(defun loki-assistant-config--dirty-p (entry)
  "ENTRY 是否有未保存的输入。"
  (not (equal (widget-value (loki-assistant-config--entry-field entry))
              (loki-assistant-config--entry-baseline entry))))

(defun loki-assistant-config--set-widget-text (widget text &optional face)
  "设置 WIDGET 的 TEXT，并为其内容应用 FACE。
`item' widget 通过 `princ' 重绘值，会丢失字符串上的文本属性，因此
这里在 widget 重绘完成后直接给内容区域加 face。"
  (when widget
    (widget-value-set widget text)
    (let ((from (widget-get widget :from))
          (to (widget-get widget :to)))
      (when (and (markerp from) (markerp to))
        (with-silent-modifications
          (remove-text-properties from to '(face nil))
          (when face
            (add-text-properties from to (list 'face face))))))))

(defun loki-assistant-config--status (entry)
  "重绘 ENTRY 的执行过程、结果和配置来源。"
  (let (text face)
    (cond
     ((loki-assistant-config--entry-busy entry)
      (setq text
            (format "%s（已等待 %d 秒）"
                    "正在执行并保存，请稍候…"
                    (max 0 (floor (- (float-time)
                                     (loki-assistant-config--entry-started-at entry)))))
            face 'warning))
     ((loki-assistant-config--entry-error entry)
      (setq text (loki-assistant-config--entry-error entry) face 'error))
     ((loki-assistant-config--entry-result entry)
      (setq text (loki-assistant-config--entry-result entry) face 'success))
     ((loki-assistant-config--dirty-p entry)
      (setq text "已修改，尚未保存" face 'custom-modified))
     ((equal (loki-assistant-config--entry-source entry) "default")
      (setq text "默认值" face 'shadow))
     ((equal (loki-assistant-config--entry-source entry) "user")
      (setq text "用户设置，已保存"))
     (t (setq text "未设置")))
    (when (and (loki-assistant-config--entry-pending entry)
               (not (loki-assistant-config--entry-busy entry)))
      (setq text (concat "待确认 · " text)))
    (loki-assistant-config--set-widget-text
     (loki-assistant-config--entry-status entry) text face)))

(defun loki-assistant-config--edited (entry)
  "ENTRY 发生编辑时清除上次结果，保留执行中和结果未知状态。"
  (unless (or (loki-assistant-config--entry-busy entry)
              (loki-assistant-config--entry-uncertain entry))
    (setf (loki-assistant-config--entry-error entry) nil
          (loki-assistant-config--entry-result entry) nil))
  (loki-assistant-config--mark-field (loki-assistant-config--entry-field entry)
                                      (loki-assistant-config--entry-item entry))
  (loki-assistant-config--status entry))

(defun loki-assistant-config--button (label action)
  "创建并返回 LABEL 按钮，激活时调用 ACTION。"
  (let ((button (widget-create 'push-button :notify (lambda (&rest _) (funcall action)) label)))
    (widget-insert " ")
    button))

(defun loki-assistant-config--busy-p ()
  "当前是否有配置项在执行。"
  (cl-some #'loki-assistant-config--entry-busy loki-assistant-config--entries))

(defun loki-assistant-config--sync-controls ()
  "根据执行和批量状态启用或禁用操作按钮，输入控件继续可编辑。"
  (let ((busy (or loki-assistant-config--loading loki-assistant-config--saving-all
                  (loki-assistant-config--busy-p))))
    (dolist (button loki-assistant-config--buttons)
      (let ((disabled (or busy
                          (and (widget-get button :loki-save-all)
                               (cl-some #'loki-assistant-config--entry-uncertain
                                        loki-assistant-config--entries)))))
        (unless (eq (not (null disabled)) (not (null (widget-get button :inactive))))
          (widget-apply button (if disabled :deactivate :activate)))))
    (dolist (entry loki-assistant-config--entries)
      (let ((disabled (or loki-assistant-config--saving-all
                          (loki-assistant-config--entry-busy entry)
                          (loki-assistant-config--entry-uncertain entry))))
        (dolist (button (loki-assistant-config--entry-buttons entry))
          (unless (eq (not (null disabled)) (not (null (widget-get button :inactive))))
            (widget-apply button (if disabled :deactivate :activate))))))))

(defun loki-assistant-config--stop-wait-timer ()
  "停止当前配置 buffer 的等待显示计时器。"
  (when loki-assistant-config--wait-timer
    (cancel-timer loki-assistant-config--wait-timer)
    (setq loki-assistant-config--wait-timer nil)))

(defun loki-assistant-config--show-wait ()
  "重绘等待时长和批量保存进度，不改变输入或请求状态。"
  (let ((busy (cl-remove-if-not #'loki-assistant-config--entry-busy
                                loki-assistant-config--entries)))
    (dolist (entry busy) (loki-assistant-config--status entry))
    (when (and busy loki-assistant-config--message)
      (widget-value-set
       loki-assistant-config--message
       (format "%s%s；请等待执行结果，可继续编辑。"
               (if loki-assistant-config--saving-all
                   (format "正在保存 %d/%d 项：" (1+ loki-assistant-config--batch-done)
                           loki-assistant-config--batch-total)
                 "正在处理：")
               (mapconcat (lambda (entry) (alist-get 'name (loki-assistant-config--entry-item entry)))
                          busy "、"))))))

(defun loki-assistant-config--begin (entry phase)
  "将 ENTRY 置为 PHASE 等待阶段，启动时长显示。"
  (setf (loki-assistant-config--entry-busy entry) phase
        (loki-assistant-config--entry-started-at entry) (float-time)
        (loki-assistant-config--entry-error entry) nil
        (loki-assistant-config--entry-result entry) nil)
  (unless loki-assistant-config--wait-timer
    (let ((buffer (current-buffer)) (generation loki-assistant-config--generation) timer)
      (setq timer
            (run-at-time
             1 1
             (lambda ()
               (if (and (buffer-live-p buffer)
                        (with-current-buffer buffer
                          (and (eq generation loki-assistant-config--generation)
                               (loki-assistant-config--busy-p))))
                   (with-current-buffer buffer (loki-assistant-config--show-wait))
                 (cancel-timer timer)))))
      (setq loki-assistant-config--wait-timer timer)))
  (loki-assistant-config--show-wait)
  (loki-assistant-config--sync-controls))

(defun loki-assistant-config--render-entry (module item values)
  "为 MODULE 的 ITEM 创建控件，VALUES 为生效配置。"
  (let* ((key (alist-get 'key item))
         (stored (alist-get (intern key) values))
         (value (if stored (alist-get 'value stored) :null))
         (type (loki-assistant-config--data-type item))
         (entry (loki-assistant-config--entry-create
                 :module module :item item :value value :source (alist-get 'source stored)
                 :pending (not (seq-empty-p (alist-get 'pending stored)))))
         (initial (loki-assistant-config--display-value item value)))
    (widget-insert (propertize (concat (alist-get 'name item) ": ") 'face 'bold))
    (setf (loki-assistant-config--entry-field entry)
          (widget-create
           (if (equal type "boolean") 'checkbox 'editable-field)
           :value initial :size (unless (equal type "boolean") 54)
           :secret (when (equal type "password") ?*)
           :value-face (loki-assistant-config--input-face item initial)
           :format (if (equal type "boolean") "%[%v%]" "%v")
           :notify (lambda (&rest _) (loki-assistant-config--edited entry))))
    (setf (loki-assistant-config--entry-baseline entry) initial)
    (when (member type '("file_path" "executable"))
      (widget-insert " ")
      (push (loki-assistant-config--button
             "选择文件…" (lambda () (loki-assistant-config--choose-file entry)))
            (loki-assistant-config--entry-buttons entry)))
    (widget-insert "\n    ")
    (dolist (button
             (list
              (loki-assistant-config--button
               "保存" (lambda () (loki-assistant-config--submit entry)))
              (loki-assistant-config--button
               "撤销编辑"
               (lambda ()
                 (when (or loki-assistant-config--saving-all
                           (loki-assistant-config--entry-busy entry)
                           (loki-assistant-config--entry-uncertain entry))
                   (user-error "请先等待执行完成或刷新核对保存结果"))
                 (widget-value-set (loki-assistant-config--entry-field entry)
                                   (loki-assistant-config--entry-baseline entry))
                 (loki-assistant-config--edited entry)))))
      (push button (loki-assistant-config--entry-buttons entry)))
    (setf (loki-assistant-config--entry-status entry)
          (widget-create 'item :format "%v" "未设置"))
    (setf (loki-assistant-config--entry-retry-point entry) (copy-marker (point)))
    (widget-insert "\n")
    (let ((description (alist-get 'description item)))
      (unless (or (null description) (string-empty-p description))
        (widget-insert (concat "    " description "\n"))))
    (widget-insert "\n")
    (push entry loki-assistant-config--entries)
    (loki-assistant-config--entry-sync-retry entry)
    (loki-assistant-config--status entry)))

(defun loki-assistant-config--render-read-only-entry (item values)
  "显示只读的 ITEM 及其生效值 VALUES。"
  (let* ((stored (alist-get (intern (alist-get 'key item)) values))
         (value (alist-get 'value stored)))
    (widget-insert (propertize (concat (alist-get 'name item) "：") 'face 'bold)
                   (cond ((member value '(nil :null "")) "未设置")
                         ((equal (loki-assistant-config--data-type item) "password")
                          (if (eq value :false) "未设置" "已保存"))
                         (t (format "%s" value)))
                   "  只读\n")
    (when-let ((description (alist-get 'description item)))
      (unless (string-empty-p description)
        (widget-insert "    " description "\n")))
    (widget-insert "\n")))

(defun loki-assistant-config--choose-file (entry)
  "为 ENTRY 选择文件或目录，程序有效性由服务端检查。"
  (when (or loki-assistant-config--saving-all
            (loki-assistant-config--entry-busy entry)
            (loki-assistant-config--entry-uncertain entry))
    (user-error "请先等待执行完成或刷新核对保存结果"))
  (let* ((item (loki-assistant-config--entry-item entry))
         (file-path (alist-get 'file_path item))
         (executable (alist-get 'executable item))
         (directory (and (listp file-path) (eq (alist-get 'directory file-path) t)))
         (current (widget-value (loki-assistant-config--entry-field entry)))
         (suggested (and (listp executable) (alist-get 'search_paths executable)))
         (start (or (and (file-name-absolute-p current)
                         (file-directory-p (file-name-directory current))
                         (file-name-directory current))
                    (and (sequencep suggested) (cl-find-if #'file-directory-p suggested))
                    default-directory))
         (selected (if directory (read-directory-name "选择目录：" start)
                     (read-file-name "选择文件（可进入 .app 包）：" start))))
    (widget-value-set (loki-assistant-config--entry-field entry)
                      (expand-file-name selected))
    (loki-assistant-config--edited entry)))

(defun loki-assistant-config--clear ()
  "移除当前 buffer 的旧控件。"
  (loki-assistant-config--stop-wait-timer)
  (loki-assistant-config--record-detach)
  (let ((inhibit-read-only t))
    (mapc #'widget-delete widget-field-list)
    (mapc #'delete-overlay (overlays-in (point-min) (point-max)))
    (erase-buffer)
    (setq widget-field-list nil widget-field-new nil
          loki-assistant-config--entries nil loki-assistant-config--buttons nil)))

(defun loki-assistant-config--render (module values &optional error-text)
  "显示 MODULE 的配置 VALUES，或 ERROR-TEXT。"
  (loki-assistant-config--clear)
  (widget-insert
   (propertize (format "Loki Assistant 配置 · %s\n\n"
                       (or (alist-get 'name module) loki-assistant-config--module-id))
               'face 'custom-group-tag))
  (widget-insert "TAB 移动，RET 激活按钮，C-c C-c 保存，g 刷新，q 关闭。\n\n")
  (push (loki-assistant-config--button "刷新 / 重试" #'loki-assistant-config-refresh)
        loki-assistant-config--buttons)
  (unless error-text
    (let ((button (loki-assistant-config--button "保存本模块修改" #'loki-assistant-config-save)))
      (widget-put button :loki-save-all t)
      (push button loki-assistant-config--buttons)))
  (widget-insert "\n\n")
  (setq loki-assistant-config--message (widget-create 'item :format "%v\n\n" "就绪"))
  (if error-text
      (loki-assistant-config--set-widget-text loki-assistant-config--message error-text 'error)
    (let ((id (alist-get 'id module))
          (description (alist-get 'description module)))
      (when (and (stringp description) (not (string-empty-p description)))
        (widget-insert description "\n\n"))
      (dolist (item (append (alist-get 'items module) nil))
        (cond
         ((consp (alist-get 'structure item))
          (loki-assistant-config--record-insert id item values))
         ((eq (alist-get 'read_only item) t)
          (loki-assistant-config--render-read-only-entry item values))
         (t (loki-assistant-config--render-entry id item values))))))
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
  "从 RESPONSE 读取具有配置项的模块。"
  (unless (plist-get response :ok) (error "%s" (loki-assistant-config--error response)))
  (let ((modules (alist-get 'modules (plist-get response :data))))
    (unless (vectorp modules) (error "配置元数据缺少 modules 数组"))
    (seq-filter
     (lambda (module)
       (and (stringp (alist-get 'id module))
            (stringp (alist-get 'name module))
            (vectorp (alist-get 'items module))
            (not (seq-empty-p (alist-get 'items module)))))
     (append modules nil))))

(defun loki-assistant-config--read-values (response id)
  "从 RESPONSE 读取模块 ID 的生效值和待确认变更。"
  (unless (plist-get response :ok) (error "%s" (loki-assistant-config--error response)))
  (let* ((body (plist-get response :data))
         (values (copy-tree (alist-get 'values body))))
    (unless (and (equal (alist-get 'module_id body) id)
                 (assq 'values body) (listp values)
                 (cl-every (lambda (entry)
                             (loki-assistant-config--valid-value-p (cdr entry)))
                           values))
      (error "services 返回了无效的模块配置"))
    (dolist (entry (alist-get 'pending body))
      (unless (assq (car entry) values)
        (push (list (car entry) (cons 'value :null)) values))
      (push (cons 'pending (cdr entry)) (cdr (assq (car entry) values))))
    values))

(defun loki-assistant-config--finish-load (module values &optional error-text)
  "结束当前读取，展示 MODULE 的 VALUES 或 ERROR-TEXT。"
  (setq loki-assistant-config--loading nil)
  (loki-assistant-config--render module values error-text))

(defun loki-assistant-config--load-data (module buffer generation)
  "为 BUFFER 的 GENERATION 读取 MODULE 的配置。"
  (let ((id (alist-get 'id module)))
    (loki-assistant-request
     'get (concat "/data/" (url-hexify-string id))
     :callback
     (lambda (response)
       (loki-assistant-config--with-current
        buffer generation
        (lambda ()
          (condition-case err
              (loki-assistant-config--finish-load
               module (loki-assistant-config--read-values response id))
            (error
             (loki-assistant-config--finish-load module nil (error-message-string err))))))))))

(defun loki-assistant-config--load (&optional module)
  "异步读取当前模块。可复用选择器已获取的 MODULE 元数据。"
  (let* ((id loki-assistant-config--module-id)
         (buffer (current-buffer))
         (generation (setq loki-assistant-config--generation (list nil))))
    (setq loki-assistant-config--loading t)
    (loki-assistant-config--clear)
    (widget-insert (format "正在读取模块 %s 的配置…\n" id))
    (cl-labels ((load-module (metadata)
                  (unless (and (equal (alist-get 'id metadata) id)
                               (vectorp (alist-get 'items metadata))
                               (not (seq-empty-p (alist-get 'items metadata)))
                               (cl-every
                                (lambda (item)
                                  (and (stringp (alist-get 'key item))
                                       (stringp (alist-get 'name item))
                                       (member (loki-assistant-config--data-type item)
                                               '("string" "password" "integer" "number" "boolean"
                                                 "dictionary" "structure" "list" "password_reference"
                                                 "file_path" "executable"))))
                                (alist-get 'items metadata)))
                    (error "模块 %s 的配置元数据格式无效" id))
                  (loki-assistant-config--load-data metadata buffer generation)))
      (if module
          (condition-case err
              (load-module module)
            (error (loki-assistant-config--finish-load nil nil (error-message-string err))))
        (loki-assistant-request
         'get (concat "/meta/" (url-hexify-string id))
         :callback
         (lambda (response)
           (loki-assistant-config--with-current
            buffer generation
            (lambda ()
              (condition-case err
                  (progn
                    (unless (plist-get response :ok)
                      (error "%s" (loki-assistant-config--error response)))
                    (load-module (plist-get response :data)))
                (error
                 (loki-assistant-config--finish-load nil nil (error-message-string err))))))))))))

(defun loki-assistant-config-refresh ()
  "重新获取配置；有未保存输入时先确认。"
  (interactive)
  (when (or loki-assistant-config--loading
            (cl-some #'loki-assistant-config--entry-busy loki-assistant-config--entries))
    (user-error "请等待当前请求完成"))
  (when (or (not (cl-some #'loki-assistant-config--dirty-p loki-assistant-config--entries))
            (yes-or-no-p "刷新会丢弃未保存的编辑，继续？"))
    (loki-assistant-config--load)))

(defun loki-assistant-config--submit (entry &optional callback)
  "保存 ENTRY，完成后 CALLBACK 接收成功标志。"
  (when (and loki-assistant-config--saving-all (not callback))
    (user-error "请等待批量保存完成"))
  (when (loki-assistant-config--entry-busy entry) (user-error "配置项正在处理"))
  (when (loki-assistant-config--entry-uncertain entry)
    (user-error "上次保存结果未知，请先等待处理完成并刷新核对"))
  (let ((buffer (current-buffer))
        (generation loki-assistant-config--generation)
        (module (loki-assistant-config--entry-module entry))
        (key (alist-get 'key (loki-assistant-config--entry-item entry)))
        (name (alist-get 'name (loki-assistant-config--entry-item entry)))
        (snapshot (widget-value (loki-assistant-config--entry-field entry)))
        (secret (equal (loki-assistant-config--data-type (loki-assistant-config--entry-item entry)) "password"))
        headers
        done)
    (cl-labels
        ((finish (success text &optional uncertain)
           (unless done
             (setq done t)
             (setf (loki-assistant-config--entry-busy entry) nil
                   (loki-assistant-config--entry-started-at entry) nil
                   (loki-assistant-config--entry-uncertain entry) uncertain
                   (loki-assistant-config--entry-error entry) (unless success text)
                   (loki-assistant-config--entry-result entry) (when success text))
             (loki-assistant-config--status entry)
             (unless (loki-assistant-config--busy-p)
               (loki-assistant-config--stop-wait-timer)
               (unless loki-assistant-config--saving-all
                 (loki-assistant-config--set-widget-text
                  loki-assistant-config--message (format "%s：%s" name text)
                  (if success 'success 'error))))
             (loki-assistant-config--sync-controls)
             (message "%s：%s" name text)
             (when callback (funcall callback success))))
         (in-buffer (action)
           (loki-assistant-config--with-current
            buffer generation
            (lambda ()
              (unless done
                (condition-case err (funcall action)
                  (error (finish nil (error-message-string err))))))))
         (save-failed (result &optional invalid-response)
           (let* ((unknown (or invalid-response
                               (member (plist-get result :error-code)
                                       '("timeout" "transport-error" "invalid-response"))))
                  (reason (loki-assistant-config--error result)))
             (finish nil
                     (if unknown
                         (concat "保存结果未知：" reason
                                 "；请等待处理完成后刷新核对，再决定是否重试。")
                       reason)
                     (not (null unknown)))))
         (store (value)
           (loki-assistant-config--begin entry 'saving)
           ;; 保存包含配置处理器的执行时间，独立于普通读取请求的超时。
           (loki-assistant-request
            'put (concat "/data/" (url-hexify-string module) "/" (url-hexify-string key))
            :body (loki-assistant-config--json value)
            :headers headers
            :timeout loki-assistant-config-save-timeout
            :callback
            (lambda (response)
              (in-buffer
               (lambda ()
                 (let ((stored (plist-get response :data)))
                   (cond
                    ((not (plist-get response :ok)) (save-failed response))
                    ((not (and (listp stored)
                               (equal (alist-get 'module_id stored) module)
                               (equal (alist-get 'key stored) key)
                               (loki-assistant-config--valid-value-p stored)))
                     (save-failed response t))
                    (t
                     (setf (loki-assistant-config--entry-value entry) (alist-get 'value stored)
                           (loki-assistant-config--entry-source entry) (alist-get 'source stored)
                           (loki-assistant-config--entry-pending entry)
                           (equal (alist-get 'status stored) "pending")
                           (loki-assistant-config--entry-baseline entry)
                           (loki-assistant-config--display-value
                            (loki-assistant-config--entry-item entry) (alist-get 'value stored)))
                     ;; 旧请求只更新保存基线，不覆盖执行期间的新输入。
                     (when (equal snapshot (widget-value (loki-assistant-config--entry-field entry)))
                       (widget-value-set (loki-assistant-config--entry-field entry)
                                         (loki-assistant-config--entry-baseline entry)))
                     (loki-assistant-config--mark-field (loki-assistant-config--entry-field entry)
                                                         (loki-assistant-config--entry-item entry))
                     (loki-assistant-config--entry-sync-retry entry)
                     (finish t (cond ((loki-assistant-config--entry-pending entry) "提交已受理")
                                     ((loki-assistant-config--dirty-p entry)
                                      "提交值已保存；仍有新的编辑尚未保存。")
                                     (t "保存成功。"))))))))))))
      (condition-case err
          (let ((value (loki-assistant-config--value entry)))
            (when (or secret (eq (alist-get 'requires_local_auth
                                          (loki-assistant-config--entry-item entry)) t))
              (setq headers (list (cons "Authorization"
                                        (concat "Bearer " (loki-assistant--module-api-token module))))))
            (store value))
        (error (finish nil (error-message-string err)))))))

(defun loki-assistant-config-save ()
  "逐项保存已修改项，显示执行进度；失败时停止并保留输入。"
  (interactive)
  (when (or loki-assistant-config--loading loki-assistant-config--saving-all
            (loki-assistant-config--busy-p))
    (user-error "请等待当前请求完成"))
  (when (cl-some #'loki-assistant-config--entry-uncertain loki-assistant-config--entries)
    (user-error "存在保存结果未知的配置项，请先刷新核对"))
  (let ((pending (cl-remove-if-not #'loki-assistant-config--dirty-p loki-assistant-config--entries))
        current)
    (setq loki-assistant-config--saving-all t
          loki-assistant-config--batch-total (length pending)
          loki-assistant-config--batch-done 0)
    (loki-assistant-config--sync-controls)
    (cl-labels
        ((next (success)
           (when (and success current) (cl-incf loki-assistant-config--batch-done))
           (cond
            ((not success)
             (setq loki-assistant-config--saving-all nil)
             (loki-assistant-config--set-widget-text
              loki-assistant-config--message
              (format "保存已停止（已完成 %d/%d 项）：%s：%s；此前成功项已保存，其余输入保留。"
                      loki-assistant-config--batch-done loki-assistant-config--batch-total
                      (alist-get 'name (loki-assistant-config--entry-item current))
                      (loki-assistant-config--entry-error current))
              'error)
             (loki-assistant-config--sync-controls))
            (pending
             (setq current (pop pending))
             (if (loki-assistant-config--dirty-p current)
                 (loki-assistant-config--submit current #'next)
               (next t)))
            (t
             (setq loki-assistant-config--saving-all nil)
             (widget-value-set
              loki-assistant-config--message
              (cond
               ((zerop loki-assistant-config--batch-total) "没有需要保存的修改。")
               ((cl-some #'loki-assistant-config--dirty-p loki-assistant-config--entries)
                "提交的修改已保存；仍有新的编辑尚未保存。")
               (t (format "全部 %d 项修改保存成功。" loki-assistant-config--batch-done))))
             (loki-assistant-config--sync-controls)))))
      (next t))))

(defun loki-assistant-config--open-module (module)
  "打开 MODULE 独立的配置缓冲区。"
  (let* ((id (alist-get 'id module))
         (buffer (get-buffer-create (format "*Loki 配置 · %s*" id))))
    (with-current-buffer buffer
      (unless (derived-mode-p 'loki-assistant-config-mode)
        (loki-assistant-config-mode)
        (setq loki-assistant-config--module-id id)
        (loki-assistant-config--load module)))
    (pop-to-buffer-same-window buffer)))

;;;###autoload
(defun loki-assistant-config ()
  "选择一个 services 模块并打开它的配置。"
  (interactive)
  (let ((window (selected-window))
        (generation (setq loki-assistant-config--selection-generation (list nil))))
    (loki-assistant-request
     'get "/meta"
     :callback
     (lambda (response)
       (when (and (eq generation loki-assistant-config--selection-generation)
                  (window-live-p window))
         (condition-case err
             (with-selected-window window
               (let ((choices
                      (mapcar (lambda (module)
                                (cons (format "%s · %s" (alist-get 'name module)
                                                       (alist-get 'id module))
                                      module))
                              (loki-assistant-config--modules response))))
                 (unless choices (user-error "没有可配置的模块"))
                 (let ((selected (completing-read "选择配置模块：" choices nil t)))
                   (when-let ((module (cdr (assoc selected choices))))
                     (loki-assistant-config--open-module module)))))
           (quit nil)
           (error (message "%s" (error-message-string err)))))))))

(defun loki-assistant-config--retry (path data-path headers timeout callback)
  "向 PATH 发送空 PUT，再读取 DATA-PATH 的最新配置。"
  (loki-assistant-request
   "PUT" path :headers headers :timeout timeout
   :callback
   (lambda (result)
     (loki-assistant-request "GET" data-path
                             :callback (lambda (data) (funcall callback result data))))))

(defun loki-assistant-config--retry-message (result)
  "从 RESULT 生成重试结果。"
  (cond ((not (plist-get result :ok)) (loki-assistant-config--error result))
        ((equal (alist-get 'status (plist-get result :data)) "pending") "仍待确认")
        (t "已确认")))

(defun loki-assistant-config--entry-sync-retry (entry)
  "按 ENTRY 的待确认状态更新重试按钮。"
  (let ((button (loki-assistant-config--entry-retry-button entry))
        (inhibit-read-only t))
    (cond
     ((and (loki-assistant-config--entry-pending entry) (not button))
      (save-excursion
        (goto-char (loki-assistant-config--entry-retry-point entry))
        (setq button (widget-create 'push-button :format " %[%v%]"
                                    :notify (lambda (&rest _) (loki-assistant-config--entry-retry entry))
                                    "重试"))
        (push button (loki-assistant-config--entry-buttons entry))
        (setf (loki-assistant-config--entry-retry-button entry) button)
        (widget-setup)))
     ((and button (not (loki-assistant-config--entry-pending entry)))
      (widget-delete button)
      (setf (loki-assistant-config--entry-buttons entry)
            (delq button (loki-assistant-config--entry-buttons entry))
            (loki-assistant-config--entry-retry-button entry) nil)))))

(defun loki-assistant-config--entry-retry (entry)
  "重新触发 ENTRY 的待确认变更。"
  (let* ((buffer (current-buffer))
         (generation loki-assistant-config--generation)
         (module (loki-assistant-config--entry-module entry))
         (item (loki-assistant-config--entry-item entry))
         (path (concat "/data/" (url-hexify-string module) "/"
                       (url-hexify-string (alist-get 'key item))))
         (headers (when (eq (alist-get 'requires_local_auth item) t)
                    (list (cons "Authorization" (concat "Bearer " (loki-assistant--module-api-token module))))))
         (snapshot (widget-value (loki-assistant-config--entry-field entry)))
         (dirty (loki-assistant-config--dirty-p entry)))
    (when (or (loki-assistant-config--entry-busy entry) (not (loki-assistant-config--entry-pending entry)))
      (user-error "此配置项当前无法重试"))
    (loki-assistant-config--begin entry 'saving)
    (loki-assistant-config--retry
     path path headers loki-assistant-config-save-timeout
     (lambda (result data)
       (loki-assistant-config--with-current
        buffer generation
        (lambda ()
          (setf (loki-assistant-config--entry-busy entry) nil
                (loki-assistant-config--entry-started-at entry) nil
                (loki-assistant-config--entry-error entry)
                (unless (plist-get result :ok) (loki-assistant-config--retry-message result))
                (loki-assistant-config--entry-result entry)
                (when (plist-get result :ok) (loki-assistant-config--retry-message result)))
          (when (or (plist-get data :ok) (equal (plist-get data :error-code) "config-not-set"))
            (let* ((stored (plist-get data :data))
                   (value (if (assq 'value stored) (alist-get 'value stored) :null))
                   (baseline (loki-assistant-config--display-value item value)))
              (setf (loki-assistant-config--entry-value entry) value
                    (loki-assistant-config--entry-source entry) (alist-get 'source stored)
                    (loki-assistant-config--entry-baseline entry) baseline
                    (loki-assistant-config--entry-pending entry) (not (seq-empty-p (alist-get 'pending stored))))
              (when (and (not dirty)
                         (equal snapshot (widget-value (loki-assistant-config--entry-field entry))))
                (widget-value-set (loki-assistant-config--entry-field entry) baseline)
                (loki-assistant-config--mark-field (loki-assistant-config--entry-field entry) item))))
          (loki-assistant-config--entry-sync-retry entry)
          (loki-assistant-config--status entry)
          (unless (loki-assistant-config--busy-p) (loki-assistant-config--stop-wait-timer))
          (loki-assistant-config--sync-controls)))))))

;;; 结构化配置项

(cl-defstruct (loki-assistant-config--record-section
               (:constructor loki-assistant-config--record-create))
  buffer module metadata start end widgets records
  form form-baseline form-id form-generation inputs saving notice)

(defvar-local loki-assistant-config--record-sections nil)

(defun loki-assistant-config--record-field (key object)
  "读取 OBJECT 中 Metadata 指定的 KEY。"
  (when (stringp key)
    (let ((value (alist-get (intern key) object)))
      (unless (eq value :null) value))))

(defun loki-assistant-config--record-data-path (section key &optional id)
  "返回 SECTION 的配置项 KEY 或记录 ID 的本机路径。"
  (mapconcat #'url-hexify-string
             (delq nil (list "" "data" (loki-assistant-config--record-section-module section)
                             key id))
             "/"))

(defun loki-assistant-config--record-header (section)
  "构造 SECTION 的本机鉴权请求头。"
  (let ((module (loki-assistant-config--record-section-module section)))
    (list (cons "Authorization" (concat "Bearer " (loki-assistant--module-api-token module))))))

(defun loki-assistant-config--record-alive-p (section)
  "判断 SECTION 所在的配置页是否仍然有效。"
  (buffer-live-p (loki-assistant-config--record-section-buffer section)))

(defun loki-assistant-config--record-button (section label function &optional disabled)
  "在 SECTION 中加入调用 FUNCTION 的 LABEL 按钮。"
  (let ((widget (widget-create 'push-button
                               :notify (lambda (&rest _) (funcall function)) label)))
    (when disabled (widget-apply widget :deactivate))
    (push widget (loki-assistant-config--record-section-widgets section))))

(defun loki-assistant-config--record-record-meta (section)
  "获取 SECTION 的记录声明。"
  (alist-get 'record (loki-assistant-config--record-section-metadata section)))

(defun loki-assistant-config--record-default (template values)
  "用已录入的 VALUES 展开默认值 TEMPLATE。"
  (when (stringp template)
    (replace-regexp-in-string
     "{\\([a-z_]+\\)\\(:lowercase\\)?}"
     (lambda (match)
       (let ((value (cdr (assoc (match-string 1 match) values))))
         (if (match-string 2 match) (downcase (or value "")) (or value ""))))
     template t t)))

(defun loki-assistant-config--record-remember-form (section)
  "保存 SECTION 上的输入，供重绘时恢复。"
  (when-let ((inputs (loki-assistant-config--record-section-inputs section)))
    (setf (loki-assistant-config--record-section-form section)
          (mapcar (lambda (entry) (cons (car entry) (widget-value (cdr entry)))) inputs))))

(defun loki-assistant-config--record-detach ()
  "刷新配置页时暂存记录表单。"
  (dolist (section loki-assistant-config--record-sections)
    (loki-assistant-config--record-remember-form section)
    (when-let ((start (loki-assistant-config--record-section-start section)))
      (set-marker start nil)
      (set-marker (loki-assistant-config--record-section-end section) nil))
    (setf (loki-assistant-config--record-section-start section) nil
          (loki-assistant-config--record-section-end section) nil
          (loki-assistant-config--record-section-widgets section) nil
          (loki-assistant-config--record-section-inputs section) nil)))

(defun loki-assistant-config--record-stop ()
  "关闭配置页时注销各记录区块。"
  (dolist (section loki-assistant-config--record-sections)
    (setf (loki-assistant-config--record-section-buffer section) nil)))

(defun loki-assistant-config--record-display-value (field value show-all)
  "返回 FIELD 的可显示文本，密码字段只显示保存状态。"
  (cond ((and show-all (or (null value) (equal value ""))) "未设置")
        ((equal (loki-assistant-config--data-type field) "password") "已保存")
        (t (format "%s" value))))

(defun loki-assistant-config--record-rows (section)
  "在 SECTION 中显示配置记录、状态和操作。"
  (let* ((metadata (loki-assistant-config--record-section-metadata section))
         (record-meta (alist-get 'record metadata))
         (show-all (eq (alist-get 'show_all_fields record-meta) t)))
    (dolist (entry (loki-assistant-config--record-section-records section))
      (let* ((id (car entry))
             (record (cdr entry))
             (title (or (loki-assistant-config--record-field
                         (alist-get 'label_field record-meta) record)
                        (unless (string-empty-p id) id)
                        (alist-get 'title metadata))))
        (insert (propertize title 'face 'bold)
                (if (or (string-empty-p id) (and show-all (equal title id)))
                    "" (concat "  [" id "]"))
                (cond ((alist-get 'delete_pending record) "  待删除")
                      ((alist-get 'validation_error record) "  校验失败")
                      ((alist-get 'pending record) "  待确认")
                      (t "")) "\n")
        (when-let ((reason (alist-get 'validation_error record)))
          (insert "  " (propertize reason 'face 'error) "\n"))
        (dolist (field (append (alist-get 'fields record-meta) nil))
          (let* ((key (alist-get 'key field))
                 (value (loki-assistant-config--record-field key record))
                 (password (equal (loki-assistant-config--data-type field) "password")))
            (when (or show-all
                      (and value (not password) (not (equal key (alist-get 'label_field record-meta)))))
              (insert "  " (alist-get 'label field) "  "
                      (loki-assistant-config--record-display-value field value show-all)
                      "\n"))))
        (insert "  ")
        (when (and (not (alist-get 'delete_pending record))
                   (or (alist-get 'single metadata) (alist-get 'id_field record-meta)))
          (loki-assistant-config--record-button
           section "编辑" (lambda () (loki-assistant-config--record-edit section id))
           (loki-assistant-config--record-section-saving section)))
        (when (and (not (alist-get 'delete_pending record))
                   (not (alist-get 'single metadata)) (alist-get 'id_field record-meta)
                   (not (string-empty-p id)))
          (insert "  ")
          (loki-assistant-config--record-button
           section "删除" (lambda () (loki-assistant-config--record-delete section id))
           (loki-assistant-config--record-section-saving section)))
        (when (alist-get 'pending record)
          (insert "  ")
          (loki-assistant-config--record-button
           section "重试" (lambda () (loki-assistant-config--record-retry section id))
           (loki-assistant-config--record-section-saving section)))
        (insert "\n\n")))))

(defun loki-assistant-config--record-allowed-field-p (section field existing)
  "判断 FIELD 是否用于 SECTION 当前的新增或 EXISTING 记录。"
  (let ((create-fields (alist-get 'create_fields
                                  (loki-assistant-config--record-section-metadata section))))
    (and (not (eq (alist-get 'read_only field) t))
         (or existing (seq-empty-p create-fields)
             (seq-contains-p create-fields (alist-get 'key field) #'equal)))))

(defun loki-assistant-config--record-form (section)
  "在 SECTION 中显示当前记录的内嵌表单。"
  (when-let ((form (loki-assistant-config--record-section-form section)))
    (let* ((record (loki-assistant-config--record-record-meta section))
           (id (loki-assistant-config--record-section-form-id section))
           (row (and id (cdr (assoc id (loki-assistant-config--record-section-records section)))))
           (saving (loki-assistant-config--record-section-saving section)))
      (insert (if id (concat "编辑" (unless (string-empty-p id) (concat "  " id)))
                (or (alist-get 'add_label record) "新增")) "\n")
      (dolist (field (append (alist-get 'fields record) nil))
        (let ((key (alist-get 'key field)))
          (cond
           ((and row (eq (alist-get 'show_all_fields record) t)
                 (eq (alist-get 'read_only field) t)
                 (not (equal key (alist-get 'id_field record))))
            (insert "  " (alist-get 'label field) "  "
                    (loki-assistant-config--record-display-value
                     field (loki-assistant-config--record-field key row) t)
                    "\n"))
           ((and (loki-assistant-config--record-allowed-field-p section field id)
                 (not (and id (equal key (alist-get 'id_field record)))))
            (insert "  " (alist-get 'label field) "  ")
            (let* ((value (or (cdr (assoc key form)) ""))
                   (widget (widget-create 'editable-field :format "%v"
                                          :size 45 :value value
                                          :value-face (loki-assistant-config--input-face field value)
                                          :notify (lambda (input &rest _)
                                                    (loki-assistant-config--mark-field input field))
                                          :secret (when (equal (loki-assistant-config--data-type field) "password") ?*))))
              (push widget (loki-assistant-config--record-section-widgets section))
              (push (cons key widget) (loki-assistant-config--record-section-inputs section))
              (when saving (widget-apply widget :deactivate))
              (when-let ((options (loki-assistant-config--record-field "options" field)))
                (insert "  ")
                (loki-assistant-config--record-button
                 section "选择已有条目"
                 (lambda () (loki-assistant-config--record-choose section key options)) saving)))
            (when-let ((help (alist-get 'description (loki-assistant-config--record-field "checker" field))))
              (when (stringp help) (insert "  " help)))
            (when (stringp (alist-get 'default_template field))
              (insert "  留空使用默认值"))
            (insert "\n")))))
      (insert "  ")
      (loki-assistant-config--record-button
       section (if saving "正在保存…" "保存记录")
       (lambda () (loki-assistant-config--record-save section)) saving)
      (insert "  ")
      (loki-assistant-config--record-button
       section "取消编辑" (lambda () (loki-assistant-config--record-edit section nil t)) saving)
      (insert "\n\n"))))

(defun loki-assistant-config--record-render (section)
  "仅重绘 SECTION，保留配置页其他控件。"
  (when (and (loki-assistant-config--record-alive-p section)
             (loki-assistant-config--record-section-start section))
    (with-current-buffer (loki-assistant-config--record-section-buffer section)
      (let* ((inhibit-read-only t)
             (inhibit-modification-hooks t)
             (modified (buffer-modified-p))
             (start (loki-assistant-config--record-section-start section))
             (end (loki-assistant-config--record-section-end section))
             (offset (and (<= start (point)) (< (point) end) (- (point) start))))
        (loki-assistant-config--record-remember-form section)
        (save-excursion
          (mapc #'widget-delete (loki-assistant-config--record-section-widgets section))
          (setf (loki-assistant-config--record-section-widgets section) nil
                (loki-assistant-config--record-section-inputs section) nil)
          (delete-region start end)
          (goto-char start)
          (let* ((metadata (loki-assistant-config--record-section-metadata section))
                 (record (alist-get 'record metadata)))
            (insert (propertize (alist-get 'title metadata) 'face 'bold) "\n")
            (unless (and (alist-get 'single metadata)
                         (loki-assistant-config--record-section-records section))
              (loki-assistant-config--record-button
               section (or (alist-get 'add_label record) (if (alist-get 'single metadata) "设置" "新增"))
               (lambda () (loki-assistant-config--record-edit section nil))
               (loki-assistant-config--record-section-saving section)))
            (insert "\n\n"))
          (when-let ((notice (loki-assistant-config--record-section-notice section)))
            (insert notice "\n\n"))
          (loki-assistant-config--record-form section)
          (loki-assistant-config--record-rows section)
          (set-marker end (point))
          (widget-setup))
        (when offset (goto-char (min (+ start offset) end)))
        (restore-buffer-modified-p modified)))))

(defun loki-assistant-config--record-edit (section id &optional cancel)
  "为 SECTION 打开记录 ID 的表单。CANCEL 关闭表单。"
  (setf (loki-assistant-config--record-section-inputs section) nil
        (loki-assistant-config--record-section-form-id section) id
        (loki-assistant-config--record-section-form-generation section) (list nil)
        (loki-assistant-config--record-section-notice section) nil
        (loki-assistant-config--record-section-form section)
        (unless cancel
          (let* ((record (and id (cdr (assoc id (loki-assistant-config--record-section-records section)))))
                 (fields (alist-get 'fields (loki-assistant-config--record-record-meta section))))
            (mapcar (lambda (field)
                      (let ((key (alist-get 'key field)))
                        (let ((value (loki-assistant-config--record-field key record)))
                          (cons key (cond ((or (null value) (equal (loki-assistant-config--data-type field) "password")) "")
                                          ((stringp value) value)
                                          (t (loki-assistant-config--json value)))))))
                    (append fields nil)))))
  (setf (loki-assistant-config--record-section-form-baseline section)
        (copy-tree (loki-assistant-config--record-section-form section)))
  (loki-assistant-config--record-render section))

(defun loki-assistant-config--record-choose (section key options)
  "读取 OPTIONS 中的已有条目，设置 SECTION 的字段 KEY。"
  (let* ((generation (loki-assistant-config--record-section-form-generation section))
         (buffer (loki-assistant-config--record-section-buffer section)))
    (loki-assistant-request
     "GET" (alist-get 'path options)
     :callback
     (lambda (response)
       (when (and (loki-assistant-config--record-alive-p section)
                  (eq generation (loki-assistant-config--record-section-form-generation section)))
         (with-current-buffer buffer
           (let ((items (loki-assistant-config--record-field
                         (alist-get 'items_field options) (plist-get response :data))))
             (if (and (plist-get response :ok) (vectorp items))
                 (when-let ((widget (cdr (assoc key (loki-assistant-config--record-section-inputs section))))
                            ((> (length items) 0)))
                   (let ((selected (completing-read "选择已有条目：" (append items nil) nil t)))
                     (when (and (not (string-empty-p selected))
                                (eq widget (cdr (assoc key (loki-assistant-config--record-section-inputs section)))))
                       (widget-value-set widget selected))))
               (message "%s" (loki-assistant-config--error response))))))))))

(defun loki-assistant-config--record-records (value record single)
  "将配置 VALUE 转为 RECORD 的界面行。"
  (if single
      (when (listp value) (list (cons "" value)))
    (when (vectorp value)
      (cl-loop for item across value for index from 1
               collect (cons (or (loki-assistant-config--record-field
                                  (alist-get 'id_field record) item)
                                 (number-to-string index))
                             item)))))

(defun loki-assistant-config--record-retry (section id)
  "重新触发 SECTION 中 ID 对应的待确认变更。"
  (let* ((key (alist-get 'key (loki-assistant-config--record-record-meta section)))
         (path (loki-assistant-config--record-data-path section key (unless (string-empty-p id) id)))
         (data-path (loki-assistant-config--record-data-path section key))
         (headers (when (eq (alist-get 'requires_local_auth
                                      (loki-assistant-config--record-section-metadata section)) t)
                    (loki-assistant-config--record-header section))))
    (when (loki-assistant-config--record-section-saving section)
      (user-error "请等待当前请求完成"))
    (setf (loki-assistant-config--record-section-saving section) t
          (loki-assistant-config--record-section-notice section) "正在重试…")
    (loki-assistant-config--record-render section)
    (loki-assistant-config--retry
     path data-path headers (loki-assistant-config--record-timeout section)
     (lambda (result data)
       (when (loki-assistant-config--record-alive-p section)
         (setf (loki-assistant-config--record-section-saving section) nil
               (loki-assistant-config--record-section-notice section)
               (loki-assistant-config--retry-message result))
         (loki-assistant-config--record-clear-rejected-fields section id result)
         (when (or (plist-get data :ok) (equal (plist-get data :error-code) "config-not-set"))
           (loki-assistant-config--record-set-data section (plist-get data :data)))
         (loki-assistant-config--record-render section))))))

(defun loki-assistant-config--record-clear-rejected-fields (section id response)
  "按 RESPONSE 清除 SECTION 中 ID 的已拒绝密码字段。"
  (let ((headers (plist-get response :headers))
        (case-fold-search t))
    (when (and (stringp headers)
               (string-match "^x-loki-config-cleared-fields:[ \t]*\\([^\r\n]+\\)" headers))
      (let ((keys (split-string (match-string 1 headers) "[ ,\t]+" t))
            (row (assoc id (loki-assistant-config--record-section-records section))))
        (when row
          (setcdr row
                  (seq-remove (lambda (field) (member (symbol-name (car field)) keys))
                              (cdr row)))
          (setf (alist-get 'pending (cdr row)) t
                (alist-get 'validation_error (cdr row)) (loki-assistant-config--error response)))
        (when (equal id (loki-assistant-config--record-section-form-id section))
          (dolist (key keys)
            (when-let ((input (cdr (assoc key (loki-assistant-config--record-section-inputs section)))))
              (widget-apply input :activate)
              (widget-value-set input ""))))))))

(defun loki-assistant-config--record-delete (section id)
  "确认后删除 SECTION 中主键为 ID 的记录。"
  (when (loki-assistant-config--record-section-saving section)
    (user-error "请等待当前请求完成"))
  (when (yes-or-no-p (format "删除配置记录 %s？" id))
    (let* ((metadata (loki-assistant-config--record-section-metadata section))
           (key (alist-get 'key (loki-assistant-config--record-record-meta section)))
           (headers (when (eq (alist-get 'requires_local_auth metadata) t)
                      (loki-assistant-config--record-header section))))
      (setf (loki-assistant-config--record-section-saving section) t
            (loki-assistant-config--record-section-notice section) "正在删除…")
      (loki-assistant-config--record-render section)
      (loki-assistant-request
       "DELETE" (loki-assistant-config--record-data-path section key id)
       :headers headers :timeout (loki-assistant-config--record-timeout section)
       :callback
       (lambda (response)
         (when (loki-assistant-config--record-alive-p section)
           (setf (loki-assistant-config--record-section-saving section) nil)
           (let ((records (loki-assistant-config--record-section-records section)))
             (setf
              (loki-assistant-config--record-section-notice section)
              (cond
               ((not (plist-get response :ok)) (loki-assistant-config--error response))
               ((eq (plist-get response :status) 202)
                (when-let ((record (assoc id records)))
                  (setf (alist-get 'pending (cdr record)) t
                        (alist-get 'delete_pending (cdr record)) t
                        (alist-get 'validation_error (cdr record)) nil))
                "删除待确认")
               (t
                (setf (loki-assistant-config--record-section-records section)
                      (assoc-delete-all id records))
                (format "已删除 %s" id)))))
           (when (and (plist-get response :ok)
                      (equal id (loki-assistant-config--record-section-form-id section)))
             (let ((notice (loki-assistant-config--record-section-notice section)))
               (loki-assistant-config--record-edit section nil t)
               (setf (loki-assistant-config--record-section-notice section) notice)))
           (loki-assistant-config--record-render section)))))))

(defun loki-assistant-config--record-save (section)
  "保存 SECTION 当前编辑的单条记录。"
  (loki-assistant-config--record-remember-form section)
  (let* ((meta (loki-assistant-config--record-section-metadata section))
         (record (loki-assistant-config--record-record-meta section))
         (single (alist-get 'single meta))
         (id-field (alist-get 'id_field record))
         (existing (loki-assistant-config--record-section-form-id section))
         (baseline (loki-assistant-config--record-section-form-baseline section))
         (inputs (loki-assistant-config--record-section-form section))
         (body (make-hash-table :test #'equal))
         fields)
    (dolist (field (append (alist-get 'fields record) nil))
      (let* ((key (alist-get 'key field))
             (input (string-trim (or (cdr (assoc key inputs)) "")))
             (before (string-trim (or (cdr (assoc key baseline)) ""))))
        (when (and (loki-assistant-config--record-allowed-field-p section field existing)
                   (or (not existing)
                       (and (not (equal key id-field)) (not (equal input before)))))
          (let ((value (if (and (not existing) (string-empty-p input))
                           (loki-assistant-config--record-default
                            (alist-get 'default_template field) fields)
                         input)))
            (when (and (eq (alist-get 'required field) t) (string-empty-p (or value "")))
              (user-error "%s不能为空" (alist-get 'label field)))
            (when value
              (push (cons key value) fields)
              (puthash key (if (and existing (string-empty-p value)) :null
                             (loki-assistant-config--parse-input field value))
                       body))))))
    (when (and existing (zerop (hash-table-count body)))
      (user-error "没有修改的字段"))
    (let* ((id (and (not single) (or existing (cdr (assoc id-field fields)))))
           (append-item (and (not single) (not id)))
           (path (loki-assistant-config--record-data-path section (alist-get 'key record) id))
           (headers (append
                     (when existing '(("Content-Type" . "application/merge-patch+json")))
                     (when (and id (not existing)) '(("If-None-Match" . "*")))
                     (when (eq (alist-get 'requires_local_auth meta) t)
                       (loki-assistant-config--record-header section)))))
      (when id (remhash id-field body))
      (setf (loki-assistant-config--record-section-saving section) t
            (loki-assistant-config--record-section-notice section) nil)
      (loki-assistant-config--record-render section)
      (loki-assistant-request
       (cond (existing "PATCH") (append-item "POST") (t "PUT")) path
       :headers headers
       :timeout (loki-assistant-config--record-timeout section)
       :body (if append-item (vector body) body)
       :callback
       (lambda (response)
         (when (loki-assistant-config--record-alive-p section)
           (setf (loki-assistant-config--record-section-saving section) nil)
           (let* ((stored (plist-get response :data))
                  (value (alist-get 'value stored)))
             (if (and (plist-get response :ok)
                      (if id
                          (and (equal (alist-get 'instance stored) id) (listp value))
                        (if single (listp value) (vectorp value))))
                 (progn
                   (if id
                       (setf (alist-get id (loki-assistant-config--record-section-records section)
                                        nil nil #'equal)
                             (if (equal (alist-get 'status stored) "pending")
                                 (cons '(pending . t) value)
                               value))
                     (let ((records (loki-assistant-config--record-records value record single)))
                       (when (and single (equal (alist-get 'status stored) "pending"))
                         (setf (alist-get "" records nil nil #'equal) (cons '(pending . t) value)))
                       (setf (loki-assistant-config--record-section-records section) records)))
                   (loki-assistant-config--record-edit section nil t))
               (loki-assistant-config--record-clear-rejected-fields section id response)
               (setf (loki-assistant-config--record-section-notice section)
                     (loki-assistant-config--error response))))
           (loki-assistant-config--record-render section)))))))

(defun loki-assistant-config--record-timeout (section)
  "读取 SECTION 的保存等待时间。"
  (or (alist-get 'save_timeout_seconds (loki-assistant-config--record-section-metadata section))
      loki-assistant-config-save-timeout))

(defun loki-assistant-config--record-insert (module item values)
  "按 ITEM 的结构元数据在当前页面渲染 MODULE 的配置记录。"
  (let* ((key (alist-get 'key item))
         (record (cons (cons 'key key) (alist-get 'structure item)))
         (single (equal (loki-assistant-config--data-type item) "structure"))
         (data (loki-assistant-config--record-field key values))
         (section (cl-find-if
                   (lambda (section)
                     (and (equal module (loki-assistant-config--record-section-module section))
                          (equal key (alist-get 'key (loki-assistant-config--record-record-meta section)))))
                   loki-assistant-config--record-sections))
         (position (point)))
    (unless section
      (setq section (loki-assistant-config--record-create
                     :module module :buffer (current-buffer)))
      (push section loki-assistant-config--record-sections))
    (unless (loki-assistant-config--record-section-saving section)
      (setf (loki-assistant-config--record-section-notice section) nil))
    (widget-insert "\n")
    (setf (loki-assistant-config--record-section-metadata section)
          `((title . ,(alist-get 'name item)) (record . ,record) (single . ,single)
            (save_timeout_seconds . ,(alist-get 'save_timeout_seconds item))
            (requires_local_auth . ,(alist-get 'requires_local_auth item))
            (create_fields . ,(alist-get 'create_fields item)))
          (loki-assistant-config--record-section-start section) (copy-marker position)
          (loki-assistant-config--record-section-end section) (copy-marker position))
    (loki-assistant-config--record-set-data section data)
    (loki-assistant-config--record-render section)))

(defun loki-assistant-config--record-set-data (section data)
  "将 DATA 中的生效记录和待确认记录填入 SECTION。"
  (let* ((meta (loki-assistant-config--record-section-metadata section))
         (single (alist-get 'single meta))
         (records (loki-assistant-config--record-records
                   (alist-get 'value data) (alist-get 'record meta) single)))
    (dolist (pending (append (alist-get 'pending data) nil))
      (when-let ((id (if single "" (alist-get 'key pending)))
                 (value (alist-get 'value pending)))
        (let ((error (alist-get 'message (alist-get 'error pending))))
          (setf (alist-get id records nil nil #'equal)
                (append (when error (list (cons 'validation_error error)))
                        (when (equal (alist-get 'operation pending) "delete")
                          (list '(delete_pending . t)))
                        (cons '(pending . t) value))))))
    (setf (loki-assistant-config--record-section-records section) records)))

(provide 'loki-assistant-config)
;;; loki-assistant-config.el ends here
