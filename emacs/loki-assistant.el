;;; loki-assistant.el --- Loki Assistant 的 Emacs 入口 -*- lexical-binding: t; -*-

;; Version: 0.1.0
;; Package-Requires: ((emacs "29.1") (logview "0.19.3") (agent-shell "0.94.1"))
;; Keywords: convenience

;;; Commentary:

;; 通过 services pfile 发现本机服务，异步访问 HTTP API 并发送通知。

;;; Code:

(require 'cl-lib)
(require 'json)
(require 'subr-x)
(require 'url)
(require 'url-http)

(defvar url-http-response-status)
(defvar url-http-end-of-headers)

(declare-function consult--read "consult")
(declare-function consult--lookup-cdr "consult")

(defgroup loki-assistant nil
  "Loki Assistant 的 Emacs 集成。"
  :group 'applications
  :prefix "loki-assistant-")

(defun loki-assistant--mode-line-keys (shortcuts &optional help-key)
  "格式化状态栏快捷键 SHORTCUTS，HELP-KEY 默认为问号。"
  (concat " · C-c C-q 退出  " (or help-key "?") " 帮助"
          (when shortcuts (concat "  " shortcuts))))

(defun loki-assistant--read-menu (prompt entries)
  "用 PROMPT 从 ENTRIES 中选择并返回值。
每个条目为 (显示名称 值 [搜索别名])，搜索别名在候选中隐藏。"
  (let ((candidates
         (mapcar (lambda (entry)
                   (let ((label (nth 0 entry))
                         (aliases (nth 2 entry)))
                     (cons (if aliases
                               (concat label (propertize (concat " " aliases) 'invisible t))
                             label)
                           (nth 1 entry))))
                 entries))
        (keymap (make-sparse-keymap))
        ;; 单字母前缀优先，拼音片段仍可匹配。
        (completion-styles (delete-dups (append '(basic) completion-styles '(substring)))))
    (keymap-set keymap "C-c C-q" #'abort-recursive-edit)
    (if (require 'consult nil t)
        (consult--read candidates :prompt prompt :keymap keymap :sort nil
                       :require-match t :lookup #'consult--lookup-cdr :default "")
      (cdr (assoc (minibuffer-with-setup-hook
                      (lambda ()
                        (set-keymap-parent keymap (current-local-map))
                        (use-local-map keymap))
                    (completing-read prompt candidates nil t))
                  candidates)))))

(autoload 'loki-assistant-logs "loki-assistant-log" "浏览 Loki Assistant 日志。" t)
(autoload 'loki-assistant-config "loki-assistant-config"
  "打开 services 配置管理界面。" t)
(autoload 'loki-assistant-devices "loki-assistant-devices"
  "打开已绑定设备管理界面。" t)
(autoload 'loki-assistant-read-later "loki-assistant-read-later"
  "打开稍后阅读列表。" t)
(autoload 'loki-assistant-xbloom-sync "loki-assistant-xbloom"
  "按照当前 Org 文件头的设置同步 xBloom 咖啡配方。" t)
(autoload 'loki-assistant-xbloom-mode "loki-assistant-xbloom"
  "在当前 Org 文件中启用 xBloom 配方同步。" t)
(autoload 'loki-assistant-llm-usage "loki-assistant-llm-usage"
  "打开 LLM 用量查询界面。" t)
(autoload 'loki-assistant-mail-open-inbox "loki-assistant-mail"
  "打开 mu4e 中配置的收件箱书签。" t)
(autoload 'loki-assistant-mail-open-message "loki-assistant-mail"
  "按 Message-ID 打开 mu4e 中的邮件。" t)
(autoload 'loki-assistant-slack-select-chat "loki-assistant-slack-chat"
  "通过 Consult 选择 Slack 聊天。" t)

;;;###autoload
(autoload 'loki-assistant-command-map "loki-assistant-command-menu"
  "通过 Consult 选择并执行 Loki Assistant 命令。" t)

(defcustom loki-assistant-service-file "~/.assistant/service"
  "services 地址信息文件；每次请求重新读取。"
  :type 'file
  :group 'loki-assistant)

(defcustom loki-assistant-request-timeout 10
  "HTTP 请求等待回调的最长秒数。超时不会自动重试。"
  :type 'number
  :group 'loki-assistant)

(defun loki-assistant--module-api-token (module)
  "读取 services 为 MODULE 创建的本机凭据。"
  (let* ((path (expand-file-name (format "%s-api-token" module)
                                 (file-name-directory
                                  (expand-file-name loki-assistant-service-file))))
         (mode (file-modes path)))
    (unless (and (integerp mode) (zerop (logand mode #o077))
                 (file-regular-p path) (not (file-symlink-p path)))
      (error "模块本机凭据不可用，请确认 services 已运行"))
    (with-temp-buffer
      (insert-file-contents-literally path nil 0 64)
      (let ((token (string-trim (buffer-string))))
        (unless (and (= (length token) 43) (string-match-p "\\`[-_[:alnum:]]+\\'" token))
          (error "模块本机凭据格式无效"))
        token))))

(defun loki-assistant--service-url ()
  "读取 pfile，返回经过 loopback 校验的 HTTP 地址。"
  (let* ((service (with-temp-buffer
                    (insert-file-contents
                     (expand-file-name loki-assistant-service-file))
                    (json-parse-buffer :object-type 'alist)))
         (host (alist-get 'host service))
         (port (alist-get 'port service)))
    (unless (and (stringp host)
                 (or (member host '("::1" "0:0:0:0:0:0:0:1"))
                     (and (string-match-p
                           "\\`127\\(?:\\.[0-9]\\{1,3\\}\\)\\{3\\}\\'" host)
                          (cl-every (lambda (part) (<= (string-to-number part) 255))
                                    (split-string host "\\."))))
                 (integerp port) (<= 1 port 65535))
      (error "services pfile 必须包含有效的 loopback host 和 port"))
    (format "http://%s:%d"
            (if (string-match-p ":" host) (concat "[" host "]") host)
            port)))

(defun loki-assistant--query-string (params)
  "将 PARAMS alist 编码为查询字符串，保留重复键。"
  (mapconcat
   (lambda (pair)
     (concat (url-hexify-string (format "%s" (car pair))) "="
             (url-hexify-string (if (null (cdr pair)) ""
                                 (format "%s" (cdr pair))))))
   params "&"))

(defun loki-assistant--failure (code message)
  "构造带 CODE 和 MESSAGE 的本地错误响应。"
  (list :ok nil :status nil :headers nil :body nil :data nil
        :error-code code :error-message message))

(defun loki-assistant--response (status)
  "从当前 URL buffer 解析响应和回调 STATUS。"
  (let* ((http-status url-http-response-status)
         ;; URL 的 marker 指向分隔空行的最后一个换行符。
         (end (and url-http-end-of-headers (1+ url-http-end-of-headers)))
         (headers (and end (buffer-substring-no-properties (point-min) end)))
         (body (and end (decode-coding-string
                         (buffer-substring-no-properties end (point-max)) 'utf-8)))
         (success (and http-status (<= 200 http-status) (< http-status 300)))
         data parse-error)
    (when (and body (not (string-empty-p (string-trim body))))
      (condition-case err
          (setq data (json-parse-string body :object-type 'alist
                                        :array-type 'array
                                        :null-object :null :false-object :false))
        (error (setq parse-error (error-message-string err)))))
    (let* ((object (and (listp data) data))
           (nested-error (alist-get 'error object))
           (error-object (if (consp nested-error) nested-error object))
           (code (alist-get 'code error-object))
           (message (alist-get 'message error-object)))
      (list :ok (and success (not parse-error))
            :status http-status :headers headers :body body :data data
            :error-code (cond ((not http-status) "transport-error")
                              ((not success) (or code "http-error"))
                              (parse-error "invalid-response"))
            :error-message
            (cond ((not http-status)
                   (format "无法连接 services：%s" (plist-get status :error)))
                  ((not success) (or message (format "services 返回 HTTP %s" http-status)))
                  (parse-error (concat "services 响应不是有效 JSON：" parse-error)))))))

(defun loki-assistant--report-response (response)
  "显示 RESPONSE 的成功状态或明确的失败原因。"
  (if (plist-get response :ok)
      (message "services 请求成功（HTTP %s）" (plist-get response :status))
    (message "services 请求失败 [%s]（HTTP %s）：%s"
             (plist-get response :error-code)
             (or (plist-get response :status) "无响应")
             (plist-get response :error-message))))

(cl-defun loki-assistant-request
    (method path &key params body headers callback
            (timeout loki-assistant-request-timeout))
  "异步访问 services，使用 HTTP METHOD 和绝对 PATH。
METHOD 接受字符串或符号；PATH 可含查询参数，但不能包含主机或 fragment。
PARAMS 为查询参数 alist，键和值会进行 URL 编码，支持重复键。
BODY 为 nil 时不发送正文；字符串按原样发送，其他值编码为 JSON。
JSON null 和 false 分别用 :null、:false 表示。HEADERS 为额外请求头 alist。
CALLBACK 接收一个 plist，包含 :ok、:status、:headers、:body、:data、
:error-code 和 :error-message。JSON 对象为 symbol 键 alist，数组为 vector。
非空响应正文应为 JSON；原始正文和响应头始终保留。
发现、连接、HTTP 和解析失败通过同一回调反馈；无 CALLBACK 时显示结果。
TIMEOUT 是正数秒，超时结果未知，不会自动重试。回调至多执行一次。
返回请求 buffer；无法开始请求时返回 nil。buffer 由本函数负责清理。
参数无效会直接报错；发现失败的回调可能在本函数返回前执行。"
  (setq method (upcase (if (symbolp method) (symbol-name method) method)))
  (unless (string-match-p "\\`[A-Z]+\\'" method)
    (user-error "HTTP method 必须是非空字母串"))
  (unless (and (stringp path) (string-prefix-p "/" path)
               (not (string-prefix-p "//" path))
               (not (string-match-p "[[:space:]#]" path)))
    (user-error "HTTP path 必须是无空白和 fragment 的本机绝对路径"))
  (unless (and (numberp timeout) (> timeout 0))
    (user-error "HTTP timeout 必须是正数"))
  (let* ((callback (or callback #'loki-assistant--report-response))
         (query (loki-assistant--query-string params))
         (url-request-method (encode-coding-string method 'us-ascii))
         (url-request-extra-headers
          ;; URL 会拼接请求头和正文，统一使用字节字符串以保留正文的 UTF-8 编码。
          (mapcar (lambda (header)
                    (cons (encode-coding-string (car header) 'us-ascii)
                          (encode-coding-string (cdr header) 'utf-8)))
                  (append headers
                          (unless (assoc-string "Content-Type" headers t)
                            '(("Content-Type" . "application/json; charset=utf-8"))))))
         (json-null :null)
         (json-false :false)
         (url-request-data
          (and body (encode-coding-string
                     (if (stringp body) body (json-encode body)) 'utf-8)))
         ;; 本机调用不使用代理、Cookie、缓存或重定向。
         (url-proxy-services nil)
         (url-using-proxy nil)
         (url-automatic-caching nil)
         (url-max-redirections 0)
         response-buffer timer done base-url)
    (cl-labels
        ((finish (response)
           (unless done
             (setq done t)
             (when timer (cancel-timer timer))
             (when (buffer-live-p response-buffer)
               (when (equal (plist-get response :error-code) "timeout")
                 (let ((process (get-buffer-process response-buffer)))
                   (when process (delete-process process))))
               (kill-buffer response-buffer))
             (funcall callback response))))
      (condition-case err
          (setq base-url (loki-assistant--service-url))
        (error (finish (loki-assistant--failure
                        "service-discovery-failed" (error-message-string err)))))
      (unless done
        (condition-case err
            (progn
              (setq response-buffer
                    (url-retrieve
                     (concat base-url path
                             (unless (string-empty-p query)
                               (concat (if (string-match-p "?" path) "&" "?") query)))
                     (lambda (status)
                       (unless done
                         (setq response-buffer (current-buffer))
                         (finish (loki-assistant--response status))))
                     nil t t))
              (unless done
                (if (not (buffer-live-p response-buffer))
                    (finish (loki-assistant--failure
                             "transport-error" "无法创建 services HTTP 请求"))
                  (setq timer
                        (run-at-time
                         timeout nil
                         (lambda ()
                           (finish (loki-assistant--failure
                                    "timeout" "services 请求超时，处理结果未知"))))))))
          (error
           (if done (signal (car err) (cdr err))
             (finish (loki-assistant--failure
                      "transport-error" (error-message-string err)))))))
      (and (buffer-live-p response-buffer) response-buffer))))

(cl-defun loki-assistant-notify
    (buffer title &key body app (module-id "emacs") payload callback
            (timeout loki-assistant-request-timeout))
  "向 services 的 MODULE-ID 处理器发送 BUFFER、TITLE、可选 BODY 和 APP。
BUFFER 接受存活 buffer 对象或非空名称；nil 表示不指定目标 buffer。
APP 为应用标识，省略时由默认处理器使用 emacs；图标由 center 提供；PAYLOAD 为附加 JSON 数据。
CALLBACK 和 TIMEOUT 与 `loki-assistant-request' 相同。
HTTP 成功仅表示处理器已响应，系统通知展示状态需用 notification_id 查询。
已返回的 failed 通知回执也按失败反馈；无 CALLBACK 时显示成功或失败原因。"
  (when (bufferp buffer)
    (unless (buffer-live-p buffer) (user-error "通知目标 buffer 已关闭"))
    (setq buffer (buffer-name buffer)))
  (dolist (field `(("title" . ,title) ("module-id" . ,module-id)
                   ,@(when buffer `(("buffer" . ,buffer)))))
    (unless (and (stringp (cdr field)) (not (string-empty-p (string-trim (cdr field)))))
      (user-error "通知 %s 必须是非空字符串" (car field))))
  (dolist (value (list body app))
    (unless (or (null value) (stringp value))
      (user-error "通知正文和应用标识必须为字符串或 nil")))
  (loki-assistant-request
   "POST" "/emacs/receiver/notification"
   :body `((module_id . ,module-id) (title . ,title)
           ,@(when buffer `((buffer . ,buffer)))
           ,@(when body `((body . ,body)))
           ,@(when app `((app . ,app)))
           ,@(when payload `((payload . ,payload))))
   :timeout timeout
   :callback
   (lambda (response)
     (let* ((data (plist-get response :data))
            (result (and (listp data) (alist-get 'result data))))
       (when (and (plist-get response :ok) (listp result)
                  (equal (alist-get 'status result) "failed"))
         (setq response (plist-put response :ok nil)
               response (plist-put response :error-code (alist-get 'code result))
               response (plist-put response :error-message (alist-get 'message result))))
       (if callback (funcall callback response)
         (if (plist-get response :ok)
             (message "通知处理器 %s 已响应：%s（%s）"
                      module-id title
                      (or (and (listp result) (alist-get 'status result)) "成功"))
           (loki-assistant--report-response response)))))))

(defcustom loki-assistant-agent-shell-enabled t
  "非 nil 时，在 macOS 上自动加载 `loki-assistant-agent-shell'。
扩展只会在 `agent-shell' 已加载后安装 mode hook。"
  :type 'boolean
  :group 'loki-assistant)

(defun loki-assistant--setup-agent-shell ()
  "在 `agent-shell' 已加载后安装 Loki Assistant 扩展。"
  (when (and loki-assistant-agent-shell-enabled
             (eq system-type 'darwin)
             (featurep 'agent-shell))
    (require 'loki-assistant-agent-shell)
    ;; 如果用户配置中还挂着上游 macext 包，避免新旧通知同时发送。
    (when (fboundp 'agent-shell-macext-setup)
      (remove-hook 'agent-shell-mode-hook #'agent-shell-macext-setup))
    (add-hook 'agent-shell-mode-hook #'loki-assistant-agent-shell-setup)
    (add-hook 'agent-shell-viewport-edit-mode-hook
              #'loki-assistant-agent-shell--setup-dnd)
    (add-hook 'agent-shell-viewport-view-mode-hook
              #'loki-assistant-agent-shell--setup-dnd)))

(if (featurep 'agent-shell)
    (loki-assistant--setup-agent-shell)
  (with-eval-after-load 'agent-shell
    (loki-assistant--setup-agent-shell)))

(require 'loki-assistant-monitor)
(loki-assistant-monitor-mode 1)

(with-eval-after-load 'org-edna
  (require 'loki-assistant-edna))

(with-eval-after-load 'mu4e
  (require 'loki-assistant-mail))

(autoload 'loki-assistant-doc-get "loki-assistant-doc")
(autoload 'loki-assistant-doc-capture-full "loki-assistant-doc")

(provide 'loki-assistant)
;;; loki-assistant.el ends here
