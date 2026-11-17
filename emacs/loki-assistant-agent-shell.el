;;; loki-assistant-agent-shell.el --- agent-shell 的 macOS 扩展  -*- lexical-binding: t; -*-

;; Copyright (C) 2026 realazy

;; Author: Loki Assistant contributors
;; Based-on: https://github.com/cxa/agent-shell-macext
;; Version: 0.1.0
;; Package-Requires: ((emacs "29.1") (agent-shell "0.48.1"))

;; This package is free software; you can redistribute it and/or modify
;; it under the terms of the GNU General Public License as published by
;; the Free Software Foundation; either version 3, or (at your option)
;; any later version.

;; This package is distributed in the hope that it will be useful,
;; but WITHOUT ANY WARRANTY; without even the implied warranty of
;; MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
;; GNU General Public License for more details.

;; You should have received a copy of the GNU General Public License
;; along with GNU Emacs.  If not, see <https://www.gnu.org/licenses/>.

;;; Commentary:
;;
;; `loki-assistant-agent-shell' 为 `agent-shell' 提供 macOS 文件输入、文件访问
;; 适配和 Loki Assistant 通知集成，由 `loki-assistant' 统一加载。
;;

;;; Code:

(require 'agent-shell)
(require 'image)

(declare-function agent-shell--get-files-context "agent-shell")
(declare-function agent-shell-insert "agent-shell")
(declare-function agent-shell--shell-buffer "agent-shell")
(declare-function agent-shell--dot-subdir "agent-shell")
(declare-function agent-shell-yank-dwim "agent-shell")
(declare-function agent-shell-cwd "agent-shell-project")
(declare-function agent-shell-subscribe-to "agent-shell")
(declare-function agent-shell-viewport--buffer "agent-shell-viewport")
(declare-function loki-assistant-notify "loki-assistant")

(defgroup loki-assistant-agent-shell nil
  "`agent-shell' 的 Loki Assistant macOS 扩展。"
  :group 'agent-shell
  :prefix "loki-assistant-agent-shell-")

;;; Yank

(defcustom loki-assistant-agent-shell-file-copy-policy 'auto
  "控制文件交给 Agent 前是否复制到扩展数据目录。

目录由 `agent-shell--dot-subdir' 解析，因此遵循
`agent-shell-dot-subdir-function'。默认位置是当前项目下的
.agent-shell/.loki-assistant/。

`auto'           仅复制当前项目外的文件；项目内文件直接使用。

`always-copy'    无论文件位置都复制到扩展数据目录。

`always-original' 从不复制，始终将原路径交给 Agent。

系统临时目录中的文件和截图始终复制，以避免文件在 Agent 使用前被系统删除。
截图使用“截图 - 时间戳”的文件名，重名时附加数字序号。"
  :type '(choice (const :tag "自动（仅复制项目外文件）" auto)
                 (const :tag "始终复制" always-copy)
                 (const :tag "始终使用原路径" always-original))
  :group 'loki-assistant-agent-shell)

(defun loki-assistant-agent-shell--clipboard-file-paths ()
  "读取 macOS 剪贴板中的文件路径（例如从 Finder 复制），没有则返回 nil。"
  (when (eq (window-system) 'ns)
    (condition-case nil
        (when-let ((output (with-temp-buffer
                             (when (zerop
                                    (call-process
                                     "osascript" nil t nil
                                     "-l" "JavaScript"
                                     "-e" "ObjC.import('AppKit'); \
var pb = $.NSPasteboard.generalPasteboard; \
var names = pb.propertyListForType('NSFilenamesPboardType'); \
if (names.isNil()) { '' } else { \
  var r = ''; \
  for (var i = 0; i < names.count; i++) { \
    r += ObjC.unwrap(names.objectAtIndex(i)) + '\\n'; \
  } r; }"))
                               (buffer-string)))))
          (seq-filter #'file-exists-p
                      (mapcar #'string-trim
                              (split-string output "\n" t))))
      (error nil))))

(defun loki-assistant-agent-shell--data-dir ()
  "返回扩展数据目录；目录不存在时创建，并遵循 `agent-shell-dot-subdir-function'。"
  (agent-shell--dot-subdir ".loki-assistant"))

(defun loki-assistant-agent-shell--available-cache-path (filename)
  "返回扩展数据目录中 FILENAME 的可用路径，重名时附加数字序号。"
  (let* ((dest-dir (loki-assistant-agent-shell--data-dir))
         (dest (expand-file-name filename dest-dir))
         (number 0))
    (while (or (file-exists-p dest) (file-symlink-p dest))
      (setq number (1+ number)
            dest (expand-file-name
                  (format "%s-%d%s" (file-name-sans-extension filename)
                          number (or (file-name-extension filename t) ""))
                  dest-dir)))
    dest))

(defun loki-assistant-agent-shell--copy-to-data-dir (file-path &optional filename)
  "将 FILE-PATH 复制到扩展数据目录并返回新路径，重名时附加数字序号。
FILENAME 非 nil 时作为目标文件名。"
  (let ((dest (loki-assistant-agent-shell--available-cache-path
               (or filename (file-name-nondirectory file-path)))))
    (copy-file file-path dest nil)
    dest))

(defun loki-assistant-agent-shell--outside-project-p (file-path)
  "如果 FILE-PATH 位于当前项目外则返回非 nil。"
  (let ((project-dir (file-truename (agent-shell-cwd)))
        (file-truename (file-truename file-path)))
    (not (string-prefix-p (file-name-as-directory project-dir) file-truename))))

(defun loki-assistant-agent-shell--permission-allow-all-p ()
  "如果 `agent-shell' 配置为自动允许所有权限则返回非 nil。"
  (eq agent-shell-permission-responder-function
      #'agent-shell-permission-allow-always))

(defun loki-assistant-agent-shell--temp-path-p (file-path)
  "如果 FILE-PATH 位于系统临时目录则返回非 nil。"
  (let ((real (file-truename file-path)))
    (or (string-prefix-p "/var/folders/" real)
        (string-prefix-p "/private/var/folders/" real)
        (string-prefix-p "/tmp/" real)
        (string-prefix-p "/private/tmp/" real))))

(defun loki-assistant-agent-shell--screenshot-file-p (file-path)
  "如果 FILE-PATH 是 macOS 截图则返回非 nil。
识别截图浮动缩略图的临时目录，以及常见中英文截图文件名。"
  (let ((case-fold-search t))
    (and (image-supported-file-p file-path)
         (or (string-match-p "/NSIRD_screencaptureui_[^/]+/" file-path)
             (string-match-p
              "\\`\\(?:截图\\|截屏\\|屏幕快照\\|螢幕截圖\\|Screen ?Shot\\)"
              (file-name-nondirectory file-path))))))

(defun loki-assistant-agent-shell--resolve-file-path (file-path)
  "根据复制策略返回 FILE-PATH 的使用路径；截图始终以时间戳命名后缓存。"
  (cond
   ((loki-assistant-agent-shell--screenshot-file-p file-path)
    (loki-assistant-agent-shell--copy-to-data-dir
     file-path (concat (format-time-string "截图 - %Y-%m-%d %H-%M-%S")
                       (file-name-extension file-path t))))
   ((loki-assistant-agent-shell--temp-path-p file-path)
    (loki-assistant-agent-shell--copy-to-data-dir file-path))
   (t
    (pcase loki-assistant-agent-shell-file-copy-policy
      ('always-copy     (loki-assistant-agent-shell--copy-to-data-dir file-path))
      ('always-original file-path)
      ('auto            (if (or (loki-assistant-agent-shell--permission-allow-all-p)
                                (not (loki-assistant-agent-shell--outside-project-p file-path)))
                            file-path
                          (loki-assistant-agent-shell--copy-to-data-dir file-path)))))))

;; 继承 yank 的 `delete-selection' 属性，使粘贴替换活动区域。
(put 'loki-assistant-agent-shell-yank 'delete-selection 'yank)
(defun loki-assistant-agent-shell-yank (&optional arg)
  "在 `agent-shell' 中按 macOS 剪贴板内容选择粘贴方式。

Finder 文件、表示现有文件的文本路径会作为文件上下文插入；图片由
`agent-shell' 显示预览，其他文件显示为可点击链接。其余内容交给
`agent-shell-yank-dwim'。"
  (interactive "*P")
  (let* ((ns-files (loki-assistant-agent-shell--clipboard-file-paths))
         (kill-text (ignore-errors (current-kill 0 t)))
         (text-as-file (when (and kill-text (not ns-files))
                         (let ((trimmed (string-trim kill-text)))
                           (when (file-exists-p trimmed) trimmed)))))
    (cond
     (ns-files
      (loki-assistant-agent-shell--insert-files ns-files))
     (text-as-file
      (loki-assistant-agent-shell--insert-files (list text-as-file)))
     (t
      (agent-shell-yank-dwim arg)))))

(defun loki-assistant-agent-shell--clipboard-image ()
  "返回剪贴板图片的 (扩展名 . 二进制数据)，没有图片时返回 nil。"
  (seq-some
   (lambda (type)
     (let ((data (ignore-errors (gui-get-selection 'CLIPBOARD (car type)))))
       (when (and (stringp data) (not (string-empty-p data))
                  (eq (image-type-from-data data) (cadr type)))
         (cons (caddr type) data))))
   '((image/png png ".png") (image/tiff tiff ".tiff") (image/jpeg jpeg ".jpg")
     (image/gif gif ".gif") (image/webp webp ".webp") (image/svg+xml svg ".svg"))))

;;;###autoload
(defun loki-assistant-agent-shell-yank-image ()
  "将剪贴板图片保存到当前会话的缓存目录并插入预览。
适用于 `agent-shell-mode' 和 viewport，建议绑定到 `C-c y'。
文件采用“截图 - 时间戳”的名称，重名时附加数字序号。
剪贴板没有图片时提示错误，不粘贴文本；不自动发送给 Agent。"
  (interactive)
  (unless (derived-mode-p 'agent-shell-mode
                          'agent-shell-viewport-edit-mode
                          'agent-shell-viewport-view-mode)
    (user-error "请在 agent-shell 会话或 viewport 中粘贴图片"))
  (let* ((image (or (loki-assistant-agent-shell--clipboard-image)
                    (user-error "剪贴板中没有可用的图片")))
         (shell-buffer (agent-shell--shell-buffer))
         (path (with-current-buffer shell-buffer
                 (loki-assistant-agent-shell--available-cache-path
                  (concat (format-time-string "截图 - %Y-%m-%d %H-%M-%S")
                          (car image)))))
         (coding-system-for-write 'no-conversion))
    (write-region (cdr image) nil path nil 'silent nil 'excl)
    (agent-shell-insert
     :text (agent-shell--get-files-context :files (list path))
     :shell-buffer shell-buffer)))

;;; Drag and drop

(defun loki-assistant-agent-shell--insert-files (file-paths)
  "将 FILE-PATHS 按粘贴规则插入当前 `agent-shell' 缓冲区。"
  (let* ((shell-buffer (agent-shell--shell-buffer))
         (resolved (with-current-buffer shell-buffer
                     (mapcar #'loki-assistant-agent-shell--resolve-file-path file-paths))))
    (when resolved
      (agent-shell-insert
       :text (agent-shell--get-files-context :files resolved)
       :shell-buffer shell-buffer))))

(defun loki-assistant-agent-shell--dnd-handler (url _action)
  "处理拖入 `agent-shell' 缓冲区的单个文件 URL。"
  (require 'dnd)
  (when-let ((file (dnd-get-local-file-name url t)))
    (loki-assistant-agent-shell--insert-files (list file))
    'private))

(defun loki-assistant-agent-shell--dnd-multi-handler (urls _action)
  "处理拖入 `agent-shell' 缓冲区的多个文件 URL。"
  (require 'dnd)
  (let ((files (delq nil (mapcar (lambda (url) (dnd-get-local-file-name url t)) urls))))
    (when files
      (loki-assistant-agent-shell--insert-files files)
      'private)))

(put 'loki-assistant-agent-shell--dnd-multi-handler 'dnd-multiple-handler t)

(defun loki-assistant-agent-shell--setup-dnd ()
  "为当前 `agent-shell' 缓冲区设置拖放处理器。"
  (let ((handler (if (>= emacs-major-version 30)
                     #'loki-assistant-agent-shell--dnd-multi-handler
                   #'loki-assistant-agent-shell--dnd-handler)))
    (unless (rassq handler dnd-protocol-alist)
      (setq-local dnd-protocol-alist
                  (append (list (cons "^file:///" handler)
                                (cons "^file:/[^/]" handler)
                                (cons "^file:[^/]" handler))
                          dnd-protocol-alist)))))

;;; Notifications

(defcustom loki-assistant-agent-shell-notifications t
  "非 nil 时，通过 Loki Assistant services 发送 Agent 事件通知。"
  :type 'boolean
  :group 'loki-assistant-agent-shell)

(defcustom loki-assistant-agent-shell-notify-current-buffer nil
  "非 nil 时，即使当前正在查看该缓冲区也发送通知。
Emacs 未聚焦或当前显示其他缓冲区时始终发送通知；该变量可设置为缓冲区局部值。"
  :type 'boolean
  :group 'loki-assistant-agent-shell)

(defun loki-assistant-agent-shell--agent-name (buffer)
  "返回 BUFFER 中 Agent 的可读名称。"
  (buffer-name buffer))

(defun loki-assistant-agent-shell--describe-stop (stop-reason)
  "返回 STOP-REASON 的可读描述。"
  (pcase stop-reason
    ("end_turn"           "已完成")
    ("max_tokens"         "达到 token 上限")
    ("max_turn_requests"  "超过请求次数上限")
    ("refusal"            "Agent 拒绝执行")
    ("cancelled"          "已取消")
    ((pred stringp)       (format "已停止：%s" stop-reason))
    (_                    "已完成")))

(defun loki-assistant-agent-shell--notify (buffer title message)
  "通过 Loki Assistant services 发送 BUFFER、TITLE 和 MESSAGE 通知。"
  (loki-assistant-notify
   (and (buffer-live-p buffer) buffer)
   title
   :body message
   :module-id "emacs"
   :callback
   (lambda (response)
     (unless (plist-get response :ok)
       (message "agent-shell 通知失败 [%s]：%s"
                (plist-get response :error-code)
                (plist-get response :error-message))))))

(defun loki-assistant-agent-shell--current-session-buffer-p (buffer current-buffer)
  "如果 CURRENT-BUFFER 是 BUFFER 或其 viewport 缓冲区则返回非 nil。"
  (or (eq buffer current-buffer)
      (and (fboundp 'agent-shell-viewport--buffer)
           (let ((viewport-buffer
                  (agent-shell-viewport--buffer
                   :shell-buffer buffer
                   :existing-only t)))
             (eq viewport-buffer current-buffer)))))

(defun loki-assistant-agent-shell--should-notify-p (buffer)
  "判断 BUFFER 是否应发送通知。"
  (or (not (frame-focus-state))
      (not (loki-assistant-agent-shell--current-session-buffer-p
            buffer
            (window-buffer (selected-window))))
      (buffer-local-value 'loki-assistant-agent-shell-notify-current-buffer buffer)))

(defun loki-assistant-agent-shell--handle-event (buffer event)
  "处理 BUFFER 的 `agent-shell' EVENT 并发送通知。"
  (when (and loki-assistant-agent-shell-notifications
             (loki-assistant-agent-shell--should-notify-p buffer))
    (let ((data (map-elt event :data))
          (agent (loki-assistant-agent-shell--agent-name buffer)))
      (pcase (map-elt event :event)
        ('permission-request
         (loki-assistant-agent-shell--notify buffer agent "需要授权"))
        ('turn-complete
         (loki-assistant-agent-shell--notify
          buffer agent
          (loki-assistant-agent-shell--describe-stop (map-elt data :stop-reason))))))))

(defun loki-assistant-agent-shell--setup-notifications ()
  "订阅 `agent-shell' 事件并通过 Loki Assistant 发送通知。"
  (let ((buffer (current-buffer)))
    (agent-shell-subscribe-to
     :shell-buffer buffer
     :on-event (lambda (event)
                 (loki-assistant-agent-shell--handle-event buffer event)))))

;;; Setup

;;;###autoload
(defun loki-assistant-agent-shell-setup ()
  "为 `agent-shell' 设置 Loki Assistant 的 macOS 扩展。
由 `loki-assistant' 在 `agent-shell' 加载后加入 mode hook。"
  (unless (eq system-type 'darwin)
    (user-error "loki-assistant-agent-shell 仅支持 macOS"))
  (define-key agent-shell-mode-map [remap yank] #'loki-assistant-agent-shell-yank)
  (define-key agent-shell-viewport-edit-mode-map [remap yank]
              #'loki-assistant-agent-shell-yank)
  (loki-assistant-agent-shell--setup-dnd)
  (loki-assistant-agent-shell--setup-notifications))

(provide 'loki-assistant-agent-shell)

;;; loki-assistant-agent-shell.el ends here
