;;; loki-assistant-read-later.el --- 日记中的稍后阅读 -*- lexical-binding: t; -*-

;;; Code:

(require 'loki-assistant-doc)
(require 'browse-url)
(require 'json)
(require 'org-capture)
(require 'rx)
(require 'tabulated-list)

(declare-function org-roam-dailies-capture-today "org-roam-dailies" (&optional goto keys))
(declare-function org-roam-db-clear-file "org-roam-db" (&optional file))
(defvar org-roam-dailies-capture-templates)
(defvar org-roam-db-location)

(defcustom loki-assistant-read-later-daily-template-key "d"
  "用于确定稍后阅读所在日记文件的 Org-roam 日记模板键。"
  :type 'string
  :group 'loki-assistant-doc)

(defcustom loki-assistant-read-later-search-program nil
  "用于查询稍后阅读的 org-daily 路径。
为空时依次查找 ~/.local/bin、exec-path 及当前仓库的 dist 目录。"
  :type '(choice (const :tag "自动查找" nil) file)
  :group 'loki-assistant-doc)

(defconst loki-assistant-read-later--dist-program
  (expand-file-name "../dist/org-daily"
                    (file-name-directory (or load-file-name buffer-file-name))))

(defconst loki-assistant-read-later--tag "稍后阅读")
(defconst loki-assistant-read-later--read-at "READED")
(defconst loki-assistant-read-later--id-prefix "read-later:")
(defconst loki-assistant-read-later--created-regexp
  (rx string-start (= 4 digit) "-" (= 2 digit) "-" (= 2 digit) "T"
      (= 2 digit) ":" (= 2 digit) ":" (= 2 digit) "." (= 6 digit) "Z" string-end))
(defvar loki-assistant-read-later--pending-index (make-hash-table :test #'equal)
  "已保存但尚待索引的日记文件，按索引路径与文件名分组。")

(defun loki-assistant-read-later--web-url-p (url)
  "判断 URL 是否为绝对 HTTP(S) 网页链接。"
  (and (stringp url)
       (string-match-p "\\`https?://" url)
       (not (string-match-p "[[:space:][:cntrl:]]" url))
       (let ((parsed (ignore-errors (url-generic-parse-url url))))
         (and parsed (not (string-empty-p (or (url-host parsed) "")))))))

(defun loki-assistant-read-later--heading-url (heading)
  "从 HEADING 取第一个绝对 HTTP(S) Org 链接。"
  (let ((position 0) url)
    (while (and (not url) (string-match org-link-bracket-re heading position))
      (setq position (match-end 0))
      (let ((target (org-link-unescape (match-string 1 heading))))
        (when (loki-assistant-read-later--web-url-p target)
          (setq url target))))
    url))

(defun loki-assistant-read-later--org-timestamp ()
  "返回当前本地时间的 Org 非激活时间戳。"
  (let ((system-time-locale "C"))
    (format-time-string (org-time-stamp-format t t))))

(defun loki-assistant-read-later--timestamp-utc (stamp)
  "将 Org 时间戳 STAMP 转换为 API 使用的 UTC 时间。"
  (when-let* ((time (ignore-errors (org-time-string-to-time stamp))))
    (format-time-string "%FT%TZ" time t)))

(defun loki-assistant-read-later--precise-created-p (created)
  "判断 CREATED 是否为稍后阅读捕获使用的 UTC 创建时间。"
  (and (stringp created) (string-match-p loki-assistant-read-later--created-regexp created)))

(defun loki-assistant-read-later--record-id (created org-id)
  "返回基于 CREATED 或旧 ORG-ID 的稍后阅读接口标识。"
  (cond
   ((and (stringp org-id) (not (string-blank-p org-id))) org-id)
   ((loki-assistant-read-later--precise-created-p created)
    (concat loki-assistant-read-later--id-prefix created))))

(defun loki-assistant-read-later--captured-at (created &optional legacy)
  "将 CREATED 转为 API 的创建时间，兼容旧属性 LEGACY。"
  (or (and (loki-assistant-read-later--precise-created-p created)
           (concat (substring created 0 19) "Z"))
      (loki-assistant-read-later--timestamp-utc created)
      legacy
      :null))

(defun loki-assistant-read-later--daily-root ()
  "返回日记目录的绝对路径。"
  (unless (ignore-errors (require 'org-roam-dailies nil t))
    (loki-assistant-doc--fail "doc-unavailable"))
  (expand-file-name org-roam-dailies-directory org-roam-directory))

(defun loki-assistant-read-later--require-roam ()
  "加载日记能力并补齐当前知识库的待更新索引。"
  (loki-assistant-read-later--daily-root)
  (maphash (lambda (key _)
             (when (equal (car key) org-roam-db-location)
               (loki-assistant-read-later--index (cdr key))))
           loki-assistant-read-later--pending-index))

(defun loki-assistant-read-later--index (file)
  "更新 FILE 的 org-roam 索引，失败时保留重试记录。"
  (let ((key (cons org-roam-db-location file)))
    (puthash key t loki-assistant-read-later--pending-index)
    (condition-case nil
        (let ((org-roam-db-update-on-save nil)
              (enable-local-variables nil)
              (enable-local-eval nil))
          (if (file-regular-p file)
              (progn
                ;; org-roam 更新已访问文件时可能调用 save-buffer。
                (loki-assistant-read-later--check-buffer (org-find-base-buffer-visiting file))
                (org-roam-db-update-file file))
            (org-roam-db-clear-file file))
          (remhash key loki-assistant-read-later--pending-index))
      (error (loki-assistant-doc--fail "read-later-index-failed")))))

(defconst loki-assistant-read-later--unread-query
  (list "--where" (concat "(prop:ID OR prop:CREATED) AND NOT prop:"
                         loki-assistant-read-later--read-at)))

(defun loki-assistant-read-later--search-command (filters)
  "返回查询 FILTERS 的命令，日记目录尚未创建时返回 nil。"
  (let ((root (loki-assistant-read-later--daily-root))
        (installed (expand-file-name "~/.local/bin/org-daily")))
    (when (file-exists-p root)
      (append
       (list (or loki-assistant-read-later-search-program
                 (and (file-executable-p installed) installed)
                 (executable-find "org-daily")
                 loki-assistant-read-later--dist-program)
             "--dir" root "--include-undated" "--format" "jsonl"
             "--local-tag" loki-assistant-read-later--tag)
       filters))))

(defun loki-assistant-read-later--parse-row (line)
  "将 org-daily 的一行 LINE 解析为日记标题。"
  (json-parse-string line :object-type 'alist :null-object nil))

(defun loki-assistant-read-later--search (&rest filters)
  "同步查询 FILTERS 并返回日记标题。"
  (when-let* ((command (loki-assistant-read-later--search-command filters)))
    (condition-case nil
        (let ((coding-system-for-read 'utf-8-unix)
              (coding-system-for-write 'utf-8-unix))
          (with-temp-buffer
            (pcase (apply #'process-file (car command) nil '(t nil) nil (cdr command))
              (0 (mapcar #'loki-assistant-read-later--parse-row
                         (split-string (buffer-string) "\n" t)))
              (1 nil)
              (_ (loki-assistant-doc--fail "document-read-failed")))))
      (error (loki-assistant-doc--fail "document-read-failed")))))

(defun loki-assistant-read-later--entry (row)
  "将 ROW 转成稍后阅读摘要，兼容有 Org ID 的旧记录。"
  (let* ((properties (alist-get 'properties row))
         (created (alist-get 'CREATED properties))
         (id (loki-assistant-read-later--record-id created (alist-get 'ID properties)))
         (title (alist-get 'title row)))
    (unless (stringp title)
      (error "无效的 org-daily 记录"))
    (when id
      `((id . ,id)
        (title . ,(org-link-display-format title))
        (url . ,(or (loki-assistant-read-later--heading-url title) :null))
        (captured_at .
         ,(loki-assistant-read-later--captured-at
           created
           (alist-get 'LOKI_READ_LATER_CAPTURED_AT properties)))))))

(defun loki-assistant-read-later--list-entries ()
  "从磁盘查询未读标题，并按捕获时间倒序排列。"
  (condition-case nil
      (sort (delq nil (mapcar #'loki-assistant-read-later--entry
                             (apply #'loki-assistant-read-later--search
                                    loki-assistant-read-later--unread-query)))
            (lambda (first second)
              (let ((first-time (alist-get 'captured_at first))
                    (second-time (alist-get 'captured_at second)))
                (string> (if (stringp first-time) first-time "")
                         (if (stringp second-time) second-time "")))))
    (error (loki-assistant-doc--fail "document-read-failed"))))

(defun loki-assistant-read-later-stream (on-entries on-done)
  "异步查询未读标题并返回可取消的进程。
每读完一批记录，向 ON-ENTRIES 传入进程和摘要列表。
结束时向 ON-DONE 传入进程和错误码，成功时错误码为 nil。
无需启动进程时同步调用 ON-DONE，进程参数为 nil。"
  (let* ((reply (catch 'loki-assistant-doc-error
                  (list 'command (loki-assistant-read-later--search-command
                                  loki-assistant-read-later--unread-query))))
         (command (cadr reply)))
    (if (not command)
        (progn (funcall on-done nil (alist-get 'error reply)) nil)
      (let ((pending "")
            (stderr (generate-new-buffer " *稍后阅读错误*")))
        (condition-case nil
            (make-process
             :name "loki-read-later" :command command :connection-type 'pipe
             :coding 'utf-8-unix :noquery t :stderr stderr
             :filter
             (lambda (process chunk)
               (let* ((lines (split-string (concat pending chunk) "\n"))
                      (entries
                       (condition-case nil
                           (delq nil (mapcar
                                      (lambda (line)
                                        (loki-assistant-read-later--entry
                                         (loki-assistant-read-later--parse-row line)))
                                      (butlast lines)))
                         (error
                          (process-put process 'loki-assistant-read-later-invalid-output t)
                          (delete-process process)
                          nil))))
                 (setq pending (car (last lines)))
                 (when entries (funcall on-entries process entries))))
             :sentinel
             (lambda (process _event)
               (when (memq (process-status process) '(exit signal failed closed))
                 (kill-buffer stderr)
                 (funcall on-done process
                          (unless (and (not (process-get process 'loki-assistant-read-later-invalid-output))
                                       (equal pending "")
                                       (eq (process-status process) 'exit)
                                       (memq (process-exit-status process) '(0 1)))
                            "document-read-failed")))))
          (error
           (kill-buffer stderr)
           (funcall on-done nil "document-read-failed")
           nil))))))

(defun loki-assistant-read-later--check-buffer (buffer)
  "确认 BUFFER 没有尚未保存的编辑或未加载的磁盘修改。"
  (when (and buffer
             (or (buffer-modified-p buffer)
                 (not (verify-visited-file-modtime buffer))))
    (loki-assistant-doc--fail "read-later-daily-conflict")))

(defun loki-assistant-read-later--new-created ()
  "生成尚未用于稍后阅读记录的 UTC 创建时间。"
  (let* ((instant (current-time))
         (created (format-time-string "%FT%T.%6NZ" instant t)))
    (while (loki-assistant-read-later--search "--property" (concat "CREATED=" created))
      (setq instant (time-add instant '(1 . 1000000))
            created (format-time-string "%FT%T.%6NZ" instant t)))
    created))

(defun loki-assistant-read-later--capture (title url)
  "通过日记捕获保存 TITLE 和 URL，返回新记录的摘要。"
  (let* ((template (assoc loki-assistant-read-later-daily-template-key
                          org-roam-dailies-capture-templates))
         (target (plist-get (nthcdr 4 template) :target))
         (root (file-name-as-directory (file-truename (loki-assistant-read-later--daily-root))))
         (created (loki-assistant-read-later--new-created))
         (id (loki-assistant-read-later--record-id created nil))
         (get-buffer (symbol-function 'org-capture-target-buffer))
         (org-capture-entry nil)
         (org-capture-plist nil)
         (org-capture-after-finalize-hook org-capture-after-finalize-hook)
         (org-capture-link-is-already-stored t)
         (org-store-link-plist nil)
         (org-roam-db-update-on-save nil)
         (enable-local-variables nil)
         (enable-local-eval nil)
         file buffer visiting capture-buffer change-group prepared saved)
    (unless (memq (car-safe target) '(file file+head file+olp file+head+olp file+datetree))
      (loki-assistant-doc--fail "read-later-template-unavailable"))
    (let ((org-roam-dailies-capture-templates
           `(("r" "稍后阅读" entry ,(concat "* 网页 :" loki-assistant-read-later--tag ":")
              :target ,(copy-tree target) :empty-lines-before 1 :immediate-finish t :no-save t
              :prepare-finalize
              ,(lambda ()
                 (setq capture-buffer (current-buffer))
                 (save-excursion
                   (goto-char (point-min))
                   (org-back-to-heading t)
                   ;; 在模板展开之后写入网页数据。
                   (org-edit-headline (org-link-make-string url title))
                   (org-entry-put nil "CREATED" created)
                   (setq prepared t)))))))
      (save-window-excursion
        (unwind-protect
            (progn
              (cl-letf (((symbol-function 'org-capture-target-buffer)
                         (lambda (path)
                           (unless (string-prefix-p root (file-truename path))
                             (loki-assistant-doc--fail "read-later-template-unavailable"))
                           (setq visiting (org-find-base-buffer-visiting path))
                           (loki-assistant-read-later--check-buffer visiting)
                           (make-directory (file-name-directory path) t)
                           (setq file path
                                 buffer (funcall get-buffer path)
                                 change-group (prepare-change-group buffer))
                           (activate-change-group change-group)
                           buffer)))
                (org-roam-dailies-capture-today nil "r"))
              (with-current-buffer buffer
                (unless prepared
                  (loki-assistant-doc--fail "read-later-save-failed"))
                (save-buffer))
              (accept-change-group change-group)
              (setq saved t))
          (unless saved
            (when (buffer-live-p capture-buffer) (kill-buffer capture-buffer))
            (when change-group (cancel-change-group change-group))
            (when (and (buffer-live-p buffer) (not visiting)) (kill-buffer buffer))))))
    (unless (and file (file-regular-p file))
      (loki-assistant-doc--fail "read-later-save-failed"))
    (loki-assistant-read-later--index file)
    `((id . ,id) (title . ,title) (url . ,url)
      (captured_at . ,(loki-assistant-read-later--captured-at created)))))

(defun loki-assistant-read-later-capture (info)
  "将 INFO 中的网页链接加入当天日记，重复提交时复用未读节点。"
  (catch 'loki-assistant-doc-error
    (let ((title (plist-get info :title))
          (url (plist-get info :url)))
      (unless (and (stringp title) (not (string-blank-p title))
                   (not (string-match-p "[[:cntrl:]]" title))
                   (loki-assistant-read-later--web-url-p url))
        (loki-assistant-doc--fail "invalid-read-later-link"))
      (loki-assistant-read-later--require-roam)
      (let* ((entries (loki-assistant-read-later--list-entries))
             (key (loki-assistant-doc--url-key url))
             (existing (seq-find (lambda (entry)
                                   (let ((entry-url (alist-get 'url entry)))
                                     (and (stringp entry-url)
                                          (equal key (loki-assistant-doc--url-key entry-url)))))
                                 entries))
             (entry (or existing
                        (condition-case nil
                            (loki-assistant-read-later--capture (string-trim title) url)
                          (error (loki-assistant-doc--fail "read-later-save-failed"))))))
        `((result (entry . ,entry)))))))

(defun loki-assistant-read-later-list ()
  "查询本地日记中的未读记录。"
  (catch 'loki-assistant-doc-error
    `((result (entries . ,(vconcat (loki-assistant-read-later--list-entries)))))))

(defun loki-assistant-read-later--entry-file (id)
  "返回 ID 对应的稍后阅读日记文件。"
  (unless (and (stringp id) (not (string-blank-p id))
               (not (string-match-p "[[:cntrl:]]" id)))
    (loki-assistant-doc--fail "invalid-request"))
  (let* ((created (and (string-prefix-p loki-assistant-read-later--id-prefix id)
                       (string-remove-prefix loki-assistant-read-later--id-prefix id)))
         (rows (if (loki-assistant-read-later--precise-created-p created)
                   (loki-assistant-read-later--search
                    "--where" (concat "prop:ID=" (json-serialize id)
                                      " OR prop:CREATED=" (json-serialize created)))
                 (loki-assistant-read-later--search "--property" (concat "ID=" id))))
         (matches (seq-filter
                   (lambda (row)
                     (let ((properties (alist-get 'properties row)))
                       (equal id (loki-assistant-read-later--record-id
                                  (alist-get 'CREATED properties) (alist-get 'ID properties)))))
                   rows))
         (file (alist-get 'file (car matches))))
    (unless (and (null (cdr matches)) (stringp file) (file-regular-p file)
                 (file-in-directory-p file (loki-assistant-read-later--daily-root)))
      (loki-assistant-doc--fail "read-later-not-found"))
    file))

(defun loki-assistant-read-later--goto-entry (id)
  "定位当前日记中 ID 对应的稍后阅读标题。"
  (let (positions)
    (org-map-entries
     (lambda ()
       (when (and (member loki-assistant-read-later--tag (org-get-tags nil t))
                  (equal id (loki-assistant-read-later--record-id
                             (org-entry-get nil "CREATED") (org-entry-get nil "ID"))))
         (push (point) positions))))
    (unless (and positions (null (cdr positions)))
      (loki-assistant-doc--fail "read-later-not-found"))
    (goto-char (car positions))))

(defun loki-assistant-read-later--update-entry (id update)
  "在 ID 对应的稍后阅读标题上执行 UPDATE，保存并更新索引后返回其结果。"
  (loki-assistant-read-later--require-roam)
  (let ((file (loki-assistant-read-later--entry-file id))
        result)
    (condition-case nil
        (let ((enable-local-variables nil)
              (enable-local-eval nil)
              (org-roam-db-update-on-save nil)
              (visiting (org-find-base-buffer-visiting file)))
          (loki-assistant-read-later--check-buffer visiting)
          (with-current-buffer (or visiting (find-file-noselect file))
            (org-with-wide-buffer
             (loki-assistant-read-later--goto-entry id)
             (atomic-change-group
               (setq result (funcall update))
               (when (buffer-modified-p) (save-buffer))))))
      (error (loki-assistant-doc--fail "read-later-save-failed")))
    (loki-assistant-read-later--index file)
    result))

(defun loki-assistant-read-later-mark-read (id)
  "标记 ID 对应的日记节点为已读，保留首次已读时间。"
  (catch 'loki-assistant-doc-error
    (let ((read-at
           (loki-assistant-read-later--update-entry
            id (lambda ()
                 (or (org-entry-get nil loki-assistant-read-later--read-at)
                     (let ((time (loki-assistant-read-later--org-timestamp)))
                       (org-entry-put nil loki-assistant-read-later--read-at time)
                       time))))))
      `((result (id . ,id)
                (read_at . ,(or (and (string-prefix-p "[" read-at)
                                     (loki-assistant-read-later--timestamp-utc read-at))
                                read-at)))))))

(defun loki-assistant-read-later-delete (id)
  "删除 ID 对应的整个稍后阅读子树，并更新日记索引。"
  (catch 'loki-assistant-doc-error
    (loki-assistant-read-later--update-entry
     id (lambda ()
          ;; Org 会将 inlinetask 的剪切目标提升到父标题。
          (unless (eq (org-element-type (org-element-at-point)) 'headline)
            (loki-assistant-doc--fail "read-later-not-found"))
          (let (kill-ring kill-ring-yank-pointer interprogram-cut-function
                org-subtree-clip org-subtree-clip-folded org-markers-to-move)
            (org-cut-subtree 1))))
    `((result (id . ,id)))))

(defun loki-assistant-read-later--result (reply)
  "返回 REPLY 中的结果，交互操作失败时显示原因。"
  (if-let* ((code (alist-get 'error reply)))
      (user-error "%s"
                  (pcase code
                    ("doc-unavailable" "请先加载 org-roam 及日记模块")
                    ("document-read-failed" "无法查询稍后阅读，请检查 org-daily 的安装及日记目录")
                    ("read-later-not-found" "找不到这条稍后阅读记录，请按 g 刷新")
                    ("read-later-daily-conflict" "日记有未保存或未加载的更改，请处理后重试")
                    ("read-later-save-failed" "无法保存稍后阅读记录，请检查日记文件")
                    ("read-later-index-failed" "无法更新日记索引，请检查 org-roam 后重试")
                    (_ (format "稍后阅读操作失败（%s）" code))))
    (alist-get 'result reply)))

(defvar-local loki-assistant-read-later--list-process nil
  "当前列表使用的查询进程。")

(defvar-local loki-assistant-read-later--list-error nil
  "最近一次列表查询返回的错误码。")

(defun loki-assistant-read-later--cancel-list ()
  "取消当前列表查询并丢弃该进程的后续回调。"
  (when-let* ((process loki-assistant-read-later--list-process))
    (setq loki-assistant-read-later--list-process nil)
    (delete-process process)))

(defun loki-assistant-read-later--load-rows ()
  "启动未读查询，收到完整记录后分批刷新列表。"
  (loki-assistant-read-later--cancel-list)
  (setq tabulated-list-entries nil
        loki-assistant-read-later--list-error nil)
  (let ((buffer (current-buffer)))
    (setq loki-assistant-read-later--list-process
          (loki-assistant-read-later-stream
           (lambda (process entries)
             (when (buffer-live-p buffer)
               (with-current-buffer buffer
                 (when (eq process loki-assistant-read-later--list-process)
                   (dolist (entry entries)
                     (let ((id (alist-get 'id entry))
                           (captured-at (alist-get 'captured_at entry))
                           (url (alist-get 'url entry)))
                       (unless (member id (process-get process 'loki-assistant-read-later-marked))
                         (push (list id (vector (if (stringp captured-at) captured-at "")
                                                (alist-get 'title entry)
                                                (if (stringp url) url "")))
                               tabulated-list-entries))))
                   (tabulated-list-print t)))))
           (lambda (process error-code)
             (when (buffer-live-p buffer)
               (with-current-buffer buffer
                 (when (eq process loki-assistant-read-later--list-process)
                   (setq loki-assistant-read-later--list-process nil
                         loki-assistant-read-later--list-error error-code)
                   (force-mode-line-update)
                   (when error-code
                     (condition-case err
                         (loki-assistant-read-later--result (list (cons 'error error-code)))
                       (user-error (message "%s" (error-message-string err))))))))))))
  (force-mode-line-update))

(defun loki-assistant-read-later--id-at-point ()
  "返回当前行的 Org ID。"
  (or (tabulated-list-get-id) (user-error "请先选择一条未读记录")))

(defun loki-assistant-read-later--entry-location (id)
  "返回 ID 对应的日记缓冲区及标题位置，或文档错误。"
  (catch 'loki-assistant-doc-error
    (let* ((file (loki-assistant-read-later--entry-file id))
           (buffer (find-file-noselect file)))
      (with-current-buffer buffer
        (org-with-wide-buffer
         (loki-assistant-read-later--goto-entry id)
         `((result . (,buffer . ,(point)))))))))

(defun loki-assistant-read-later-open ()
  "打开当前记录对应的 Org 日记条目。"
  (interactive)
  (let* ((id (loki-assistant-read-later--id-at-point))
         (location
          (loki-assistant-read-later--result (loki-assistant-read-later--entry-location id))))
    (pop-to-buffer-same-window (car location))
    (unless (<= (point-min) (cdr location) (point-max)) (widen))
    (goto-char (cdr location))
    (org-fold-show-context)
    (org-fold-show-entry)))

(defun loki-assistant-read-later-open-url ()
  "打开当前记录的网页链接。"
  (interactive)
  (loki-assistant-read-later--id-at-point)
  (let ((url (aref (tabulated-list-get-entry) 2)))
    (unless (loki-assistant-read-later--web-url-p url)
      (user-error "此条目没有网页链接，按 Alt-Enter 可打开日记条目"))
    (browse-url url)))

(defun loki-assistant-read-later-mark-current-read ()
  "标记当前记录为已读并从未读列表移除。"
  (interactive)
  (let ((id (loki-assistant-read-later--id-at-point)))
    (loki-assistant-read-later--result (loki-assistant-read-later-mark-read id))
    (when-let* ((process loki-assistant-read-later--list-process))
      (process-put process 'loki-assistant-read-later-marked
                   (cons id (process-get process 'loki-assistant-read-later-marked))))
    (setq tabulated-list-entries (assoc-delete-all id tabulated-list-entries))
    (tabulated-list-print t)
    (message "已标记为已读")))

(defun loki-assistant-read-later-help ()
  "显示稍后阅读模式的操作说明。"
  (interactive)
  (describe-function 'loki-assistant-read-later-mode))

(defvar loki-assistant-read-later-mode-map
  (let ((map (make-sparse-keymap)))
    (define-key map (kbd "RET") #'loki-assistant-read-later-open-url)
    (define-key map (kbd "<return>") #'loki-assistant-read-later-open-url)
    (define-key map (kbd "A-RET") #'loki-assistant-read-later-open)
    (define-key map (kbd "A-<return>") #'loki-assistant-read-later-open)
    (define-key map (kbd "M-RET") #'loki-assistant-read-later-open)
    (define-key map (kbd "M-<return>") #'loki-assistant-read-later-open)
    (define-key map (kbd "b") #'loki-assistant-read-later-open-url)
    (define-key map (kbd "r") #'loki-assistant-read-later-mark-current-read)
    (define-key map (kbd "C-c C-q") #'quit-window)
    (define-key map (kbd "?") #'describe-mode)
    map)
  "稍后阅读列表的快捷键。")

(define-derived-mode loki-assistant-read-later-mode tabulated-list-mode "稍后阅读"
  "查看 org-roam 日记中的未读记录，默认按加入时间倒序排列。

\\<loki-assistant-read-later-mode-map>\\[next-line] / \\[previous-line]  移到下一条或上一条记录。
RET / b  打开当前记录的网页链接。
Alt-Enter  跳转到当前记录对应的本地 Org 日记条目。
\\[loki-assistant-read-later-mark-current-read]  保存已读时间，并从列表移除当前记录。
\\[revert-buffer]  重新查询未读列表。
C-c C-q  退出列表。
?  查看模式帮助。"
  ;; macOS 上将 Option 与 Control 区分开，作用范围为当前列表缓冲区。
  (when (boundp 'ns-alternate-modifier)
    (setq-local ns-alternate-modifier 'alt))
  (setq tabulated-list-format [("加入时间 (UTC)" 20 t) ("标题" 42 t) ("网页" 0 nil)]
        tabulated-list-padding 2
        tabulated-list-sort-key '("加入时间 (UTC)" . t))
  (setq-local mode-line-process
              (list (loki-assistant--mode-line-keys
                     "g 刷新  RET / b 打开网页  Alt-Enter 跳转到日记  r 标为已读")
                    '(:eval (cond (loki-assistant-read-later--list-process " · 查询中")
                                  (loki-assistant-read-later--list-error " · 查询失败，按 g 重试")
                                  ((null tabulated-list-entries) " · 没有未读记录")))))
  (add-hook 'tabulated-list-revert-hook #'loki-assistant-read-later--load-rows nil t)
  (add-hook 'kill-buffer-hook #'loki-assistant-read-later--cancel-list nil t)
  (add-hook 'change-major-mode-hook #'loki-assistant-read-later--cancel-list nil t)
  (tabulated-list-init-header))

;;;###autoload
(defun loki-assistant-read-later ()
  "打开并刷新稍后阅读未读列表。"
  (interactive)
  (let ((buffer (get-buffer-create "*稍后阅读*")))
    (with-current-buffer buffer
      (unless (derived-mode-p 'loki-assistant-read-later-mode)
        (loki-assistant-read-later-mode))
      (tabulated-list-revert))
    (pop-to-buffer-same-window buffer)))

(provide 'loki-assistant-read-later)
;;; loki-assistant-read-later.el ends here
