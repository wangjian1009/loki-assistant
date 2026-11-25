;;; loki-assistant-llm-usage.el --- 查询 LLM 用量 -*- lexical-binding: t; -*-

(require 'cl-lib)
(require 'calendar)
(require 'json)
(require 'subr-x)
(require 'tabulated-list)
(require 'parse-time)
(require 'loki-assistant)

(defconst loki-assistant-llm-usage--views
  '((overview "概览" "/llm/usage/summary")
    (history "请求历史" "/llm/usage/requests")
    (sessions "会话" "/llm/usage/sessions")
    (totals "全量累计" "/llm/usage")
    (live "实时请求" "/llm/requests")))

(defconst loki-assistant-llm-usage--fields
  '((client_id . "客户端") (upstream . "供应商") (model . "模型")
    (protocol . "协议") (session_id . "会话 ID") (outcome . "结果")
    (incomplete . "用量完整性") (from_date . "起始日期")
    (to_date . "排除的结束日期") (from . "起始时间") (to . "排除的结束时间")
    (group_by . "分组") (bucket . "周期")))

(cl-defstruct loki-assistant-llm-usage--view
  filters items data cursor selected loading error)

(defvar-local loki-assistant-llm-usage--views-state nil)
(defvar-local loki-assistant-llm-usage--current 'overview)
(defvar-local loki-assistant-llm-usage--zone "Asia/Shanghai")
(defvar-local loki-assistant-llm-usage--context nil)

(defun loki-assistant-llm-usage--state ()
  (or (gethash loki-assistant-llm-usage--current loki-assistant-llm-usage--views-state)
      (puthash loki-assistant-llm-usage--current
               (make-loki-assistant-llm-usage--view)
               loki-assistant-llm-usage--views-state)))

(defun loki-assistant-llm-usage--value (key row)
  (let ((value (alist-get key row)))
    (unless (memq value '(nil :null :false)) value)))

(defun loki-assistant-llm-usage--cell (key row &optional missing)
  (if-let ((value (loki-assistant-llm-usage--value key row)))
      (format "%s" value)
    (or missing "—")))

(defun loki-assistant-llm-usage--phase (value)
  (or (cdr (assoc value '(("waiting" . "等待响应") ("streaming" . "正在输出")
                          ("completed" . "完成") ("failed" . "失败")
                          ("cancelled" . "取消"))))
      "—"))

(defun loki-assistant-llm-usage--usage-label (usage)
  (cond ((not (consp usage)) "—")
        ((eq (alist-get 'complete usage) t) "完整")
        (t "不完整")))

(defun loki-assistant-llm-usage--time (value)
  (condition-case nil
      (let ((time (if (consp value)
                      (seconds-to-time (or (alist-get 'secs_since_epoch value) 0))
                    (and (stringp value) (parse-iso8601-time-string value)))))
        (if time (format-time-string "%Y-%m-%d %H:%M:%S" time
                                     loki-assistant-llm-usage--zone)
          "—"))
    (error (format "%s" value))))

(defconst loki-assistant-llm-usage--token-fields
  '(input_uncached_tokens cache_read_tokens cache_write_tokens output_tokens))

(defun loki-assistant-llm-usage--sum (row)
  (cl-loop for field in loki-assistant-llm-usage--token-fields
           sum (or (loki-assistant-llm-usage--value field row) 0)))

(defun loki-assistant-llm-usage--result-items (view data)
  (append (pcase view
            ('live (vconcat (alist-get 'active data) (alist-get 'recent data)))
            ((or 'sessions 'totals) (alist-get 'usage data))
            (_ (alist-get 'items data))) nil))

(defun loki-assistant-llm-usage--params (filters)
  (mapcar (lambda (pair) (cons (symbol-name (car pair)) (cdr pair))) filters))

(defun loki-assistant-llm-usage--load (&optional more)
  (let* ((buffer (current-buffer))
         (view loki-assistant-llm-usage--current)
         (state (loki-assistant-llm-usage--state))
         (generation (list nil))
         (filters (copy-tree (loki-assistant-llm-usage--view-filters state))))
    (when (memq view '(history sessions)) (push '(limit . "50") filters))
    (when more (push (cons 'cursor (loki-assistant-llm-usage--view-cursor state)) filters))
    (setf (loki-assistant-llm-usage--view-loading state) generation
          (loki-assistant-llm-usage--view-error state) nil)
    (force-mode-line-update)
    (loki-assistant-request
     "GET" (nth 2 (assq view loki-assistant-llm-usage--views))
     :params (loki-assistant-llm-usage--params filters)
     :callback
     (lambda (reply)
       (when (buffer-live-p buffer)
         (with-current-buffer buffer
           (when (eq generation (loki-assistant-llm-usage--view-loading state))
             (setf (loki-assistant-llm-usage--view-loading state) nil)
             (if (plist-get reply :ok)
                 (let* ((data (plist-get reply :data))
                        (items (loki-assistant-llm-usage--result-items view data)))
                   (setf (loki-assistant-llm-usage--view-data state) data
                         (loki-assistant-llm-usage--view-items state)
                         (if more (append (loki-assistant-llm-usage--view-items state) items) items)
                         (loki-assistant-llm-usage--view-cursor state)
                         (loki-assistant-llm-usage--value 'next_cursor data))
                   (when-let ((zone (loki-assistant-llm-usage--value 'time_zone data)))
                     (setq loki-assistant-llm-usage--zone zone)))
               (setf (loki-assistant-llm-usage--view-error state)
                     (or (plist-get reply :error-message) "读取用量失败")))
             (when (eq view loki-assistant-llm-usage--current)
               (loki-assistant-llm-usage--render)))))))))

(defun loki-assistant-llm-usage--row (item max-token)
  (cl-labels ((cell (key &optional missing) (loki-assistant-llm-usage--cell key item missing)))
    (pcase loki-assistant-llm-usage--current
      ('overview
       (vconcat (vector (cell 'period_start) (cell 'group "全部"))
                (mapcar #'cell loki-assistant-llm-usage--token-fields)
                (vector (cell 'requests) (cell 'incomplete_requests)
                        (make-string (if (zerop max-token) 0
                                       (/ (* 16 (loki-assistant-llm-usage--sum item)) max-token))
                                     ?▰))))
      ('history
       (vector (loki-assistant-llm-usage--time (alist-get 'finished_at item))
               (cell 'upstream) (cell 'model)
               (loki-assistant-llm-usage--phase (alist-get 'outcome item))
               (cell 'duration_ms)
               (loki-assistant-llm-usage--cell 'output_tokens (alist-get 'usage item) "未上报")
               (loki-assistant-llm-usage--usage-label (alist-get 'usage item))))
      ('live
       (vector (loki-assistant-llm-usage--time (alist-get 'started_at item))
               (cell 'upstream) (cell 'endpoint)
               (loki-assistant-llm-usage--phase (alist-get 'phase item))
               (cell 'first_byte_ms) (cell 'duration_ms)
               (loki-assistant-llm-usage--usage-label (alist-get 'usage item))))
      (_
       (vconcat (when (eq loki-assistant-llm-usage--current 'sessions)
                  (vector (cell 'session_id)))
                (mapcar #'cell '(client_id upstream input_tokens output_tokens requests incomplete_requests)))))))

(defun loki-assistant-llm-usage--id (item)
  (or (alist-get 'id item)
      (mapcar (lambda (key) (alist-get key item))
              (if (eq loki-assistant-llm-usage--current 'overview)
                  '(period_start group)
                '(client_id session_id upstream)))))

(defun loki-assistant-llm-usage--render ()
  (let* ((state (loki-assistant-llm-usage--state))
         (data (loki-assistant-llm-usage--view-data state))
         (items (loki-assistant-llm-usage--view-items state))
         (selected (or (tabulated-list-get-id)
                       (loki-assistant-llm-usage--view-selected state)))
         (max-token (if (eq loki-assistant-llm-usage--current 'overview)
                        (apply #'max 0 (mapcar #'loki-assistant-llm-usage--sum items))
                      0)))
    (setq tabulated-list-format
          (pcase loki-assistant-llm-usage--current
            ('overview [("日期" 12 nil) ("分组" 17 nil) ("未缓存输入" 14 nil)
                        ("缓存读取" 13 nil) ("缓存写入" 13 nil) ("输出" 12 nil)
                        ("请求" 8 nil) ("不完整" 8 nil) ("趋势" 16 nil)])
            ('history [("完成时间" 20 nil) ("供应商" 15 nil) ("模型" 20 nil)
                       ("结果" 8 nil) ("耗时 ms" 10 nil) ("输出" 12 nil) ("用量" 8 nil)])
            ('live [("开始时间" 20 nil) ("供应商" 15 nil) ("接口" 16 nil)
                    ("状态" 12 nil) ("首包 ms" 10 nil) ("耗时 ms" 10 nil) ("用量" 8 nil)])
            (_ (vconcat (when (eq loki-assistant-llm-usage--current 'sessions)
                          [("会话" 26 nil)])
                        [("客户端" 38 nil) ("供应商" 17 nil) ("上游输入" 16 nil)
                         ("输出" 12 nil) ("请求" 8 nil) ("不完整" 8 nil)])))
          tabulated-list-entries
          (mapcar (lambda (item)
                    (list (loki-assistant-llm-usage--id item)
                          (loki-assistant-llm-usage--row item max-token))) items))
    (tabulated-list-init-header)
    (tabulated-list-print t)
    (when selected
      (goto-char (point-min))
      (while (and (not (eobp)) (not (equal selected (tabulated-list-get-id))))
        (forward-line 1))
      (when (eobp) (goto-char (point-min))))
    (setq-local mode-line-process
                (list " · " (nth 1 (assq loki-assistant-llm-usage--current loki-assistant-llm-usage--views))
                      " · 1-5 切换  f 筛选  g 刷新  RET 打开"
                      (when (loki-assistant-llm-usage--view-cursor state) "  l 下一页")
                      (when (loki-assistant-llm-usage--view-loading state) " · 查询中…")
                      (when (loki-assistant-llm-usage--view-error state)
                        (concat " · " (loki-assistant-llm-usage--view-error state)))))
    (let ((inhibit-read-only t))
      (save-excursion
        (goto-char (point-max))
        (insert "\n")
        (when (and data (null items)) (insert "无匹配记录\n"))
        (when (eq t (alist-get 'storage_error data))
          (insert (propertize "最近一次落盘失败，用量可能滞后\n" 'face 'warning)))
        (when (eq loki-assistant-llm-usage--current 'totals)
          (insert "全量累计含导入的旧数据；输入按上游原始口径。\n"))
        (when-let ((totals (alist-get 'totals data)))
          (insert (format "%s 至 %s（不含）  %s\n"
                          (alist-get 'from_date data) (alist-get 'to_date data)
                          loki-assistant-llm-usage--zone))
          (insert (format "区间总量  未缓存输入 %s  缓存读取 %s  缓存写入 %s  输出 %s  请求 %s  不完整 %s\n"
                          (alist-get 'input_uncached_tokens totals) (alist-get 'cache_read_tokens totals)
                          (alist-get 'cache_write_tokens totals) (alist-get 'output_tokens totals)
                          (alist-get 'requests totals) (alist-get 'incomplete_requests totals)))
          (when-let ((start (loki-assistant-llm-usage--value 'data_from data)))
            (insert (format "历史明细自 %s 起\n" (loki-assistant-llm-usage--time start)))))
        (when-let ((filters (loki-assistant-llm-usage--view-filters state)))
          (insert "筛选  " (mapconcat
                           (lambda (pair) (format "%s %s" (alist-get (car pair) loki-assistant-llm-usage--fields)
                                                  (cdr pair))) filters " · ") "\n"))))))

(defun loki-assistant-llm-usage--switch (view)
  (setf (loki-assistant-llm-usage--view-selected (loki-assistant-llm-usage--state))
        (tabulated-list-get-id))
  (setq loki-assistant-llm-usage--current view)
  (let ((inhibit-read-only t)) (erase-buffer))
  (loki-assistant-llm-usage--render)
  (loki-assistant-llm-usage--load))

(defun loki-assistant-llm-usage-overview () "打开用量概览。" (interactive) (loki-assistant-llm-usage--switch 'overview))
(defun loki-assistant-llm-usage-history () "打开请求历史。" (interactive) (loki-assistant-llm-usage--switch 'history))
(defun loki-assistant-llm-usage-sessions () "打开会话用量。" (interactive) (loki-assistant-llm-usage--switch 'sessions))
(defun loki-assistant-llm-usage-totals () "打开全量累计用量。" (interactive) (loki-assistant-llm-usage--switch 'totals))
(defun loki-assistant-llm-usage-live () "打开实时请求。" (interactive) (loki-assistant-llm-usage--switch 'live))
(defun loki-assistant-llm-usage-refresh () "按当前筛选条件重新加载。" (interactive) (loki-assistant-llm-usage--load))

(defun loki-assistant-llm-usage-more ()
  "加载下一页。"
  (interactive)
  (let ((state (loki-assistant-llm-usage--state)))
    (when (loki-assistant-llm-usage--view-loading state) (user-error "正在查询"))
    (unless (loki-assistant-llm-usage--view-cursor state) (user-error "已到末页"))
    (loki-assistant-llm-usage--load t)))

(defun loki-assistant-llm-usage--date-time (date &optional offset)
  (let* ((parts (parse-time-string (concat date "T00:00:00")))
         (time (encode-time 0 0 0 (+ (nth 3 parts) (or offset 0))
                            (nth 4 parts) (nth 5 parts) loki-assistant-llm-usage--zone)))
    (format-time-string "%Y-%m-%dT%H:%M:%SZ" time t)))

(defun loki-assistant-llm-usage--valid-date-p (value)
  (and (string-match-p (rx string-start (= 4 digit) "-" (= 2 digit)
                          "-" (= 2 digit) string-end) value)
       (calendar-date-is-valid-p
        (list (string-to-number (substring value 5 7))
              (string-to-number (substring value 8 10))
              (string-to-number (substring value 0 4))))))

(defun loki-assistant-llm-usage--history-filters (filters)
  (let ((result (cl-remove-if-not (lambda (pair) (memq (car pair)
      '(client_id upstream protocol model session_id from to))) (copy-tree filters))))
    (dolist (bound '((from_date . from) (to_date . to)))
      (when-let ((date (alist-get (car bound) filters)))
        (push (cons (cdr bound) (loki-assistant-llm-usage--date-time date)) result)))
    result))

(defun loki-assistant-llm-usage--set-filters (state filters)
  (setf (loki-assistant-llm-usage--view-filters state) filters
        (loki-assistant-llm-usage--view-data state) nil
        (loki-assistant-llm-usage--view-items state) nil
        (loki-assistant-llm-usage--view-cursor state) nil))

(defun loki-assistant-llm-usage-filter ()
  "设置当前视图的筛选条件，空值表示清除。"
  (interactive)
  (let* ((state (loki-assistant-llm-usage--state))
         (available (pcase loki-assistant-llm-usage--current
                      ('overview '(client_id upstream model protocol session_id dates bucket group_by))
                      ('history '(client_id upstream model protocol session_id dates outcome incomplete))
                      ('sessions '(client_id upstream session_id))
                      (_ (user-error "该视图没有筛选条件"))))
         (choices (mapcar (lambda (key) (cons (or (alist-get key loki-assistant-llm-usage--fields) "日期范围") key))
                          available))
         (key (cdr (assoc (completing-read "筛选条件：" choices nil t) choices)))
         (filters (copy-tree (loki-assistant-llm-usage--view-filters state))))
    (when (and (eq key 'session_id) (not (alist-get 'client_id filters)))
      (user-error "请先筛选客户端"))
    (cond
     ((eq key 'dates)
      (let ((from (read-string "起始日期 YYYY-MM-DD（留空清除）：" (alist-get 'from_date filters)))
            (to nil))
        (unless (string-empty-p from)
          (setq to (read-string "结束日期 YYYY-MM-DD（不含）：" (alist-get 'to_date filters)))
          (unless (and (loki-assistant-llm-usage--valid-date-p from)
                       (loki-assistant-llm-usage--valid-date-p to)
                       (string< from to))
            (user-error "请输入有效的日期区间")))
        (dolist (field '(from to from_date to_date)) (setq filters (assq-delete-all field filters)))
        (when to
          (if (eq loki-assistant-llm-usage--current 'overview)
              (setq filters (append filters (list (cons 'from_date from) (cons 'to_date to))))
            (setq filters (append filters (loki-assistant-llm-usage--history-filters
                                           (list (cons 'from_date from) (cons 'to_date to))))))))
      (loki-assistant-llm-usage--set-filters state filters)
      (loki-assistant-llm-usage--load))
     ((memq key '(client_id upstream model))
      (loki-assistant-llm-usage--choose-candidate state key filters))
     (t
      (let* ((values (alist-get key '((protocol "codex" "claude") (bucket "day" "month")
                                      (group_by "client_id" "upstream" "protocol" "model")
                                      (outcome "completed" "cancelled" "failed")
                                      (incomplete "true" "false"))))
             (prompt (concat (alist-get key loki-assistant-llm-usage--fields) "（留空清除）："))
             (value (if values (completing-read prompt (cons "" values) nil t)
                      (read-string prompt (alist-get key filters)))))
        (loki-assistant-llm-usage--apply-filter state filters key value))))))

(defun loki-assistant-llm-usage--apply-filter (state filters key value)
  (setq filters (assq-delete-all key filters))
  (when (eq key 'client_id)
    (setq filters (assq-delete-all 'session_id filters)))
  (unless (string-empty-p value) (push (cons key value) filters))
  (loki-assistant-llm-usage--set-filters state filters)
  (loki-assistant-llm-usage--load))

(defun loki-assistant-llm-usage--choose-candidate (state key filters)
  (let* ((buffer (current-buffer))
         (view loki-assistant-llm-usage--current)
         (prefix (read-string "候选前缀（留空列出全部候选）："))
         (context (loki-assistant-llm-usage--history-filters
                   (assq-delete-all 'session_id (assq-delete-all key (copy-tree filters)))))
         (params (append (list (cons 'dimension (symbol-name key)) (cons 'q prefix))
                         (if (eq view 'sessions)
                             (cons '(source . "sessions") context) context))))
    (loki-assistant-request
     "GET" "/llm/usage/options" :params (loki-assistant-llm-usage--params params)
     :callback
     (lambda (reply)
       (when (and (buffer-live-p buffer)
                  (eq view (buffer-local-value 'loki-assistant-llm-usage--current buffer)))
         (with-current-buffer buffer
           (let* ((data (plist-get reply :data))
                  (items (append (alist-get 'items data) nil))
                  (hint (cond ((not (plist-get reply :ok)) "候选不可用，可直接输入")
                              ((eq (alist-get 'has_more data) t) "可直接输入完整值")
                              (t "留空清除")))
                  (value (completing-read
                          (format "%s（%s）："
                                  (alist-get key loki-assistant-llm-usage--fields) hint)
                          (mapcar (lambda (item) (alist-get 'value item)) items)
                          nil nil nil nil (alist-get key filters))))
             (loki-assistant-llm-usage--apply-filter state filters key value))))))))

(defun loki-assistant-llm-usage-clear-filters ()
  "清除当前视图的筛选条件。"
  (interactive)
  (loki-assistant-llm-usage--set-filters (loki-assistant-llm-usage--state) nil)
  (loki-assistant-llm-usage--load))

(defun loki-assistant-llm-usage--item ()
  (let ((id (tabulated-list-get-id)))
    (or (and id (cl-find id (loki-assistant-llm-usage--view-items (loki-assistant-llm-usage--state))
                         :key #'loki-assistant-llm-usage--id :test #'equal))
        (user-error "请先选择一条记录"))))

(defun loki-assistant-llm-usage--drill (filters)
  (setq loki-assistant-llm-usage--context
        (cons loki-assistant-llm-usage--current (gethash 'history loki-assistant-llm-usage--views-state)))
  (puthash 'history (make-loki-assistant-llm-usage--view :filters filters)
           loki-assistant-llm-usage--views-state)
  (loki-assistant-llm-usage--switch 'history))

(defun loki-assistant-llm-usage-back ()
  "返回下钻前的视图和筛选条件。"
  (interactive)
  (if-let ((context loki-assistant-llm-usage--context))
      (progn
        (setq loki-assistant-llm-usage--context nil)
        (when (cdr context)
          (puthash 'history (cdr context) loki-assistant-llm-usage--views-state))
        (loki-assistant-llm-usage--switch (car context)))
    (quit-window)))

(defun loki-assistant-llm-usage-open ()
  "查看日期、会话或单条请求的详情。"
  (interactive)
  (let* ((row (loki-assistant-llm-usage--item))
         (state (loki-assistant-llm-usage--state)))
    (pcase loki-assistant-llm-usage--current
      ('overview
       (let* ((data (loki-assistant-llm-usage--view-data state))
              (period (alist-get 'period_start row))
              (monthly (equal (alist-get 'bucket data) "month"))
              (first (if (string< period (alist-get 'from_date data))
                         (alist-get 'from_date data) period))
              (last (format-time-string "%Y-%m-%d"
                                        (if monthly
                                            (let ((parts (parse-time-string (concat period "T00:00:00"))))
                                              (encode-time 0 0 0 1 (1+ (nth 4 parts)) (nth 5 parts) t))
                                          (time-add (date-to-time (concat period "T00:00:00Z"))
                                                    (days-to-time 1))) t))
              (filters (loki-assistant-llm-usage--history-filters
                        (loki-assistant-llm-usage--view-filters state))))
         (setq filters (assq-delete-all 'from (assq-delete-all 'to filters)))
         (when-let ((name (loki-assistant-llm-usage--value 'group_by data))
                    (value (loki-assistant-llm-usage--value 'group row)))
           (setf (alist-get (intern name) filters) value))
         (loki-assistant-llm-usage--drill
          (append filters
                  (list (cons 'from (loki-assistant-llm-usage--date-time first))
                        (cons 'to (loki-assistant-llm-usage--date-time
                                   (if (string< last (alist-get 'to_date data)) last
                                     (alist-get 'to_date data)))))))))
      ('sessions
       (loki-assistant-llm-usage--drill
        (mapcar (lambda (key) (assq key row)) '(client_id session_id upstream))))
      ('history
       (let ((origin (current-buffer))
             (id (alist-get 'id row)))
         (loki-assistant-request
          "GET" (concat "/llm/usage/requests/" (url-hexify-string id))
          :callback (lambda (reply)
                      (when (buffer-live-p origin)
                        (if (plist-get reply :ok)
                            (loki-assistant-llm-usage--detail (plist-get reply :data))
                          (message "%s" (or (plist-get reply :error-message) "读取请求详情失败"))))))))
      ('live (loki-assistant-llm-usage--detail (list (cons 'item row))))
      (_ (user-error "请从会话或请求历史查看明细")))))

(defun loki-assistant-llm-usage--detail (data)
  (let ((buffer (get-buffer-create "*LLM 请求详情*"))
        (item (alist-get 'item data)))
    (with-current-buffer buffer
      (special-mode)
      (let ((inhibit-read-only t)
            (json-null :null)
            (json-false :false))
        (erase-buffer)
        (insert (format "请求 %s\n\n" (loki-assistant-llm-usage--cell 'id item)))
        (cl-loop for (key . label) in '((client_id . "客户端") (session_id . "会话")
                                      (upstream . "供应商") (protocol . "协议") (model . "模型")
                                      (upstream_status . "HTTP 状态") (first_byte_ms . "首包 ms")
                                      (duration_ms . "耗时 ms"))
                 do (insert (format "%s  %s\n" label (loki-assistant-llm-usage--cell key item))))
        (when-let ((usage (loki-assistant-llm-usage--value 'usage item)))
          (insert "\n用量  " (loki-assistant-llm-usage--usage-label usage) "\n")
          (cl-loop for (key . label) in '((input_reported_tokens . "上游输入")
                                          (input_tokens . "输入") (input_uncached_tokens . "未缓存输入")
                                          (cache_read_tokens . "缓存读取") (cache_write_tokens . "缓存写入")
                                          (output_tokens . "输出"))
                   when (assq key usage)
                   do (insert (format "%s  %s\n" label
                                      (loki-assistant-llm-usage--cell key usage "未上报")))))
        (when (assq 'raw_usage data)
          (insert "\n原始用量\n")
          (if-let ((raw (loki-assistant-llm-usage--value 'raw_usage data)))
              (insert (json-encode raw))
            (insert "未上报"))
          (insert "\n"))
        (goto-char (point-min))))
    (if (>= (window-total-width) 110)
        (select-window (display-buffer-in-side-window buffer '((side . right) (window-width . 0.42))))
      (pop-to-buffer buffer))))

(defvar loki-assistant-llm-usage-mode-map
  (let ((map (make-sparse-keymap)))
    (define-key map (kbd "1") #'loki-assistant-llm-usage-overview)
    (define-key map (kbd "2") #'loki-assistant-llm-usage-history)
    (define-key map (kbd "3") #'loki-assistant-llm-usage-sessions)
    (define-key map (kbd "4") #'loki-assistant-llm-usage-totals)
    (define-key map (kbd "5") #'loki-assistant-llm-usage-live)
    (define-key map (kbd "g") #'loki-assistant-llm-usage-refresh)
    (define-key map (kbd "f") #'loki-assistant-llm-usage-filter)
    (define-key map (kbd "F") #'loki-assistant-llm-usage-clear-filters)
    (define-key map (kbd "l") #'loki-assistant-llm-usage-more)
    (define-key map (kbd "b") #'loki-assistant-llm-usage-back)
    (define-key map (kbd "RET") #'loki-assistant-llm-usage-open)
    map))

(define-derived-mode loki-assistant-llm-usage-mode tabulated-list-mode "LLM 用量"
  "查看用量概览、请求历史、会话、累计用量和实时请求。"
  (setq-local loki-assistant-llm-usage--views-state (make-hash-table :test #'eq))
  (setq-local tabulated-list-padding 2))

;;;###autoload
(defun loki-assistant-llm-usage ()
  "打开 LLM 用量查询界面。"
  (interactive)
  (let ((buffer (get-buffer-create "*LLM 用量*")))
    (with-current-buffer buffer
      (unless (derived-mode-p 'loki-assistant-llm-usage-mode)
        (loki-assistant-llm-usage-mode))
      (loki-assistant-llm-usage--render)
      (loki-assistant-llm-usage--load))
    (pop-to-buffer buffer)))

(provide 'loki-assistant-llm-usage)
;;; loki-assistant-llm-usage.el ends here
