;;; loki-assistant-xbloom-service.el --- 查询和保存 xBloom Org 配方 -*- lexical-binding: t; -*-

(require 'loki-assistant-xbloom)

(defvar org-roam-dailies-directory)
(defvar org-roam-db-update-on-save)
(declare-function org-roam-list-files "org-roam")
(declare-function loki-assistant-daily-capture "loki-assistant-daily" (key prepare fail))
(declare-function loki-assistant-daily-check-buffer "loki-assistant-daily" (buffer fail))

(defcustom loki-assistant-xbloom-daily-template-key "d"
  "用于新建 xBloom 配方的 Org-roam 日记模板键。"
  :type 'string
  :group 'loki-assistant-xbloom)

(defconst loki-assistant-xbloom-service--properties
  '((:model "MODEL" model) (:coffee "COFFEE") (:roaster "ROASTER")))

(defconst loki-assistant-xbloom-service--brewing
  '((:cup_type "杯型" cup) (:dose_g "粉量 (g)" number) (:water_ratio "粉水比" ratio)
    (:water_ml "计划总水量 (mL)" number) (:grinder_enabled "研磨开关" switch)
    (:grind_size "研磨档位" number) (:rpm "研磨转速 (RPM)" number)
    (:bypass_enabled "旁路开关" switch) (:bypass_ml "旁路水量 (mL)" number)
    (:bypass_temp_c "旁路水温 (°C)" number)))

(defconst loki-assistant-xbloom-service--pour
  '((:volume_ml "水量 (mL)" number) (:temperature_c "水温 (°C)" number)
    (:flow_ml_s "流速 (mL/s)" number) (:pattern "轨迹" pattern)
    (:pause_s "驻留时间 (s)" number) (:vibrate_before "段前振动" switch)
    (:vibrate_after "段后振动" switch)))

(defconst loki-assistant-xbloom-service--values
  '((model ("Studio" . "studio") ("Original" . "original"))
    (cup ("xPod" . "xpod") ("Omni" . "omni") ("Other" . "other"))
    (pattern ("中心" . "center") ("螺旋" . "spiral") ("环绕" . "circle"))
    (switch ("开启" . t) ("关闭" . :false))))

(defun loki-assistant-xbloom-service--fail (code)
  "以业务错误 CODE 结束请求。"
  (throw 'loki-assistant-xbloom-service-error `((error . ,code))))

(defun loki-assistant-xbloom-service--daily-failure (reason)
  "将日记保存失败原因 REASON 转换为配方业务错误。"
  (loki-assistant-xbloom-service--fail
   (pcase reason
     ('template "daily-template-unavailable")
     ('conflict "daily-conflict")
     ('save "recipe-save-failed"))))

(defun loki-assistant-xbloom-service--text (text)
  "将 Org 中为空的 TEXT 转为 nil。"
  (and (stringp text) (not (string-blank-p text)) (string-trim text)))

(defun loki-assistant-xbloom-service--decode (text type)
  "按 TYPE 解析 Org 单元格 TEXT。"
  (when (setq text (loki-assistant-xbloom-service--text text))
    (pcase type
      ('nil text)
      ('ratio (when (string-prefix-p "1:" text)
                (loki-assistant-xbloom-service--decode (substring text 2) 'number)))
      ('number
       (when (string-match-p
              (rx string-start (? (any "+-")) (or (seq (+ digit) (? "." (* digit)))
                                                   (seq "." (+ digit)))
                  (? (any "Ee") (? (any "+-")) (+ digit)) string-end)
              text)
         (let ((number (string-to-number text)))
           (when (< (abs number) 1.0e+INF) number))))
      (_ (cdr (assoc text (alist-get type loki-assistant-xbloom-service--values)))))))

(defun loki-assistant-xbloom-service--encode (value type)
  "将已验证的 VALUE 按 TYPE 转为 Org 单元格文本。"
  (when value
    (pcase type
      ('nil value)
      ('number (number-to-string value))
      ('ratio (concat "1:" (number-to-string value)))
      (_ (car (rassoc value (alist-get type loki-assistant-xbloom-service--values)))))))

(defun loki-assistant-xbloom-service--object (fields cells)
  "将按 FIELDS 标注的 CELLS 转换为 JSON 对象。"
  (cl-loop for (key _ type) in fields
           for cell in cells
           collect (cons (intern (substring (symbol-name key) 1))
                         (or (loki-assistant-xbloom-service--decode cell type) :null))))

(defun loki-assistant-xbloom-service--record (headline daily)
  "将 HEADLINE 解析为结构化配方，DAILY 非空表示日记条目。"
  (let* ((raw (loki-assistant-xbloom--read-headline headline))
         (properties (alist-get 'properties raw))
         (profile (alist-get 'profile raw))
         (tables (alist-get 'tables raw))
         (brewing (cdr (car tables)))
         (pour-table (cadr tables))
         (pour-columns (cdr (car pour-table)))
         (fields loki-assistant-xbloom-service--brewing)
         (pour-fields loki-assistant-xbloom-service--pour)
         (date (cdr (assoc "SAVED_ON" properties))))
    `((id . ,(cdr (assoc "ID" properties)))
      (storage . ,(if daily "daily" "library"))
      (table_id . ,(or (loki-assistant-xbloom-service--text (cdr (assoc "TABLE_ID" properties))) :null))
      (region . ,(or (loki-assistant-xbloom-service--text (cdr (assoc "REGION" properties))) :null))
      (source . ,(or (loki-assistant-xbloom-service--text (cdr (assoc "SOURCE" properties))) :null))
      (saved_on . ,(if (and date (string-match (rx string-start "[" (group (= 4 digit) "-" (= 2 digit) "-" (= 2 digit))
                                                   (any " ]")) date))
                       (match-string 1 date)
                     :null))
      (based_on_id . ,(or (loki-assistant-xbloom-service--text (cdr (assoc "XBLOOM_BASED_ON" properties))) :null))
      (data . ((title . ,(alist-get 'title raw))
               (url . ,(or (alist-get 'url raw) :null))
               (tags . ,(vconcat (alist-get 'tags raw)))
               ,@(loki-assistant-xbloom-service--object
                  loki-assistant-xbloom-service--properties
                  (mapcar (lambda (field) (cdr (assoc (cadr field) properties)))
                          loki-assistant-xbloom-service--properties))
               (origin . ,(or (loki-assistant-xbloom-service--text (cdr (assoc "ORIGIN" profile))) :null))
               (process . ,(or (loki-assistant-xbloom-service--text (cdr (assoc "PROCESS" profile))) :null))
               (flavor_notes . ,(or (loki-assistant-xbloom-service--text (cdr (assoc "FLAVOR_NOTES" profile))) :null))
               (brewing . ,(loki-assistant-xbloom-service--object
                            fields (mapcar (lambda (field) (cadr (assoc (cadr field) brewing))) fields)))
               (pours . ,(vconcat
                          (cl-loop for row in (cdr pour-table)
                                   for cells = (mapcar (lambda (field)
                                                        (when-let* ((index (seq-position pour-columns (cadr field))))
                                                          (nth index (cdr row))))
                                                      pour-fields)
                                   when (seq-some #'loki-assistant-xbloom-service--text cells)
                                   collect (loki-assistant-xbloom-service--object pour-fields cells)))))))))

(defun loki-assistant-xbloom-service--recipe-p (headline daily)
  "判断带 Org ID 的 HEADLINE 是否为库或 DAILY 配方。"
  (and (loki-assistant-xbloom-service--text (org-element-property :ID headline))
       (not (org-element-property :commentedp headline))
       (not (member "noexport" (org-element-property :tags headline)))
       (or daily (= (org-element-property :level headline) 1))
       (loki-assistant-xbloom--child headline "冲煮参数")
       (loki-assistant-xbloom--child headline "分段注水")))

(defun loki-assistant-xbloom-service--scan (visit &optional id)
  "在已保存的 Org-roam 文件中调用 VISIT 处理配方标题。
给定 ID 时只处理匹配的标题。"
  (let ((daily-root (expand-file-name org-roam-dailies-directory org-roam-directory)))
    (dolist (file (sort (delete-dups (org-roam-list-files)) #'string<))
      (when (and (not (file-remote-p file))
                 (string-suffix-p ".org" file t) (file-regular-p file))
        (with-temp-buffer
          (insert-file-contents file)
          (when (and (or (null id)
                         (re-search-forward (concat "^[ \t]*:ID:[ \t]+" (regexp-quote id) "[ \t]*$") nil t))
                     (progn (goto-char (point-min))
                            (re-search-forward "^\\*+ 冲煮参数\\(?:[ \t]\\|$\\)" nil t)))
            (delay-mode-hooks (org-mode))
            (let ((daily (file-in-directory-p file daily-root)))
              (org-element-map (org-element-parse-buffer 'element) 'headline
                (lambda (headline)
                  (when (and (or (null id) (equal id (org-element-property :ID headline)))
                             (loki-assistant-xbloom-service--recipe-p headline daily))
                    (funcall visit headline file daily)))))))))))

(defun loki-assistant-xbloom-service--find (id)
  "查找 ID 对应的唯一配方，返回配方、文件和标题位置。"
  (let (found)
    (loki-assistant-xbloom-service--scan
     (lambda (headline file daily)
       (when found (loki-assistant-xbloom-service--fail "recipe-conflict"))
       (setq found (list (loki-assistant-xbloom-service--record headline daily)
                         file (org-element-property :begin headline))))
     id)
    (or found (loki-assistant-xbloom-service--fail "recipe-not-found"))))

(defun loki-assistant-xbloom-service--list (request)
  "按 REQUEST 搜索配方并分页。"
  (let ((q (plist-get request :q))
        (offset (plist-get request :offset))
        (limit (plist-get request :limit))
        (ids (make-hash-table :test #'equal))
        daily library)
    (loki-assistant-xbloom-service--scan
     (lambda (headline _file in-daily)
       (let* ((recipe (loki-assistant-xbloom-service--record headline in-daily))
              (id (alist-get 'id recipe))
              (data (alist-get 'data recipe))
              (case-fold-search t))
         (when (gethash id ids) (loki-assistant-xbloom-service--fail "recipe-conflict"))
         (puthash id t ids)
         (when (or (null q)
                   (seq-some (lambda (key)
                               (let ((value (alist-get key data)))
                                 (and (stringp value) (string-search (downcase q) (downcase value)))))
                             '(title coffee roaster origin process flavor_notes))
                   (seq-some (lambda (tag) (string-match-p (regexp-quote q) tag))
                             (alist-get 'tags data)))
           (if in-daily (push recipe daily) (push recipe library))))))
    (let* ((all (nconc daily (nreverse library)))
           (end (+ offset limit))
           (page (seq-take (nthcdr offset all) limit)))
      `((recipes . ,(vconcat page))
        (next_offset . ,(if (nthcdr end all) end :null))))))

(defun loki-assistant-xbloom-service--table-text (recipe &optional pours)
  "生成 RECIPE 的整杯参数表，POURS 非空时生成分段表。"
  (with-temp-buffer
    (delay-mode-hooks (org-mode))
    (if pours
        (let ((fields loki-assistant-xbloom-service--pour))
          (apply #'loki-assistant-xbloom--row "段" (mapcar #'cadr fields))
          (insert "|-\n")
          (cl-loop for row across (plist-get recipe :pours)
                   for index from 1
                   do (apply #'loki-assistant-xbloom--row index
                             (mapcar (lambda (field)
                                       (loki-assistant-xbloom-service--encode
                                        (plist-get row (car field)) (nth 2 field)))
                                     fields))))
      (loki-assistant-xbloom--row "参数" "值")
      (insert "|-\n")
      (cl-loop for (key label type) in loki-assistant-xbloom-service--brewing
               do (loki-assistant-xbloom--row
                   label (loki-assistant-xbloom-service--encode
                          (plist-get (plist-get recipe :brewing) key) type))))
    (goto-char (point-min))
    (org-table-align)
    (buffer-string)))

(defun loki-assistant-xbloom-service--profile-text (recipe)
  "将 RECIPE 的产地、处理法和风味写成 Org 描述列表。"
  (with-temp-buffer
    (loki-assistant-xbloom--insert-profile
     (cl-mapcar #'cons '("ORIGIN" "PROCESS" "FLAVOR_NOTES")
                (mapcar (lambda (key) (plist-get recipe key)) '(:origin :process :flavor_notes))))
    (buffer-string)))

(defun loki-assistant-xbloom-service--validate (recipe)
  "检查 RECIPE 的文本字段能否安全写入 Org。"
  (unless
      (and (plist-get recipe :title)
           (cl-every (lambda (key)
                       (let ((text (plist-get recipe key)))
                         (or (null text)
                             (and (equal text (loki-assistant-xbloom-service--text text))
                                  (not (string-match-p "[[:cntrl:]]" text))))))
                     '(:title :coffee :roaster :origin :process :flavor_notes))
           (seq-every-p (lambda (tag) (string-match-p (rx string-start (+ (any alnum "_@#%")) string-end) tag))
                        (plist-get recipe :tags))
           (or (null (plist-get recipe :url))
               (let ((url (plist-get recipe :url)))
                 (and (not (string-match-p "[[:space:][:cntrl:]]" url))
                      (let ((parsed (url-generic-parse-url url)))
                        (and (member (url-type parsed) '("http" "https"))
                             (loki-assistant-xbloom-service--text (url-host parsed))))))))
    (loki-assistant-xbloom-service--fail "invalid-request")))

(defun loki-assistant-xbloom-service--replace-table (child text)
  "替换 CHILD 中的配方表格，或在标题下插入 TEXT。"
  (if-let* ((table (loki-assistant-xbloom--table child)))
      (progn
        (goto-char (org-element-property :post-affiliated table))
        (while (looking-at-p "[ \t]*|")
          (delete-region (point) (line-beginning-position 2))))
    (goto-char (org-element-property :begin child))
    (org-end-of-meta-data t))
  (insert text))

(defun loki-assistant-xbloom-service--write (recipe &optional existing)
  "在当前位置写入 RECIPE，EXISTING 非空时保留非配方内容。"
  (let* ((title (plist-get recipe :title))
         (url (plist-get recipe :url))
         (tags (append (plist-get recipe :tags) nil))
         (profile (loki-assistant-xbloom-service--profile-text recipe))
         (brewing (loki-assistant-xbloom-service--table-text recipe))
         (pours (loki-assistant-xbloom-service--table-text recipe t)))
    (save-excursion
      (if existing
          (let ((headline (loki-assistant-xbloom--headline)))
            ;; 从末尾向前修改，保留 Org 元素记录的前部位置。
            (loki-assistant-xbloom-service--replace-table
             (loki-assistant-xbloom--child headline "分段注水") pours)
            (loki-assistant-xbloom-service--replace-table
             (loki-assistant-xbloom--child headline "冲煮参数") brewing)
            (dolist (line (sort (mapcar #'cdr (loki-assistant-xbloom--body-profile
                                              (car (org-element-contents headline)) t)) #'>))
              (goto-char line)
              (delete-region (point) (line-beginning-position 2))))
        (let ((stars (make-string (1+ (org-outline-level)) ?*)))
          (org-end-of-subtree t t)
          (unless (bolp) (insert "\n"))
          (insert "\n" stars " 冲煮参数\n\n" brewing "\n"
                  stars " 分段注水\n\n" pours))))
    (org-edit-headline (if url (org-link-make-string url title) title))
    (let ((org-after-tags-change-hook nil) (org-tags-sort-function nil))
      (org-set-tags tags))
    (dolist (field loki-assistant-xbloom-service--properties)
      (let ((value (loki-assistant-xbloom-service--encode (plist-get recipe (car field)) (nth 2 field))))
        (if value (org-entry-put nil (cadr field) value)
          (org-entry-delete nil (cadr field)))))
    (unless (string-empty-p profile)
      (save-excursion
        (org-end-of-meta-data t)
        (insert profile)))
    (let* ((headline (loki-assistant-xbloom--headline))
           (saved (loki-assistant-xbloom-service--record headline t))
           (data (alist-get 'data saved)))
      (unless (and (loki-assistant-xbloom-service--recipe-p headline t)
                   (equal title (alist-get 'title data))
                   (equal (or url :null) (alist-get 'url data))
                   (equal (delete-dups tags) (append (alist-get 'tags data) nil)))
        (loki-assistant-xbloom-service--fail "invalid-request"))
      saved)))

(defun loki-assistant-xbloom-service--index (file)
  "尝试更新 FILE 的 Org-roam 索引，返回 JSON 布尔值。"
  (condition-case nil
      (let ((enable-local-variables nil) (enable-local-eval nil))
        (org-roam-db-update-file file)
        t)
    (error :false)))

(defun loki-assistant-xbloom-service--create (info)
  "将 INFO 中的配方写入当天日记。"
  (let ((recipe (plist-get info :recipe))
        (based-on (plist-get info :based_on_id))
        saved)
    (loki-assistant-xbloom-service--validate recipe)
    (when based-on (loki-assistant-xbloom-service--find based-on))
    (let ((file
           (condition-case nil
               (loki-assistant-daily-capture
                loki-assistant-xbloom-daily-template-key
                (lambda ()
                  (let ((org-id-method 'uuid))
                    (org-entry-put nil "ID" (org-id-new)))
                  (org-entry-put nil "SAVED_ON" (format-time-string "[%F]"))
                  (when based-on (org-entry-put nil "XBLOOM_BASED_ON" based-on))
                  (setq saved (loki-assistant-xbloom-service--write recipe)))
                #'loki-assistant-xbloom-service--daily-failure)
             (error (loki-assistant-xbloom-service--fail "recipe-save-failed")))))
      `((recipe . ,saved) (indexed . ,(loki-assistant-xbloom-service--index file))))))

(defun loki-assistant-xbloom-service--update (id recipe)
  "完整更新日记中 ID 对应的配方字段。"
  (loki-assistant-xbloom-service--validate recipe)
  (pcase-let* ((`(,current ,file ,position) (loki-assistant-xbloom-service--find id))
               (enable-local-variables nil)
               (enable-local-eval nil)
               (org-roam-db-update-on-save nil)
               (visiting (org-find-base-buffer-visiting file))
               (saved nil))
    (unless (equal (alist-get 'storage current) "daily")
      (loki-assistant-xbloom-service--fail "recipe-read-only"))
    (loki-assistant-daily-check-buffer visiting #'loki-assistant-xbloom-service--daily-failure)
    (condition-case nil
        (with-current-buffer (or visiting (find-file-noselect file))
          (org-with-wide-buffer
           (goto-char position)
           (unless (and (org-at-heading-p) (equal id (org-entry-get nil "ID")))
             (loki-assistant-xbloom-service--daily-failure 'conflict))
           (atomic-change-group
             (setq saved (loki-assistant-xbloom-service--write recipe t))
             (save-buffer))))
      (error (loki-assistant-xbloom-service--fail "recipe-save-failed")))
    `((recipe . ,saved) (indexed . ,(loki-assistant-xbloom-service--index file)))))

(defun loki-assistant-xbloom-service-request (file)
  "处理 FILE 中的配方 JSON 请求，返回 Base64 编码的 JSON。"
  (let ((reply
         (catch 'loki-assistant-xbloom-service-error
           (unless (and (require 'org-roam nil t) (require 'loki-assistant-daily nil t))
             (loki-assistant-xbloom-service--fail "xbloom-unavailable"))
           (condition-case nil
               (let ((request (with-temp-buffer
                                (let ((coding-system-for-read 'utf-8-unix))
                                  (insert-file-contents file))
                                (json-parse-buffer :object-type 'plist :null-object nil))))
                 (list (cons 'result
                             (pcase (plist-get request :operation)
                               ("list" (loki-assistant-xbloom-service--list request))
                               ("get" (car (loki-assistant-xbloom-service--find (plist-get request :id))))
                               ("create" (loki-assistant-xbloom-service--create (plist-get request :info)))
                               ("update" (loki-assistant-xbloom-service--update
                                          (plist-get request :id) (plist-get request :recipe)))
                               (_ (loki-assistant-xbloom-service--fail "invalid-request"))))))
             (error (loki-assistant-xbloom-service--fail "recipe-read-failed"))))))
    (base64-encode-string (encode-coding-string (json-serialize reply) 'utf-8) t)))

(provide 'loki-assistant-xbloom-service)
;;; loki-assistant-xbloom-service.el ends here
