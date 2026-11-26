;;; loki-assistant-xbloom.el --- 同步 xBloom 配方库 -*- lexical-binding: t; -*-

(require 'cl-lib)
(require 'json)
(require 'loki-assistant)
(require 'org)
(require 'org-element)
(require 'org-id)
(require 'org-table)
(require 'seq)
(require 'subr-x)

(defvar org-roam-directory)
(defvar org-roam-db-location)
(declare-function org-roam-db-update-file "org-roam-db" (&optional file-path))

(defgroup loki-assistant-xbloom nil
  "xBloom 官方配方同步。"
  :group 'loki-assistant)

(defcustom loki-assistant-xbloom-program "~/.local/bin/xbloom"
  "提供配方数据的 xbloom 可执行文件。"
  :type 'string
  :group 'loki-assistant-xbloom)

(defvar loki-assistant-xbloom--syncing nil)

(defconst loki-assistant-xbloom--switch-labels '(("1" . "开启") ("2" . "关闭")))

(defconst loki-assistant-xbloom--profile-fields
  '(("ORIGIN" "origin" "产地")
    ("PROCESS" "process" "处理法")
    ("FLAVOR_NOTES" "flavor" "风味")))

(defun loki-assistant-xbloom--text (value &optional labels)
  "将 VALUE 转为单行显示文字，可用 LABELS 翻译编号。"
  (let ((text (cond ((numberp value) (number-to-string value))
                    ((stringp value) (string-trim (replace-regexp-in-string "[[:cntrl:]]+" " " value)))
                    (t ""))))
    (or (cdr (assoc text labels)) text)))

(defun loki-assistant-xbloom--row (&rest cells)
  "插入由 CELLS 组成的 Org 表格行。"
  (insert "| " (mapconcat (lambda (value)
                            (string-replace "|" "\\vert{}" (loki-assistant-xbloom--text value)))
                          cells " | ")
          " |\n"))

(defun loki-assistant-xbloom--insert-profile (profile)
  "将非空 PROFILE 字段写为 Org 描述列表。"
  (let ((start (point)))
    (dolist (field loki-assistant-xbloom--profile-fields)
      (when-let* ((value (cdr (assoc (car field) profile)))
                  ((not (string-empty-p value))))
        (insert "- " (nth 2 field) " :: " value "\n")))
    (unless (= (point) start)
      (insert "\n"))))

(defun loki-assistant-xbloom--body-profile (section &optional positions)
  "从配方的 SECTION 读取正文描述列表。
POSITIONS 非空时返回匹配行的起点。"
  (let (profile)
    (when (eq (org-element-type section) 'section)
      (dolist (element (org-element-contents section))
        (when (eq (org-element-type element) 'plain-list)
          (dolist (item (org-element-contents element))
            (save-excursion
              (goto-char (org-element-property :begin item))
              (dolist (field loki-assistant-xbloom--profile-fields)
                (when (and (not (assoc (car field) profile))
                           (looking-at (concat "[ \t]*- " (nth 2 field) " :: \\(.*\\)$")))
                  (push (cons (car field) (if positions (point) (match-string-no-properties 1))) profile))))))))
    (nreverse profile)))

(defun loki-assistant-xbloom--headline ()
  "解析当前配方子树。"
  (save-restriction
    (org-narrow-to-subtree)
    (car (org-element-contents (org-element-parse-buffer 'element)))))

(defun loki-assistant-xbloom-read-profile ()
  "读取当前配方正文，返回以 ORIGIN、PROCESS、FLAVOR_NOTES 为键的 alist。"
  (org-with-wide-buffer
   (org-back-to-heading t)
   (while (org-up-heading-safe))
   (loki-assistant-xbloom--body-profile
    (car (org-element-contents (loki-assistant-xbloom--headline))))))

(defun loki-assistant-xbloom-read-recipe ()
  "从当前库配方读回标题、URL、标签、属性、描述字段和两张 Org 表。"
  (org-with-wide-buffer
   (org-back-to-heading t)
   (while (org-up-heading-safe))
   (loki-assistant-xbloom--read-headline (loki-assistant-xbloom--headline))))

(defun loki-assistant-xbloom--child (headline name)
  "查找 HEADLINE 的直接子标题 NAME。"
  (seq-find (lambda (element)
              (and (eq (org-element-type element) 'headline)
                   (equal name (org-element-property :raw-value element))))
            (org-element-contents headline)))

(defun loki-assistant-xbloom--table (headline)
  "查找 HEADLINE 正文的第一张 Org 表。"
  (org-element-map (car (org-element-contents headline)) 'table #'identity nil t))

(defun loki-assistant-xbloom--read-headline (headline)
  "从解析后的配方 HEADLINE 读取 Org 字段。"
  (let* ((section (car (org-element-contents headline)))
         (heading (org-element-property :raw-value headline))
         (link (string-match (concat (rx string-start) org-link-bracket-re (rx string-end)) heading))
         (title (if link (match-string 2 heading) heading))
         (url (when link (org-link-unescape (match-string 1 heading))))
         (drawer (org-element-map section 'property-drawer #'identity nil t))
         (profile (loki-assistant-xbloom--body-profile section)))
    (list
     (cons 'title title)
     (cons 'url url)
     (cons 'tags (org-element-property :tags headline))
     (cons 'properties
           (sort (org-element-map drawer 'node-property
                   (lambda (entry)
                     (cons (org-element-property :key entry)
                           (org-element-property :value entry))))
                 (lambda (left right) (string< (car left) (car right)))))
     (cons 'profile
           (mapcar (lambda (field) (assoc (car field) profile))
                   loki-assistant-xbloom--profile-fields))
     (cons 'tables
           (mapcar
            (lambda (name)
              (when-let* ((child (loki-assistant-xbloom--child headline name))
                          (table (loki-assistant-xbloom--table child)))
                (save-excursion
                  (goto-char (org-element-property :begin table))
                  (remq 'hline (org-table-to-lisp)))))
            '("冲煮参数" "分段注水"))))))

(defun loki-assistant-xbloom--dose-tag (dose)
  "将 DOSE 转为 Org 粉量标签。"
  (when (string-match (rx string-start (group (+ digit)) (optional "." (group (+ digit))) string-end) dose)
    (let ((integer (match-string 1 dose))
          (fraction (string-trim-right (or (match-string 2 dose) "") "0+")))
      (concat integer (unless (string-empty-p fraction) (concat "_" fraction)) "克"))))

(defun loki-assistant-xbloom--roast-tag (roast name)
  "按 ROAST 等级或 NAME 中明示的烘焙度生成标签。"
  (or (cdr (assoc (loki-assistant-xbloom--text roast)
                  '(("1" . "浅烘") ("2" . "浅烘") ("3" . "中烘") ("4" . "深烘") ("5" . "深烘")
                    ("浅" . "浅烘") ("中" . "中烘") ("深" . "深烘")
                    ("浅烘" . "浅烘") ("中烘" . "中烘") ("深烘" . "深烘"))))
      (when (string-match (rx (group (any "浅中深")) "烘") name)
        (concat (match-string 1 name) "烘"))))

(defun loki-assistant-xbloom--entry (recipe id region source)
  "用 RECIPE、Org ID、REGION 和 SOURCE 生成配方子树。"
  (let* ((name (loki-assistant-xbloom--text (gethash "theName" recipe)))
         (dose (loki-assistant-xbloom--text (gethash "dose" recipe)))
         (ratio (loki-assistant-xbloom--text (gethash "grandWater" recipe)))
         (volume (unless (or (string-empty-p dose) (string-empty-p ratio))
                   (truncate (* (string-to-number dose) (string-to-number ratio)))))
         (pods (gethash "podsVo" recipe))
         (url (loki-assistant-xbloom--text (gethash "shareRecipeLink" recipe)))
         (tag-settings (list org-tags-column org-auto-align-tags
                             org-tags-sort-function org-link-descriptive))
         (switch loki-assistant-xbloom--switch-labels))
    (with-temp-buffer
      (delay-mode-hooks (org-mode))
      (when (string-empty-p name)
        (setq name (format "xBloom 配方 %s" (gethash "tableId" recipe))))
      (insert "* " (if (string-empty-p url) name (org-link-make-string url name)))
      (let ((tags (delq nil (list (loki-assistant-xbloom--dose-tag dose)
                                 (loki-assistant-xbloom--roast-tag
                                  (and (hash-table-p pods) (gethash "roast" pods)) name)))))
        (when tags
          (cl-destructuring-bind (org-tags-column org-auto-align-tags
                                  org-tags-sort-function org-link-descriptive)
              tag-settings
            ;; 先按 Org 的显示规则渲染标题链接，标签会按可见标题对齐。
            (font-lock-ensure (point-min) (point-max))
            (goto-char (point-min))
            (let ((org-after-tags-change-hook nil))
              (org-set-tags tags))
            (goto-char (point-max)))))
      (insert "\n:PROPERTIES:\n:ID: " id
              "\n:TABLE_ID: " (loki-assistant-xbloom--text (gethash "tableId" recipe))
              "\n:SOURCE: " source "\n:REGION: " region "\n")
      (dolist (property
               (list (cons "MODEL" (loki-assistant-xbloom--text
                                    (gethash "adaptedModel" recipe)
                                    '(("1" . "Studio") ("2" . "Original"))))
                     (cons "COFFEE" (gethash "podsName" recipe))))
        (let ((value (loki-assistant-xbloom--text (cdr property))))
          (unless (string-empty-p value)
            (insert ":" (car property) ": " value "\n"))))
      (insert ":END:\n\n")
      (when (hash-table-p pods)
        (loki-assistant-xbloom--insert-profile
         (mapcar (lambda (field)
                   (cons (car field) (loki-assistant-xbloom--text (gethash (cadr field) pods))))
                 loki-assistant-xbloom--profile-fields)))
      (insert "** 冲煮参数\n\n")
      (let ((start (point)))
        (insert "| 参数 | 值 |\n|-\n")
        (loki-assistant-xbloom--row
         "杯型" (loki-assistant-xbloom--text (gethash "cupType" recipe)
                                            '(("1" . "xPod") ("2" . "Omni") ("3" . "Other"))))
        (loki-assistant-xbloom--row "粉量 (g)" dose)
        (loki-assistant-xbloom--row "粉水比" (unless (string-empty-p ratio) (concat "1:" ratio)))
        (loki-assistant-xbloom--row "计划总水量 (mL)" volume)
        (loki-assistant-xbloom--row "研磨开关" (loki-assistant-xbloom--text (gethash "isSetGrinderSize" recipe) switch))
        (loki-assistant-xbloom--row "研磨档位" (gethash "grinderSize" recipe))
        (loki-assistant-xbloom--row "研磨转速 (RPM)" (gethash "rpm" recipe))
        (loki-assistant-xbloom--row "旁路开关" (loki-assistant-xbloom--text (gethash "isEnableBypassWater" recipe) switch))
        (loki-assistant-xbloom--row "旁路水量 (mL)" (gethash "bypassVolume" recipe))
        (loki-assistant-xbloom--row "旁路水温 (°C)" (gethash "bypassTemp" recipe))
        (save-excursion (goto-char start) (org-table-align)))
      (insert "\n** 分段注水\n\n")
      (let ((start (point))
            (index 0))
        (insert "| 段 | 水量 (mL) | 水温 (°C) | 流速 (mL/s) | 轨迹 | 驻留时间 (s) | 段前振动 | 段后振动 |\n|-\n")
        (seq-doseq (pour (gethash "pourList" recipe))
          (loki-assistant-xbloom--row
           (cl-incf index) (gethash "volume" pour) (gethash "temperature" pour)
           (gethash "flowRate" pour)
           (loki-assistant-xbloom--text
            (gethash "pattern" pour) '(("1" . "中心") ("2" . "螺旋") ("3" . "环绕")))
           (gethash "pausing" pour)
           (loki-assistant-xbloom--text (gethash "isEnableVibrationBefore" pour) switch)
           (loki-assistant-xbloom--text (gethash "isEnableVibrationAfter" pour) switch)))
        (save-excursion (goto-char start) (org-table-align)))
      (buffer-substring-no-properties (point-min) (point-max)))))

(defun loki-assistant-xbloom--file-settings ()
  "读取并校验 Org 文件头的同步设置。"
  (org-with-wide-buffer
   (let ((head (car (org-element-contents (org-element-parse-buffer))))
         (keys '("XBLOOM_SCOPE" "XBLOOM_REGION" "XBLOOM_USER" "XBLOOM_PASS"))
         properties)
     (when (eq (org-element-type head) 'section)
       (org-element-map head '(keyword node-property)
         (lambda (entry)
           (let ((key (upcase (org-element-property :key entry)))
                 (value (org-element-property :value entry)))
             (when (equal key "PROPERTY")
               (when (string-match (rx string-start (* blank)
                                       (group (+ (not blank))) (+ blank) (group (* any)))
                                   value)
                 (setq key (upcase (match-string 1 value))
                       value (match-string 2 value))))
             (when (member key keys)
               (when (assoc key properties)
                 (user-error "Org 文件头的 %s 设置重复" key))
               (push (cons key (string-trim value)) properties))))))
     (let ((scope (alist-get "XBLOOM_SCOPE" properties nil nil #'equal))
           (region (alist-get "XBLOOM_REGION" properties "cn" nil #'equal))
           (user (alist-get "XBLOOM_USER" properties "" nil #'equal))
           (password-path (alist-get "XBLOOM_PASS" properties "" nil #'equal)))
       (unless (member scope '("official" "personal" "all"))
         (user-error "请在 Org 文件头设置 XBLOOM_SCOPE 为 official、personal 或 all"))
       (unless (member region '("cn" "intl"))
         (user-error "Org 文件头的 XBLOOM_REGION 须为 cn 或 intl"))
       (when (string-blank-p user)
         (user-error "请在 Org 文件头设置 XBLOOM_USER"))
       (when (string-blank-p password-path)
         (user-error "请在 Org 文件头设置 XBLOOM_PASS"))
       (list region scope user password-path)))))

(defun loki-assistant-xbloom--entries (region)
  "返回当前文档中 REGION 的 TABLE_ID 与标题位置。"
  (let ((entries (make-hash-table :test 'equal)))
    (org-with-wide-buffer
     (org-element-map (org-element-parse-buffer 'headline) 'headline
       (lambda (headline)
         (when (= (org-element-property :level headline) 1)
           (save-excursion
             (goto-char (org-element-property :begin headline))
             (when-let* ((table-id (org-entry-get nil "TABLE_ID"))
                         ((equal (org-entry-get nil "REGION") region)))
               (when (gethash table-id entries)
                 (error "配方文档中有重复的 TABLE_ID %s" table-id))
               (puthash table-id (copy-marker (point) t) entries)))))))
    entries))

(defun loki-assistant-xbloom--store-recipe (recipe entries region scope)
  "将一条 RECIPE 写入 ENTRIES，返回处理结果和标题位置。"
  (let* ((table-id (loki-assistant-xbloom--text (gethash "tableId" recipe)))
         (existing (gethash table-id entries)))
    (unless (string-match-p (rx string-start (+ (any "0-9")) string-end) table-id)
      (error "xBloom 配方的 TABLE_ID 无效"))
    (when-let* ((pours (gethash "pourList" recipe))
                ((not (and (vectorp pours) (seq-every-p #'hash-table-p pours)))))
      (error "配方 %s 的分段注水数据无效" table-id))
    (org-with-wide-buffer
     (goto-char (or existing (point-max)))
     (let* ((id (or (and existing (org-entry-get nil "ID"))
                    (let ((org-id-method 'uuid)) (org-id-new))))
            (current-source (and existing (org-entry-get nil "SOURCE")))
            (source (if (and (equal scope "all") (member current-source '("official" "personal")))
                        current-source
                      scope))
            (new (loki-assistant-xbloom--entry recipe id region source))
            (status 'unchanged))
       (if existing
           (let* ((start (point))
                  (end (save-excursion (org-end-of-subtree t t) (point)))
                  (current (loki-assistant-xbloom-read-recipe))
                  (incoming (with-temp-buffer
                              (delay-mode-hooks (org-mode))
                              (insert new)
                              (goto-char (point-min))
                              (loki-assistant-xbloom-read-recipe))))
             (unless (and (cl-every #'equal current incoming)
                          (equal (string-trim-right (buffer-substring-no-properties start end))
                                 (string-trim-right new)))
               (delete-region start end)
               (insert new "\n")
               (set-marker existing start)
               (setq status 'updated)))
         (unless (bolp) (insert "\n"))
         (unless (looking-back "\n\n" (max (point-min) (- (point) 2))) (insert "\n"))
         (let ((start (point)))
           (insert new "\n")
           (setq existing (copy-marker start t))
           (puthash table-id existing entries))
         (setq status 'created))
       (cons status existing)))))

(defun loki-assistant-xbloom--follow (marker)
  "让显示配方库的窗口跟随 MARKER。"
  (dolist (window (get-buffer-window-list (marker-buffer marker) nil t))
    (set-window-point window marker))
  (when (get-buffer-window (marker-buffer marker) t)
    (redisplay t)))

;;;###autoload
(defun loki-assistant-xbloom-sync ()
  "按照当前 Org 文件头的设置同步 xBloom 咖啡配方。"
  (interactive)
  (when loki-assistant-xbloom--syncing
    (user-error "xBloom 同步正在进行"))
  (unless (and (derived-mode-p 'org-mode) buffer-file-name
               (not (file-remote-p buffer-file-name)))
    (user-error "请在本地 Org 配方文件中执行同步"))
  (unless (and (not (buffer-modified-p)) (file-exists-p buffer-file-name)
               (verify-visited-file-modtime (current-buffer)))
    (user-error "请先保存或重新载入当前 Org 配方文件"))
  (let* ((file (expand-file-name buffer-file-name))
         (program (executable-find (substitute-in-file-name loki-assistant-xbloom-program)))
         (settings (loki-assistant-xbloom--file-settings))
         (roam (and (require 'org-roam nil t) (require 'org-roam-db nil t)))
         (root (and roam (expand-file-name org-roam-directory)))
         (database (and roam org-roam-db-location)))
    (unless program
      (user-error "找不到 xbloom，请先运行 make install"))
    (loki-assistant-xbloom--start-stream program file settings root database)))

(defun loki-assistant-xbloom--start-stream (program file settings root database)
  "用 PROGRAM 下载配方流并更新 FILE。"
  (cl-destructuring-bind (region scope user password-path) settings
    (let ((target (current-buffer))
          (entries (loki-assistant-xbloom--entries region))
          (errors (generate-new-buffer " *xBloom 状态*"))
          (tick (buffer-chars-modified-tick))
          (pending "")
          (total 0) (created 0) (updated 0) failure)
      (cl-labels
          ((consume (line)
             (let ((recipe (json-parse-string line :null-object nil)))
               (unless (hash-table-p recipe)
                 (error "xBloom 输出的配方须为 JSON object"))
               (unless (equal (loki-assistant-xbloom--text (gethash "cupType" recipe)) "4")
                 (unless (buffer-live-p target) (error "同步目标已关闭"))
                 (with-current-buffer target
                   (unless (and (= tick (buffer-chars-modified-tick))
                                (verify-visited-file-modtime target))
                     (error "同步期间配方文件已更改"))
                   (let ((result (atomic-change-group
                                   (loki-assistant-xbloom--store-recipe recipe entries region scope))))
                     (setq tick (buffer-chars-modified-tick))
                     (cl-incf total)
                     (pcase (car result)
                       ('created (cl-incf created))
                       ('updated (cl-incf updated)))
                     (message "xBloom 已处理 %d 条，新增 %d 条，更新 %d 条" total created updated)
                     (loki-assistant-xbloom--follow (cdr result)))))))
           (receive (process chunk)
             (unless failure
               (condition-case err
                   (progn
                     (setq pending (concat pending chunk))
                     (while (string-match "\n" pending)
                       (let ((line (substring pending 0 (match-beginning 0))))
                         (setq pending (substring pending (match-end 0)))
                         (consume line))))
                 (error
                  (setq failure (error-message-string err))
                  (delete-process process)))))
           (finish (process _event)
             (when (memq (process-status process) '(exit signal))
               (unwind-protect
                   (condition-case err
                       (progn
                         (when (and (> (+ created updated) 0) (buffer-live-p target))
                           (with-current-buffer target
                             (unless (and (= tick (buffer-chars-modified-tick))
                                          (verify-visited-file-modtime target))
                               (error "文件已更改，请核对尚未保存的同步内容"))
                             (save-buffer))
                           (let ((org-roam-directory root)
                                 (org-roam-db-location database))
                             (when (and root (file-in-directory-p file root))
                               (org-roam-db-update-file file))))
                         (let ((diagnostic (with-current-buffer errors (string-trim (buffer-string)))))
                           (cond
                            (failure (error "%s" failure))
                            ((not (zerop (process-exit-status process)))
                             (error "xbloom 退出码 %d%s" (process-exit-status process)
                                    (if (string-empty-p diagnostic) "" (concat "，" diagnostic))))
                            ((not (string-empty-p pending))
                             (error "xBloom 的最后一条配方记录不完整"))))
                         (message "xBloom 同步完成，已处理 %d 条，新增 %d 条，更新 %d 条"
                                  total created updated))
                     (error (message "xBloom 同步中断，已处理 %d 条。%s"
                                     total (error-message-string err))))
                 (setq loki-assistant-xbloom--syncing nil)
                 (kill-buffer errors)))))
        (setq loki-assistant-xbloom--syncing t)
        (condition-case err
            (make-process
             :name "xbloom-sync"
             :command (list program "export" "--source" scope "--region" region "--email" user
                            (concat "--password=" password-path) "--json-lines" "-o" "-")
             :stderr errors :coding 'utf-8-unix :connection-type 'pipe :noquery t
             :filter #'receive :sentinel #'finish)
          (error
           (setq loki-assistant-xbloom--syncing nil)
           (kill-buffer errors)
           (signal (car err) (cdr err))))
        (message "正在下载 xBloom 配方")))))

(defvar-keymap loki-assistant-xbloom-sync-mode-map
  "C-c x r" #'loki-assistant-xbloom-sync
  "C-c C-q" (lambda () (interactive) (loki-assistant-xbloom-sync-mode -1))
  "C-c ?" #'describe-mode)

(define-minor-mode loki-assistant-xbloom-sync-mode
  "在配置了 xBloom 同步参数的 Org 缓冲区提供配方同步。"
  :init-value nil
  :lighter (:eval (loki-assistant--mode-line-keys "C-c x r 刷新配方" "C-c ?"))
  :keymap loki-assistant-xbloom-sync-mode-map)

(provide 'loki-assistant-xbloom)
;;; loki-assistant-xbloom.el ends here
