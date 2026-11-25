;;; loki-assistant-devices.el --- 已绑定设备管理 -*- lexical-binding: t; -*-

;;; Commentary:

;; 展示 services 的绑定设备；新增设备时在当前 buffer 展开服务端二维码。

;;; Code:

(require 'loki-assistant)
(require 'button)

(defvar-local loki-assistant-devices--generation nil)
(defvar-local loki-assistant-devices--list-generation nil)
(defvar-local loki-assistant-devices--pair-generation nil)
(defvar-local loki-assistant-devices--peers [])
(defvar-local loki-assistant-devices--loading nil)
(defvar-local loki-assistant-devices--busy nil)
(defvar-local loki-assistant-devices--error nil)
(defvar-local loki-assistant-devices--expanded nil)
(defvar-local loki-assistant-devices--panel nil)
(defvar-local loki-assistant-devices--panel-error nil)
(defvar-local loki-assistant-devices--timer nil)
(defvar-local loki-assistant-devices--polling nil)

(defvar loki-assistant-devices-mode-map
  (let ((map (make-sparse-keymap)))
    (set-keymap-parent map special-mode-map)
    (define-key map (kbd "g") #'loki-assistant-devices-refresh)
    (define-key map (kbd "a") #'loki-assistant-devices-add)
    (define-key map (kbd "TAB") #'forward-button)
    (define-key map (kbd "<backtab>") #'backward-button)
    (define-key map (kbd "RET") #'push-button)
    (define-key map (kbd "q") #'loki-assistant-devices-quit)
    (define-key map (kbd "C-c C-q") #'loki-assistant-devices-quit)
    (define-key map (kbd "?") #'describe-mode)
    map)
  "设备管理界面的快捷键。")

(define-derived-mode loki-assistant-devices-mode special-mode "Loki Devices"
  "展示已绑定设备，在当前 buffer 中展开新增设备信息。

按 g 刷新，a 新增设备，TAB 或 S-TAB 移动到按钮，RET 激活按钮。
按 C-c C-q 退出并取消未完成的邀请，按 ? 查看模式帮助。"
  (setq-local truncate-lines nil)
  (setq-local mode-line-process
              (loki-assistant--mode-line-keys "g 刷新  a 新增设备  TAB/S-TAB 切换  RET 操作"))
  (setq loki-assistant-devices--generation (list nil))
  (add-hook 'kill-buffer-hook #'loki-assistant-devices--cleanup nil t)
  (add-hook 'change-major-mode-hook #'loki-assistant-devices--cleanup nil t))

(defun loki-assistant-devices--valid-id-p (id)
  "判断邀请 ID 是否可以安全用于固定 API 路径。"
  (and (stringp id)
       (string-match-p "\\`[[:xdigit:]]\\{8\\}-[[:xdigit:]]\\{4\\}-[[:xdigit:]]\\{4\\}-[[:xdigit:]]\\{4\\}-[[:xdigit:]]\\{12\\}\\'" id)))

(defun loki-assistant-devices--endpoint-p (id)
  "判断 ID 是否为规范的 Endpoint ID。"
  (and (stringp id) (string-match-p "\\`[0-9a-f]\\{64\\}\\'" id)))

(defun loki-assistant-devices--current (buffer generation action)
  "BUFFER 存活且 GENERATION 有效时执行 ACTION。"
  (when (buffer-live-p buffer)
    (with-current-buffer buffer
      (when (eq generation loki-assistant-devices--generation)
        (funcall action)))))

(defun loki-assistant-devices--stop-timer ()
  "停止当前邀请的状态查询。"
  (when (timerp loki-assistant-devices--timer)
    (cancel-timer loki-assistant-devices--timer))
  (setq loki-assistant-devices--timer nil))

(defun loki-assistant-devices--cancel (id)
  "尝试取消 ID 对应的邀请，不因失败自动重试。"
  (when (loki-assistant-devices--valid-id-p id)
    (loki-assistant-request "DELETE" (concat "/network/pairings/" id)
                            :callback #'ignore)))

(defun loki-assistant-devices--collapse ()
  "收起新增信息，并撤销尚未完成的邀请。"
  (loki-assistant-devices--stop-timer)
  (when (equal (plist-get loki-assistant-devices--panel :status) "pending")
    (loki-assistant-devices--cancel (plist-get loki-assistant-devices--panel :id)))
  (setq loki-assistant-devices--pair-generation nil
        loki-assistant-devices--expanded nil
        loki-assistant-devices--panel nil
        loki-assistant-devices--panel-error nil
        loki-assistant-devices--polling nil)
  (when (eq loki-assistant-devices--busy 'creating)
    (setq loki-assistant-devices--busy nil)))

(defun loki-assistant-devices--cleanup ()
  "关闭或卸载界面时清理计时器、邀请及过期回调。"
  (loki-assistant-devices--collapse)
  (setq loki-assistant-devices--generation nil))

(defun loki-assistant-devices--button (label action &optional disabled)
  "插入执行 ACTION 的 LABEL 按钮；DISABLED 时仅显示文字。"
  (if disabled
      (insert (propertize (concat "[" label "]") 'face 'shadow))
    (insert-text-button label 'action (lambda (_button) (funcall action))
                        'follow-link t 'face 'link)))

(defun loki-assistant-devices--time (seconds)
  "将 Unix SECONDS 转为本地时间。"
  (if (and (integerp seconds) (> seconds 0))
      (format-time-string "%Y-%m-%d %H:%M:%S" (seconds-to-time seconds))
    "未知"))

(defun loki-assistant-devices--render ()
  "绘制设备列表及按需展开的新增区域，不显示原始令牌。"
  (let ((inhibit-read-only t) (position (point)) panel-start)
    (erase-buffer)
    (insert (propertize "已绑定设备\n\n" 'face 'custom-group-tag))
    (cond
     ((and loki-assistant-devices--loading (zerop (length loki-assistant-devices--peers)))
      (insert "正在读取设备…\n"))
     ((zerop (length loki-assistant-devices--peers)) (insert "暂无绑定设备\n"))
     (t
      (mapc
       (lambda (peer)
         (let ((id (alist-get 'endpoint_id peer)) (name (alist-get 'name peer)))
           (insert (propertize name 'face 'bold) "\n")
           (insert "绑定时间：" (loki-assistant-devices--time (alist-get 'bound_at peer)) "\n")
           (insert "设备身份：" id "\n")
           (loki-assistant-devices--button
            "解除绑定" (lambda () (loki-assistant-devices-remove id name))
            loki-assistant-devices--busy)
           (insert "\n\n")))
       loki-assistant-devices--peers)))
    (when loki-assistant-devices--error
      (insert (propertize (concat loki-assistant-devices--error "\n") 'face 'error)))
    (insert "\n")
    (loki-assistant-devices--button "新增设备" #'loki-assistant-devices-add
                                   loki-assistant-devices--busy)
    (when loki-assistant-devices--expanded
      (setq panel-start (point))
      (insert "\n\n" (propertize "新增设备\n\n" 'face 'custom-group-tag))
      (cond
       ((eq loki-assistant-devices--busy 'creating) (insert "正在创建绑定二维码…\n"))
       (loki-assistant-devices--panel-error
        (insert (propertize (concat loki-assistant-devices--panel-error "\n") 'face 'error)))
       (t
        (let* ((panel loki-assistant-devices--panel)
               (status (plist-get panel :status))
               (image (plist-get panel :image)))
          (when image (insert-image image "[设备绑定二维码]") (insert "\n\n"))
          (insert (pcase status
                    ("pending" "请用手机扫描二维码，完成后会自动更新设备列表。\n")
                    ("completed" (concat "已绑定：" (or (plist-get panel :name) "手机") "\n"))
                    ("expired" "二维码已过期，请重新生成。\n")
                    (_ "本次绑定已取消。\n")))
          (insert "目标电脑：" (system-name) "\n")
          (insert "电脑身份：" (or (plist-get panel :endpoint) "未知") "\n")
          (insert "有效期至：" (loki-assistant-devices--time (plist-get panel :expires)) "\n"))))
      (insert "\n")
      (unless loki-assistant-devices--busy
        (loki-assistant-devices--button "重新生成" #'loki-assistant-devices-add)
        (insert "  "))
      (loki-assistant-devices--button
       "收起" (lambda () (loki-assistant-devices--collapse) (loki-assistant-devices--render))))
    (insert "\n")
    (goto-char (min position (point-max)))
    (set-buffer-modified-p nil)
    panel-start))

(defun loki-assistant-devices--valid-peer-p (peer)
  "检查设备 PEER 是否可展示并用于解除绑定。"
  (and (listp peer) (loki-assistant-devices--endpoint-p (alist-get 'endpoint_id peer))
       (stringp (alist-get 'name peer)) (not (string-empty-p (alist-get 'name peer)))
       (not (string-match-p "[[:cntrl:]]" (alist-get 'name peer)))
       (integerp (alist-get 'bound_at peer)) (> (alist-get 'bound_at peer) 0)))

(defun loki-assistant-devices-refresh ()
  "刷新当前已绑定设备，不创建或替换邀请。"
  (interactive)
  (unless (derived-mode-p 'loki-assistant-devices-mode)
    (user-error "请先打开设备管理界面"))
  (let ((buffer (current-buffer)) (generation loki-assistant-devices--generation)
        (request (list nil)))
    (setq loki-assistant-devices--list-generation request
          loki-assistant-devices--loading t loki-assistant-devices--error nil)
    (loki-assistant-devices--render)
    (loki-assistant-request
     "GET" "/network/peers"
     :callback
     (lambda (response)
       (loki-assistant-devices--current
        buffer generation
        (lambda ()
          (when (eq request loki-assistant-devices--list-generation)
            (setq loki-assistant-devices--loading nil)
            (let* ((data (let ((raw (plist-get response :data))) (and (listp raw) raw))) (peers (alist-get 'peers data)))
              (if (and (plist-get response :ok) (equal (alist-get 'api_version data) "1")
                       (vectorp peers) (cl-every #'loki-assistant-devices--valid-peer-p peers))
                  (setq loki-assistant-devices--peers peers loki-assistant-devices--error nil)
                (setq loki-assistant-devices--error
                      (or (plist-get response :error-message) "services 返回了无效的设备列表"))))
            (loki-assistant-devices--render))))))))

(defun loki-assistant-devices--image (encoded)
  "解码 services 返回的 ENCODED PNG，不在客户端生成二维码。"
  (unless (and (stringp encoded) (< (length encoded) 2000000))
    (error "services 返回了无效的二维码图片"))
  (unless (image-type-available-p 'png)
    (error "当前 Emacs 不支持显示 PNG，请使用图形界面"))
  (let ((data (base64-decode-string encoded)))
    (unless (string-prefix-p (unibyte-string 137 80 78 71 13 10 26 10) data)
      (error "services 返回的二维码不是 PNG 图片"))
    (create-image data 'png t :max-width 300 :max-height 300)))

(defun loki-assistant-devices-add ()
  "在当前设备管理 buffer 中展开新增设备二维码和有效期。"
  (interactive)
  (unless (derived-mode-p 'loki-assistant-devices-mode)
    (user-error "请先打开设备管理界面"))
  (when loki-assistant-devices--busy (user-error "设备操作正在执行，请稍候"))
  (loki-assistant-devices--collapse)
  (let ((buffer (current-buffer)) (generation loki-assistant-devices--generation)
        (request (list nil)))
    (setq loki-assistant-devices--pair-generation request
          loki-assistant-devices--expanded t loki-assistant-devices--busy 'creating)
    (goto-char (loki-assistant-devices--render))
    (loki-assistant-request
     "POST" "/network/pairings"
     :callback
     (lambda (response)
       (let* ((data (let ((raw (plist-get response :data))) (and (listp raw) raw))) (id (alist-get 'pairing_id data))
              (payload (let ((raw (alist-get 'qr_payload data))) (and (listp raw) raw))) accepted)
         (loki-assistant-devices--current
          buffer generation
          (lambda ()
            (when (eq request loki-assistant-devices--pair-generation)
              (setq accepted t loki-assistant-devices--busy nil)
              (condition-case err
                  (progn
                    (unless (plist-get response :ok)
                      (error "%s" (or (plist-get response :error-message) "无法创建绑定邀请")))
                    (unless (and (equal (alist-get 'api_version data) "1")
                                 (equal (alist-get 'status data) "pending")
                                 (loki-assistant-devices--valid-id-p id)
                                 (equal id (alist-get 'pairing_id payload))
                                 (loki-assistant-devices--endpoint-p (alist-get 'endpoint_id payload))
                                 (integerp (alist-get 'expires_at payload)))
                      (error "services 返回了无效的绑定邀请"))
                    (setq loki-assistant-devices--panel
                          (list :id id :status "pending" :expires (alist-get 'expires_at payload)
                                :endpoint (alist-get 'endpoint_id payload)
                                :image (loki-assistant-devices--image (alist-get 'qr_code_png_base64 data))))
                    (setq loki-assistant-devices--timer
                          (run-at-time 2 2 #'loki-assistant-devices--poll buffer generation request)))
                (error
                 (loki-assistant-devices--cancel id)
                 (setq loki-assistant-devices--panel-error (error-message-string err))))
              (loki-assistant-devices--render))))
         ;; 收起或关闭期间才返回的邀请也须撤销，不能复活已经关闭的页面。
         (unless accepted (loki-assistant-devices--cancel id)))))))

(defun loki-assistant-devices--poll (buffer generation request)
  "BUFFER 可见且 REQUEST 仍有效时查询一次绑定进度。"
  (loki-assistant-devices--current
   buffer generation
   (lambda ()
     (when (and (eq request loki-assistant-devices--pair-generation)
                (equal (plist-get loki-assistant-devices--panel :status) "pending")
                (get-buffer-window buffer t) (not loki-assistant-devices--polling))
       (setq loki-assistant-devices--polling t)
       (let ((id (plist-get loki-assistant-devices--panel :id)))
         (loki-assistant-request
          "GET" (concat "/network/pairings/" id)
          :callback
          (lambda (response)
            (loki-assistant-devices--current
             buffer generation
             (lambda ()
               (when (eq request loki-assistant-devices--pair-generation)
                 (setq loki-assistant-devices--polling nil)
                 (let* ((data (let ((raw (plist-get response :data))) (and (listp raw) raw))) (status (alist-get 'status data))
                        (peer (alist-get 'peer data)))
                   (if (and (plist-get response :ok) (equal (alist-get 'api_version data) "1")
                            (equal (alist-get 'pairing_id data) id)
                            (member status '("pending" "completed" "expired" "cancelled"))
                            (or (not (equal status "completed")) (loki-assistant-devices--valid-peer-p peer)))
                       (progn
                         (setq loki-assistant-devices--panel
                               (plist-put loki-assistant-devices--panel :status status))
                         (unless (equal status "pending")
                           (loki-assistant-devices--stop-timer)
                           (setq loki-assistant-devices--panel
                                 (plist-put loki-assistant-devices--panel :image nil)))
                         (when (equal status "completed")
                           (setq loki-assistant-devices--panel
                                 (plist-put loki-assistant-devices--panel :name (alist-get 'name peer)))
                           (loki-assistant-devices-refresh)))
                     (loki-assistant-devices--stop-timer)
                     (setq loki-assistant-devices--panel-error
                           (or (plist-get response :error-message) "无法读取绑定进度")
                           loki-assistant-devices--panel
                           (plist-put loki-assistant-devices--panel :image nil)))
                   (loki-assistant-devices--render))))))))))))

(defun loki-assistant-devices-remove (id name)
  "确认后解除 ID 对应的 NAME 设备绑定。"
  (when loki-assistant-devices--busy (user-error "设备操作正在执行，请稍候"))
  (unless (loki-assistant-devices--endpoint-p id) (user-error "设备身份无效"))
  (when (yes-or-no-p (format "解除与“%s”的绑定？" name))
    (let ((buffer (current-buffer)) (generation loki-assistant-devices--generation))
      (setq loki-assistant-devices--busy 'removing loki-assistant-devices--error nil)
      (loki-assistant-devices--render)
      (loki-assistant-request
       "DELETE" (concat "/network/peers/" id)
       :callback
       (lambda (response)
         (loki-assistant-devices--current
          buffer generation
          (lambda ()
            (setq loki-assistant-devices--busy nil)
            (if (plist-get response :ok)
                (loki-assistant-devices-refresh)
              (setq loki-assistant-devices--error
                    (or (plist-get response :error-message) "解除绑定失败"))
              (loki-assistant-devices--render)))))))))

(defun loki-assistant-devices-quit ()
  "关闭设备管理 buffer 并取消当前未完成邀请。"
  (interactive)
  (unless (derived-mode-p 'loki-assistant-devices-mode)
    (user-error "当前 buffer 不是设备管理界面"))
  (quit-window t))

;;;###autoload
(defun loki-assistant-devices ()
  "打开设备管理，默认仅展示已绑定设备和新增入口。"
  (interactive)
  (let ((buffer (get-buffer-create "*Loki Assistant Devices*")))
    (pop-to-buffer buffer)
    (unless (and (derived-mode-p 'loki-assistant-devices-mode)
                 loki-assistant-devices--generation)
      (loki-assistant-devices-mode))
    (loki-assistant-devices-refresh)
    buffer))

(defun loki-assistant-devices-unload-function ()
  "卸载前清理所有设备管理 buffer 的邀请和计时器。"
  (dolist (buffer (buffer-list))
    (with-current-buffer buffer
      (when (derived-mode-p 'loki-assistant-devices-mode)
        (loki-assistant-devices--cleanup))))
  nil)

(provide 'loki-assistant-devices)
;;; loki-assistant-devices.el ends here
