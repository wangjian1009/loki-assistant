;;; loki-assistant-llm-usage.el --- 查询 LLM 用量 -*- lexical-binding: t; -*-

(require 'cl-lib)
(require 'calendar)
(require 'json)
(require 'rx)
(require 'subr-x)
(require 'svg)
(require 'tabulated-list)
(require 'parse-time)
(require 'loki-assistant)

(declare-function agent-shell-markdown-replace-markup "agent-shell-markdown")
(declare-function agent-shell-chat-boxed-label "agent-shell-chat-mode")
(declare-function org-mode "org")
(defvar agent-shell-markdown-render-functions)
(defvar org-inhibit-startup)
(defvar org-hide-emphasis-markers)
(defvar org-src-fontify-natively)

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

(defconst loki-assistant-llm-usage--chart-series
  '((input_uncached_tokens "未缓存输入" ?█ font-lock-function-name-face "#73b8ff")
    (cache_read_tokens "读缓存" ?▓ font-lock-type-face "#75d6b8")
    (cache_write_tokens "写缓存" ?▒ font-lock-constant-face "#ffbd75")
    (output_tokens "输出" ?░ font-lock-keyword-face "#cda8fa")))

(defconst loki-assistant-llm-usage--chart-width 32)

(cl-defstruct loki-assistant-llm-usage--view
  filters items data cursor selected loading error)

(defvar-local loki-assistant-llm-usage--views-state nil)
(defvar-local loki-assistant-llm-usage--current 'overview)
(defvar-local loki-assistant-llm-usage--zone "Asia/Shanghai")
(defvar-local loki-assistant-llm-usage--context nil)
(defvar-local loki-assistant-llm-usage--chart nil)
(defvar-local loki-assistant-llm-usage--detail-request nil)
(defvar-local loki-assistant-llm-usage--detail-windows nil)
(defvar-local loki-assistant-llm-usage--detail-id nil)

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

(defun loki-assistant-llm-usage--client-name (id data)
  (when (stringp id)
    (cdr (assoc-string id (alist-get 'client_names data)))))

(defun loki-assistant-llm-usage--client-label (item data)
  (or (loki-assistant-llm-usage--value 'client_name item)
      (loki-assistant-llm-usage--client-name (alist-get 'client_id item) data)
      "未知客户端"))

(defun loki-assistant-llm-usage--phase (value)
  (or (cdr (assoc value '(("waiting" . "等待响应") ("responding" . "接收响应")
                          ("completed" . "完成") ("failed" . "失败")
                          ("cancelled" . "取消"))))
      "—"))

(defun loki-assistant-llm-usage--usage-label (usage)
  (cond ((not (consp usage)) "—")
        ((eq (alist-get 'complete usage) t) "完整")
        (t "不完整")))

(defun loki-assistant-llm-usage--instruction-label (capture)
  (pcase (alist-get 'status capture)
    ("captured" (format "%s 段" (alist-get 'count capture)))
    ("reading" "读取中")
    ("absent" "未携带")
    (_ "未采集")))

(defun loki-assistant-llm-usage--time (value)
  (condition-case nil
      (let ((time (if (consp value)
                      (seconds-to-time (or (alist-get 'secs_since_epoch value) 0))
                    (and (stringp value) (parse-iso8601-time-string value)))))
        (if time (format-time-string "%Y-%m-%d %H:%M:%S" time
                                     loki-assistant-llm-usage--zone)
          "—"))
    (error (format "%s" value))))

(defun loki-assistant-llm-usage--format-count (count)
  (if count
      (reverse (string-remove-suffix
                "," (replace-regexp-in-string "[0-9]\\{3\\}" "\\&,"
                                              (reverse (number-to-string count)))))
    "—"))

(defun loki-assistant-llm-usage--count (key row)
  (loki-assistant-llm-usage--format-count (loki-assistant-llm-usage--value key row)))

(defun loki-assistant-llm-usage--chart-total (item)
  (cl-loop for (key) in loki-assistant-llm-usage--chart-series
           sum (or (loki-assistant-llm-usage--value key item) 0)))

(defun loki-assistant-llm-usage--chart-maximum (items)
  (cl-loop for item in items maximize (loki-assistant-llm-usage--chart-total item)))

(defun loki-assistant-llm-usage--chart-widths (item maximum width)
  (let ((cumulative 0) (previous 0))
    (cl-loop for (key) in loki-assistant-llm-usage--chart-series
             do (cl-incf cumulative (or (loki-assistant-llm-usage--value key item) 0))
             for end = (round (* cumulative width) (max 1 maximum))
             collect (- end previous)
             do (setq previous end))))

(defun loki-assistant-llm-usage--chart-bar (item maximum)
  (cl-loop for (key label glyph face) in loki-assistant-llm-usage--chart-series
           for width in (loki-assistant-llm-usage--chart-widths
                         item maximum loki-assistant-llm-usage--chart-width)
           concat (propertize (make-string width glyph)
                              'face face
                              'help-echo (format "%s %s token" label
                                                 (loki-assistant-llm-usage--count key item)))))

(defun loki-assistant-llm-usage--group-label (group-by)
  (cdr (assoc group-by '(("client_id" . "客户端") ("upstream" . "供应商")
                         ("protocol" . "协议") ("model" . "模型")))))

(defun loki-assistant-llm-usage--group-value (item data label)
  (if (equal (alist-get 'group_by data) "client_id")
      (or (loki-assistant-llm-usage--client-name (alist-get 'group item) data)
          "未知客户端")
    (loki-assistant-llm-usage--cell 'group item (concat "未知" label))))

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

(defun loki-assistant-llm-usage--row (item data chart-maximum)
  (cl-labels ((cell (key &optional missing) (loki-assistant-llm-usage--cell key item missing)))
    (pcase loki-assistant-llm-usage--current
      ('overview
       (let ((group-by (alist-get 'group_by data)))
         (vconcat
          (vector (if (equal (alist-get 'bucket data) "month")
                      (substring (cell 'period_start) 0 7)
                    (cell 'period_start)))
          (when-let ((label (loki-assistant-llm-usage--group-label group-by)))
            (vector (loki-assistant-llm-usage--group-value item data label)))
          (if chart-maximum
              (vector (loki-assistant-llm-usage--chart-bar item chart-maximum)
                      (loki-assistant-llm-usage--format-count
                       (loki-assistant-llm-usage--chart-total item))
                      (loki-assistant-llm-usage--count 'incomplete_requests item))
            (mapcar (lambda (key) (loki-assistant-llm-usage--count key item))
                    '(requests incomplete_requests input_uncached_tokens
                      cache_read_tokens cache_write_tokens output_tokens))))))
      ('history
       (vector (loki-assistant-llm-usage--time (alist-get 'finished_at item))
               (loki-assistant-llm-usage--client-label item data)
               (cell 'upstream) (cell 'model)
               (loki-assistant-llm-usage--phase (alist-get 'outcome item))
               (cell 'duration_ms)
               (loki-assistant-llm-usage--cell 'output_tokens (alist-get 'usage item) "未上报")
               (loki-assistant-llm-usage--usage-label (alist-get 'usage item))))
      ('live
       (let ((usage (loki-assistant-llm-usage--value 'usage item))
             (started (loki-assistant-llm-usage--time (alist-get 'started_at item))))
         (vector (if (string-match-p "^[0-9-]\\{10\\} " started)
                     (propertize (substring started 11) 'help-echo started)
                   started)
                 (loki-assistant-llm-usage--client-label item data)
                 (loki-assistant-llm-usage--phase (alist-get 'phase item))
                 (loki-assistant-llm-usage--instruction-label
                  (loki-assistant-llm-usage--value 'instructions item))
                 (cell 'upstream) (cell 'model)
                 (cell 'first_byte_ms) (cell 'duration_ms)
                 (loki-assistant-llm-usage--cell 'output_tokens usage)
                 (loki-assistant-llm-usage--usage-label usage))))
      (_
       (vconcat (when (eq loki-assistant-llm-usage--current 'sessions)
                  (vector (cell 'session_id)))
                (vector (loki-assistant-llm-usage--client-label item data))
                (mapcar #'cell '(upstream input_tokens output_tokens requests incomplete_requests)))))))

(defun loki-assistant-llm-usage--id (item)
  (or (alist-get 'id item)
      (mapcar (lambda (key) (alist-get key item))
              (if (eq loki-assistant-llm-usage--current 'overview)
                  '(period_start group)
                '(client_id session_id upstream)))))

(defun loki-assistant-llm-usage--insert-filters (filters data)
  (when filters
    (insert "筛选  "
            (mapconcat
             (lambda (pair)
               (format "%s %s" (alist-get (car pair) loki-assistant-llm-usage--fields)
                       (if (eq (car pair) 'client_id)
                           (loki-assistant-llm-usage--client-label (list pair) data)
                         (cdr pair))))
             filters " · ")
            "\n")))

(defun loki-assistant-llm-usage--report-range (data)
  (let* ((from (loki-assistant-llm-usage--value 'from_date data))
         (to (loki-assistant-llm-usage--value 'to_date data)))
    (when (and from to)
      (let ((last (format-time-string
                   "%Y-%m-%d"
                   (date-to-time (loki-assistant-llm-usage--date-time to -1))
                   loki-assistant-llm-usage--zone)))
        (if (equal from last) from (concat from " 至 " last "（含首尾）"))))))

(defun loki-assistant-llm-usage--insert-overview (data filters has-rows chart-maximum)
  (let ((start (loki-assistant-llm-usage--value 'data_from data))
        (totals (alist-get 'totals data)))
    (when-let ((range (loki-assistant-llm-usage--report-range data)))
        (insert (propertize "统计范围  " 'face 'bold)
                range "  ·  " loki-assistant-llm-usage--zone "\n")
      (when (and start (time-less-p (date-to-time (loki-assistant-llm-usage--date-time
                                                  (alist-get 'from_date data)))
                                   (date-to-time start)))
        (insert (propertize (concat "明细采集开始于 "
                                    (loki-assistant-llm-usage--time start) "\n")
                            'face 'shadow))))
    (loki-assistant-llm-usage--insert-filters
     (cl-remove-if (lambda (pair) (memq (car pair) '(from_date to_date bucket group_by))) filters)
     data)
    (when totals
      (insert "\n" (propertize "区间汇总" 'face 'bold) "\n"
              (format "  请求 %s 次  ·  用量不完整 %s 次\n"
                      (loki-assistant-llm-usage--count 'requests totals)
                      (loki-assistant-llm-usage--count 'incomplete_requests totals))
              (format "  输入 token  未缓存 %s  ·  读缓存 %s  ·  写缓存 %s\n"
                      (loki-assistant-llm-usage--count 'input_uncached_tokens totals)
                      (loki-assistant-llm-usage--count 'cache_read_tokens totals)
                      (loki-assistant-llm-usage--count 'cache_write_tokens totals))
              (format "  输出 token  %s\n"
                      (loki-assistant-llm-usage--count 'output_tokens totals)))
      (when (> (or (alist-get 'incomplete_requests totals) 0) 0)
        (insert (propertize "  汇总只计入已上报的 token。\n" 'face 'shadow))))
    (when (and has-rows chart-maximum)
      (insert "\n" (propertize (if (equal (alist-get 'bucket data) "month")
                                   "按月用量图" "按日用量图") 'face 'bold)
              "  ·  满格 " (loki-assistant-llm-usage--format-count chart-maximum) " token\n  ")
      (cl-loop for (_ label glyph face) in loki-assistant-llm-usage--chart-series
               do (insert (propertize (string glyph) 'face face) " " label "  "))
      (insert "\n"))
    (when (and has-rows (not chart-maximum))
      (insert "\n" (propertize (if (equal (alist-get 'bucket data) "month")
                                     "按月明细" "按日明细")
                                 'face 'bold)
              "  ·  用量单位为 token\n"))))

(defun loki-assistant-llm-usage--render ()
  (let* ((state (loki-assistant-llm-usage--state))
         (title (nth 1 (assq loki-assistant-llm-usage--current loki-assistant-llm-usage--views)))
         (data (loki-assistant-llm-usage--view-data state))
         (items (loki-assistant-llm-usage--view-items state))
         (selected (or (tabulated-list-get-id)
                       (loki-assistant-llm-usage--view-selected state)))
         (overview (eq loki-assistant-llm-usage--current 'overview))
         (chart-maximum (when (and overview loki-assistant-llm-usage--chart)
                          (loki-assistant-llm-usage--chart-maximum items)))
         (filters (loki-assistant-llm-usage--view-filters state)))
    (setq-local tabulated-list-use-header-line (not overview))
    (setq-local header-line-format nil)
    (setq tabulated-list-format
          (pcase loki-assistant-llm-usage--current
            ('overview
             (vconcat
              (vector (list (if (equal (alist-get 'bucket data) "month") "月份" "日期") 10 nil))
              (when-let ((label (loki-assistant-llm-usage--group-label (alist-get 'group_by data))))
                (vector (list label 14 nil)))
              (if chart-maximum
                  (vector (list "用量" loki-assistant-llm-usage--chart-width nil)
                          '("合计 token" 16 nil :right-align t)
                          '("用量不完整" 10 nil :right-align t))
                [("请求数" 8 nil :right-align t) ("用量不完整" 10 nil :right-align t)
                 ("未缓存输入" 12 nil :right-align t) ("读缓存" 11 nil :right-align t)
                 ("写缓存" 11 nil :right-align t) ("输出" 11 nil :right-align t)])))
            ('history [("完成时间" 20 nil) ("客户端" 18 nil) ("供应商" 15 nil) ("模型" 20 nil)
                       ("结果" 8 nil) ("耗时 ms" 10 nil) ("输出" 12 nil) ("用量" 8 nil)])
            ('live [("开始时间" 9 nil) ("客户端" 14 nil) ("状态" 10 nil) ("系统指令" 9 nil)
                    ("供应商" 15 nil) ("模型" 20 nil) ("首包 ms" 10 nil) ("耗时 ms" 10 nil)
                    ("输出" 12 nil) ("用量" 8 nil)])
            (_ (vconcat (when (eq loki-assistant-llm-usage--current 'sessions)
                          [("会话" 26 nil)])
                        [("客户端" 18 nil) ("供应商" 17 nil) ("上游输入" 16 nil)
                         ("输出" 12 nil) ("请求" 8 nil) ("不完整" 8 nil)])))
          tabulated-list-entries
          (mapcar (lambda (item)
                    (list (loki-assistant-llm-usage--id item)
                          (loki-assistant-llm-usage--row item data chart-maximum))) items))
    (when (or (not overview) items)
      (tabulated-list-init-header))
    (when (and overview (not items))
      (setq-local tabulated-list--header-string nil))
    (tabulated-list-print t)
    (when overview
      (let ((inhibit-read-only t)
            (header (tabulated-list-header-overlay-p)))
        (save-excursion
          (goto-char (point-min))
          (loki-assistant-llm-usage--insert-overview data filters items chart-maximum)
          (when header
            (move-overlay header (point) (overlay-end header))))))
    (when selected
      (goto-char (point-min))
      (while (and (not (eobp)) (not (equal selected (tabulated-list-get-id))))
        (forward-line 1))
      (when (eobp) (goto-char (point-min))))
    (setq-local frame-title-format (concat "LLM 用量 · " title))
    (setq-local mode-line-process
                (list " · " title
                      (loki-assistant--mode-line-keys
                       (concat "1-5 切换  g 刷新"
                               (unless (eq loki-assistant-llm-usage--current 'totals)
                                 "  RET 打开")
                               (when (memq loki-assistant-llm-usage--current '(overview history sessions))
                                 "  f 筛选  F 清除")
                               (when overview (if loki-assistant-llm-usage--chart
                                                  "  t 表格" "  t 图表"))
                               (when (and overview items) "  e 导出 PNG")
                               (when loki-assistant-llm-usage--context "  b 返回")))
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
        (unless overview
          (loki-assistant-llm-usage--insert-filters filters data))))
    (set-buffer-modified-p nil)))

(defun loki-assistant-llm-usage--switch (view)
  (setf (loki-assistant-llm-usage--view-selected (loki-assistant-llm-usage--state))
        (tabulated-list-get-id))
  (setq loki-assistant-llm-usage--current view
        loki-assistant-llm-usage--detail-request nil)
  (let ((inhibit-read-only t)) (erase-buffer))
  (loki-assistant-llm-usage--render)
  (loki-assistant-llm-usage--load))

(defun loki-assistant-llm-usage-overview () "打开用量概览。" (interactive) (loki-assistant-llm-usage--switch 'overview))
(defun loki-assistant-llm-usage-toggle-chart ()
  "在用量概览中切换表格和图表。"
  (interactive)
  (unless (eq loki-assistant-llm-usage--current 'overview)
    (user-error "请在概览中切换图表"))
  (setq loki-assistant-llm-usage--chart (not loki-assistant-llm-usage--chart))
  (loki-assistant-llm-usage--render))

(defun loki-assistant-llm-usage--chart-svg (data items filters)
  (let* ((maximum (loki-assistant-llm-usage--chart-maximum items))
         (monthly (equal (alist-get 'bucket data) "month"))
         (group (loki-assistant-llm-usage--group-label (alist-get 'group_by data)))
         (filter-label (with-temp-buffer
                         (loki-assistant-llm-usage--insert-filters
                          (cl-remove-if (lambda (pair)
                                          (memq (car pair) '(from_date to_date bucket group_by)))
                                        filters)
                          data)
                         (string-trim (buffer-string))))
         (legend-y (if (string-empty-p filter-label) 87 108))
         (first-row-y (+ legend-y 55))
         (height (+ first-row-y (* 28 (length items))))
         (bar-start (if group 316 130))
         (bar-width (- 782 bar-start))
         (svg (svg-create 1040 height)))
    (cl-labels ((label (value x y &optional size anchor)
                  (svg-text svg value :x x :y y :fill "#e6edf3"
                            :font-family "Noto Sans SC, sans-serif"
                            :font-size (or size 14) :text-anchor (or anchor "start"))))
      (svg-rectangle svg 0 0 1040 height :fill "#161b22")
      (label (if monthly "LLM 每月用量" "LLM 每日用量") 24 34 22)
      (label (concat (loki-assistant-llm-usage--report-range data)
                     "  ·  " loki-assistant-llm-usage--zone) 24 60)
      (unless (string-empty-p filter-label)
        (label (truncate-string-to-width filter-label 118 nil nil "…") 24 81))
      (cl-loop for (_ name _glyph _face color) in loki-assistant-llm-usage--chart-series
               for x from 24 by 132
               do (svg-rectangle svg x (- legend-y 11) 12 12 :fill color)
               do (label name (+ x 20) legend-y))
      (label (format "满格 %s token" (loki-assistant-llm-usage--format-count maximum))
             1016 legend-y 14 "end")
      (let ((header-y (- first-row-y 28)))
        (label (if monthly "月份" "日期") 24 header-y)
        (when group (label group 130 header-y))
        (label "合计 token" 912 header-y 14 "end")
        (label "用量不完整" 1016 header-y 14 "end"))
      (cl-loop for item in items
               for y from first-row-y by 28
               for period = (loki-assistant-llm-usage--cell 'period_start item)
               do (label (if monthly (substring period 0 7) period) 24 y)
               do (when group
                    (label (truncate-string-to-width
                            (loki-assistant-llm-usage--group-value item data group) 23 nil nil "…")
                           130 y))
               do (let ((x bar-start))
                    (cl-loop for (_key _label _glyph _face color) in loki-assistant-llm-usage--chart-series
                             for width in (loki-assistant-llm-usage--chart-widths item maximum bar-width)
                             when (> width 0)
                             do (svg-rectangle svg x (- y 12) width 14 :fill color)
                             do (cl-incf x width)))
               do (label (loki-assistant-llm-usage--format-count
                          (loki-assistant-llm-usage--chart-total item)) 912 y 14 "end")
               do (label (loki-assistant-llm-usage--count 'incomplete_requests item)
                         1016 y 14 "end")))
    svg))

(defun loki-assistant-llm-usage-export-png (&optional file)
  "将当前用量概览绘制为 PNG。交互调用时选择输出文件。"
  (interactive)
  (unless (eq loki-assistant-llm-usage--current 'overview)
    (user-error "请在用量概览中导出 PNG"))
  (let* ((state (loki-assistant-llm-usage--state))
         (items (loki-assistant-llm-usage--view-items state))
         (data (loki-assistant-llm-usage--view-data state))
         (filters (loki-assistant-llm-usage--view-filters state))
         (converter (executable-find "rsvg-convert")))
    (unless items (user-error "当前概览没有可导出的用量记录"))
    (unless converter (user-error "导出 PNG 需要 rsvg-convert"))
    (let* ((local-directory (if (file-remote-p default-directory) "~/" default-directory))
           (name (expand-file-name (or file (read-file-name "导出 PNG 到文件：" local-directory
                                                         nil nil "llm-usage.png"))
                                   local-directory))
           (target (if (string-suffix-p ".png" name t) name (concat name ".png"))))
      (when (or (file-remote-p target) (directory-name-p name) (file-directory-p name))
        (user-error "请选择本机 PNG 文件路径"))
      (let ((temporary (make-nearby-temp-file
                        (expand-file-name ".llm-usage-" (file-name-directory target)) nil ".png"))
            (default-directory local-directory))
        (unwind-protect
            (progn
              (with-temp-buffer
                (let ((coding-system-for-write 'utf-8-unix))
                  (svg-print (loki-assistant-llm-usage--chart-svg data items filters))
                  (let ((start (point-max)))
                    (unless (eq 0 (call-process-region (point-min) start converter
                                                       nil (list t t) nil "-o" temporary))
                      (user-error "PNG 转换失败：%s"
                                  (string-trim (buffer-substring-no-properties start (point-max))))))))
              (rename-file temporary target (and (called-interactively-p 'interactive) 1))
              (message "已导出 PNG 到 %s" target)
              target)
          (when (file-exists-p temporary) (delete-file temporary)))))))

(defun loki-assistant-llm-usage-history () "打开请求历史。" (interactive) (loki-assistant-llm-usage--switch 'history))
(defun loki-assistant-llm-usage-sessions () "打开会话用量。" (interactive) (loki-assistant-llm-usage--switch 'sessions))
(defun loki-assistant-llm-usage-totals () "打开全量累计用量。" (interactive) (loki-assistant-llm-usage--switch 'totals))
(defun loki-assistant-llm-usage-live ()
  "打开实时请求快照。"
  (interactive)
  (loki-assistant-llm-usage--switch 'live))
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

(defun loki-assistant-llm-usage--choose-client (items data selected)
  (let* ((partial (eq (alist-get 'has_more data) t))
         (ids (mapcar (lambda (item) (alist-get 'value item)) items))
         (unknown 0))
    (when partial
      (setq ids (delete-dups
                 (append ids (mapcar (lambda (entry) (format "%s" (car entry)))
                                     (alist-get 'client_names data))))))
    (when selected (cl-pushnew selected ids :test #'equal))
    (let* ((choices (mapcar (lambda (id)
                              (cons (or (loki-assistant-llm-usage--client-name id data)
                                        (format "未知客户端（%d）" (cl-incf unknown)))
                                    id))
                            ids))
           (choice (completing-read "客户端（留空清除）：" (cons '("" . "") choices)
                                    nil (not partial) nil nil (car (rassoc selected choices)))))
      (or (cdr (assoc choice choices)) choice))))

(defun loki-assistant-llm-usage--choose-candidate (state key filters)
  (let* ((buffer (current-buffer))
         (view loki-assistant-llm-usage--current)
         (client-filter (eq key 'client_id))
         (context (loki-assistant-llm-usage--history-filters
                   (assq-delete-all 'session_id (assq-delete-all key (copy-tree filters)))))
         (params (append (list (cons 'dimension (symbol-name key))
                               (if client-filter
                                   '(limit . "100")
                                 (cons 'q (read-string "候选前缀（留空列出全部候选）："))))
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
                  (value (if client-filter
                             (if (plist-get reply :ok)
                                 (loki-assistant-llm-usage--choose-client items data (alist-get key filters))
                               (user-error "%s" (or (plist-get reply :error-message) "读取客户端候选失败")))
                           (completing-read
                            (format "%s（%s）：" (alist-get key loki-assistant-llm-usage--fields) hint)
                            (mapcar (lambda (item) (alist-get 'value item)) items)
                            nil nil nil nil (alist-get key filters)))))
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

(defun loki-assistant-llm-usage--open-request (row state live)
  (let* ((origin (current-buffer))
         (window (selected-window))
         (request (list nil))
         (path (concat (if live "/llm/requests/" "/llm/usage/requests/")
                       (url-hexify-string (alist-get 'id row)))))
    (setq loki-assistant-llm-usage--detail-request request)
    (loki-assistant-request
     "GET" path
     :callback
     (lambda (reply)
       (when (and (buffer-live-p origin) (eq (selected-window) window)
                  (eq (window-buffer window) origin)
                  (eq request (buffer-local-value 'loki-assistant-llm-usage--detail-request origin)))
         (if (plist-get reply :ok)
             (loki-assistant-llm-usage--detail (plist-get reply :data) live)
           (when live
             (loki-assistant-llm-usage--detail
              (list (cons 'item (cons '(instructions (status . "unavailable")) row))
                    (cons 'client_names (alist-get 'client_names
                                                   (loki-assistant-llm-usage--view-data state))))
              t))
           (message "%s" (or (plist-get reply :error-message) "读取请求详情失败"))))))))

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
      ('history (loki-assistant-llm-usage--open-request row state nil))
      ('live (loki-assistant-llm-usage--open-request row state t))
      (_ (user-error "请从会话或请求历史查看明细")))))

(defvar loki-assistant-llm-usage-detail-mode-map
  (let ((map (make-sparse-keymap)))
    (define-key map (kbd "C-c C-q") #'loki-assistant-llm-usage-close-detail)
    (define-key map (kbd "q") #'loki-assistant-llm-usage-close-detail)
    (define-key map (kbd "g") #'loki-assistant-llm-usage-refresh-detail)
    (define-key map (kbd "j") #'loki-assistant-llm-usage-jump-detail)
    (define-key map (kbd "?") #'describe-mode)
    (define-key map (kbd "TAB") #'loki-assistant-llm-usage-next-item)
    (define-key map (kbd "<tab>") #'loki-assistant-llm-usage-next-item)
    (define-key map (kbd "n") #'loki-assistant-llm-usage-next-item)
    (define-key map (kbd "<backtab>") #'loki-assistant-llm-usage-previous-item)
    (define-key map (kbd "S-<tab>") #'loki-assistant-llm-usage-previous-item)
    (define-key map (kbd "p") #'loki-assistant-llm-usage-previous-item)
    (define-key map (kbd "C-M-u") #'loki-assistant-llm-usage-parent-item)
    (define-key map (kbd "RET") #'loki-assistant-llm-usage-fold-instruction)
    (define-key map (kbd "<return>") #'loki-assistant-llm-usage-fold-instruction)
    (define-key map (kbd "w") #'loki-assistant-llm-usage-copy-instruction)
    map))

(define-derived-mode loki-assistant-llm-usage-detail-mode special-mode "LLM 请求详情"
  "查看实时请求的对话、请求上下文和工具定义。
Tab/n 和 Shift-Tab/p 跳到相邻可见项目，C-M-u 回到上一级。
RET 折叠或展开当前项目，C-s 搜索，w 复制当前内容。
实时请求可按 j 跳到章节，按 g 刷新。
按 q 或 C-c C-q 返回，按 ? 查看模式帮助。"
  (require 'agent-shell-faces nil t)
  (require 'agent-shell-chat-mode nil t)
  (require 'agent-shell-markdown nil t)
  (setq-local truncate-lines nil)
  (add-to-invisibility-spec 'loki-assistant-llm-usage-instruction)
  (add-to-invisibility-spec 'org-link)
  (setq-local mode-line-process (loki-assistant--mode-line-keys nil)))

(defun loki-assistant-llm-usage--detail-face (face fallback)
  (if (facep face) face fallback))

(defun loki-assistant-llm-usage--speaker-label (title role)
  (if (fboundp 'agent-shell-chat-boxed-label)
      (agent-shell-chat-boxed-label (list (cons :text title) (cons :role role)))
    (propertize (format " %s " title)
                'face (list :inherit
                            (if (eq role 'user)
                                '(bold font-lock-keyword-face)
                              '(bold font-lock-function-name-face))
                            :inverse-video t :box t))))

(defun loki-assistant-llm-usage--fold-instruction (body hide)
  (overlay-put body 'invisible (when hide 'loki-assistant-llm-usage-instruction))
  (let ((indicator (+ (overlay-start (overlay-get body 'heading))
                      (length (overlay-get body 'indent)))))
    (with-silent-modifications
      (put-text-property indicator (1+ indicator) 'display (if hide "▶" "▼")))))

(defun loki-assistant-llm-usage--reveal-instruction (body)
  (while body
    (loki-assistant-llm-usage--fold-instruction body nil)
    (setq body (overlay-get body 'parent-fold))))

(defun loki-assistant-llm-usage--instruction-at-point ()
  (or (get-text-property (point) 'loki-assistant-llm-usage-instruction)
      (user-error "请把光标移到一段内容上")))

(defun loki-assistant-llm-usage-fold-instruction ()
  "折叠或展开当前段内容。"
  (interactive)
  (let* ((body (loki-assistant-llm-usage--instruction-at-point))
         (hide (not (overlay-get body 'invisible))))
    (when hide (goto-char (overlay-start (overlay-get body 'heading))))
    (loki-assistant-llm-usage--fold-instruction body hide)))

(defun loki-assistant-llm-usage-copy-instruction ()
  "复制当前项目的正文。"
  (interactive)
  (let ((body (loki-assistant-llm-usage--instruction-at-point)))
    (kill-new (or (overlay-get body 'text)
                  (buffer-substring-no-properties (overlay-start body) (overlay-end body)))))
  (message "已复制当前内容"))

(defun loki-assistant-llm-usage--move-item (forward)
  (let ((target
         (save-excursion
           (let ((property 'loki-assistant-llm-item-heading)
                 (search (if forward #'text-property-search-forward
                           #'text-property-search-backward))
                 match)
             (when-let ((body (get-text-property (point) property)))
               (let ((heading (overlay-get body 'heading)))
                 (goto-char (if forward (overlay-end heading) (overlay-start heading)))))
             (while (and (setq match (funcall search property nil (lambda (_ value) value)))
                         (invisible-p (prop-match-beginning match))))
             (and match (prop-match-beginning match))))))
    (if target (goto-char target) (user-error "没有更多可见项目"))))

(defun loki-assistant-llm-usage-next-item ()
  "跳到下一个可见项目。"
  (interactive)
  (loki-assistant-llm-usage--move-item t))

(defun loki-assistant-llm-usage-previous-item ()
  "跳到上一个可见项目。"
  (interactive)
  (loki-assistant-llm-usage--move-item nil))

(defun loki-assistant-llm-usage-parent-item ()
  "跳到当前项目的上一级。"
  (interactive)
  (if-let ((parent (overlay-get (loki-assistant-llm-usage--instruction-at-point) 'parent-fold)))
      (goto-char (overlay-start (overlay-get parent 'heading)))
    (user-error "当前项目已经在顶层")))

(defun loki-assistant-llm-usage-close-detail ()
  "返回请求列表并恢复原窗口布局。"
  (interactive)
  (if-let ((windows loki-assistant-llm-usage--detail-windows))
      (progn
        (setq loki-assistant-llm-usage--detail-windows nil
              loki-assistant-llm-usage--detail-request nil)
        (set-window-configuration windows))
    (quit-window)))

(defun loki-assistant-llm-usage-refresh-detail ()
  "读取当前实时请求的最新响应。"
  (interactive)
  (unless loki-assistant-llm-usage--detail-id
    (user-error "请在实时请求详情中刷新"))
  (let ((buffer (current-buffer))
        (window (selected-window))
        (id loki-assistant-llm-usage--detail-id)
        (request (list nil)))
    (setq loki-assistant-llm-usage--detail-request request)
    (loki-assistant-request
     "GET" (concat "/llm/requests/" (url-hexify-string id))
     :callback (lambda (reply)
                 (when (and (eq window (selected-window))
                            (eq (window-buffer window) buffer)
                            (eq request (buffer-local-value
                                         'loki-assistant-llm-usage--detail-request buffer)))
                   (if (plist-get reply :ok)
                       (let ((line (with-current-buffer buffer (line-number-at-pos))))
                         (loki-assistant-llm-usage--detail (plist-get reply :data) t)
                         (forward-line (1- line)))
                     (message "%s" (or (plist-get reply :error-message) "刷新请求失败"))))))))

(defvar loki-assistant-llm-usage--enclosing-fold nil
  "渲染嵌套项目时包含它的折叠区。")

(defun loki-assistant-llm-usage--insert-fold (title hide render &optional face)
  (let* ((start (point))
         (parent loki-assistant-llm-usage--enclosing-fold)
         (indent (if parent (concat (overlay-get parent 'indent) "  ") ""))
         (label (copy-sequence title)))
    (add-face-text-property 0 (length label)
                            (or face (loki-assistant-llm-usage--detail-face 'agent-shell-secondary 'shadow))
                            t label)
    (insert indent (propertize "▶" 'face 'default) " " label "\n")
    (let* ((body-start (point))
           (body (make-overlay body-start body-start))
           (heading (make-overlay start body-start)))
      (overlay-put body 'indent indent)
      (overlay-put body 'heading heading)
      (overlay-put body 'parent-fold parent)
      (overlay-put body 'isearch-open-invisible #'loki-assistant-llm-usage--reveal-instruction)
      (add-text-properties start body-start
                           (list 'loki-assistant-llm-item-heading body
                                 'loki-assistant-llm-usage-instruction body))
      (let ((loki-assistant-llm-usage--enclosing-fold body))
        (funcall render))
      (move-overlay body body-start (point))
      (loki-assistant-llm-usage--fold-instruction body hide)
      body)))

(defun loki-assistant-llm-usage--insert-block (title text hide &optional face rendered)
  (let ((body (loki-assistant-llm-usage--insert-fold
               title hide
               (lambda ()
                 (let ((start (point))
                       (indent (concat (overlay-get loki-assistant-llm-usage--enclosing-fold
                                                   'indent)
                                       "  ")))
                   (insert (or rendered text))
                   (unless (bolp) (insert "\n"))
                   ;; 行首的显示前缀会在隐藏正文后作用到下一条可见标题。
                   (remove-text-properties start (point) '(line-prefix nil wrap-prefix nil))
                   (let ((end (copy-marker (point) t)))
                     (save-excursion
                       (goto-char start)
                       (while (< (point) end)
                         (unless (eolp) (insert indent))
                         (forward-line 1)))
                     (set-marker end nil)))
                 (insert "\n"))
               face)))
    (overlay-put body 'text text)
    (put-text-property (overlay-start body) (overlay-end body)
                       'loki-assistant-llm-usage-instruction body)))

(defun loki-assistant-llm-usage--render-markdown (text)
  (when (fboundp 'agent-shell-markdown-replace-markup)
    (with-temp-buffer
      (let ((agent-shell-markdown-render-functions nil))
        (insert text)
        (condition-case nil
            (progn
              (agent-shell-markdown-replace-markup :force t :render-images nil :highlight-blocks nil)
              (buffer-string))
          (error nil))))))

(defun loki-assistant-llm-usage--org-output (text)
  (let* ((case-fold-search t)
         (fence (make-string 3 #x60))
         (start (when (string-match (concat (rx string-start (* (any " \t\n")))
                                           fence (rx "org" (* (any " \t")) "\n"))
                                    text)
                  (match-end 0)))
         (end (and start (string-match (concat "^" fence (rx (* (any " \t\n")) string-end))
                                       text start))))
    (cond ((and start end) (substring text start end))
          ((and (not (string-match-p fence text))
                (string-match-p
                 (rx (or (seq line-start "#+" (+ (any alnum "_-")))
                         (seq line-start "**" (* "*") " " (+ (not (any "*\n"))) line-end)
                         (seq "[[" (+ (not (any "]\n"))) "][")))
                 text))
           text))))

(defun loki-assistant-llm-usage--render-assistant (text)
  (let ((org-text (loki-assistant-llm-usage--org-output text)))
    (or (when (and org-text (require 'org nil t))
          (with-temp-buffer
            (let ((org-inhibit-startup t)
                  (org-hide-emphasis-markers t)
                  (org-src-fontify-natively nil))
              (delay-mode-hooks (org-mode))
              (insert org-text)
              (font-lock-ensure)
              (buffer-string))))
        (loki-assistant-llm-usage--render-markdown text))))

(defun loki-assistant-llm-usage--insert-instructions (capture)
  (let ((blocks (vconcat (loki-assistant-llm-usage--value 'blocks capture))))
    (if (seq-empty-p blocks)
        (insert "  系统指令 · " (loki-assistant-llm-usage--instruction-label capture) "\n")
      (loki-assistant-llm-usage--insert-fold
       (format "系统指令 · %d 段" (length blocks)) t
       (lambda ()
         (cl-loop for block across blocks
                  for source = (alist-get 'source block)
                  for role = (loki-assistant-llm-usage--value 'role block)
                  do (loki-assistant-llm-usage--insert-block
                      (if role (concat source " · " role) source)
                      (alist-get 'text block) t)))))))

(defun loki-assistant-llm-usage--pretty-json (text)
  (with-temp-buffer
    (insert text)
    (condition-case nil
        (progn (json-pretty-print-buffer) (buffer-string))
      (error text))))

(defun loki-assistant-llm-usage--insert-transcript-block (block &optional hide-message)
  (let* ((kind (alist-get 'kind block))
         (name (loki-assistant-llm-usage--value 'name block))
         (id (loki-assistant-llm-usage--value 'id block))
         (text (or (alist-get 'text block) ""))
         (role (alist-get 'role block))
         (title (pcase kind
                  ("message" (pcase role
                               ("user" (loki-assistant-llm-usage--speaker-label "用户" 'user))
                               ("assistant" (loki-assistant-llm-usage--speaker-label "助手" 'agent))
                               (role (or role "消息"))))
                  ("tool_call" "工具调用")
                  ("tool_result" "工具结果")
                  ("error" "上游错误")
                  (_ "内容"))))
    (loki-assistant-llm-usage--insert-block
     (string-join (delq nil (list title name id
                                 (pcase kind
                                   ("tool_call" (if (string-empty-p text) "无参数" "参数"))
                                   ("tool_result" "结果正文"))))
                  " · ")
     (if (string-empty-p text) "无已采集正文" text)
     (or (member kind '("tool_call" "tool_result"))
         (and hide-message (equal kind "message")))
     (when (equal kind "error")
       (loki-assistant-llm-usage--detail-face 'agent-shell-error 'error))
     (cond ((and (equal kind "tool_call") (not (string-empty-p text)))
            (loki-assistant-llm-usage--pretty-json text))
           ((and (equal role "assistant") (not (string-empty-p text)))
            (loki-assistant-llm-usage--render-assistant text))))))

(defun loki-assistant-llm-usage--tool-activity-p (block)
  (member (alist-get 'kind block) '("tool_call" "tool_result")))

(defun loki-assistant-llm-usage--transcript-counts (blocks)
  (let ((users 0) (assistants 0) (calls 0) (results 0))
    (dolist (block blocks)
      (pcase (alist-get 'kind block)
        ("message" (pcase (alist-get 'role block)
                     ("user" (cl-incf users))
                     ("assistant" (cl-incf assistants))))
        ("tool_call" (cl-incf calls))
        ("tool_result" (cl-incf results))))
    (delq nil (list (when (> users 0) (format "%d 条用户消息" users))
                    (when (> assistants 0) (format "%d 条助手消息" assistants))
                    (when (> calls 0) (format "%d 次工具调用" calls))
                    (when (> results 0) (format "%d 条工具结果" results))))))

(defun loki-assistant-llm-usage--insert-dialogue (blocks &optional history)
  (while blocks
    (if (loki-assistant-llm-usage--tool-activity-p (car blocks))
        (let* ((end (cl-position-if-not #'loki-assistant-llm-usage--tool-activity-p blocks))
               (activity (if end (seq-take blocks end) blocks))
               (names (delete-dups (delq nil (mapcar (lambda (block) (alist-get 'name block))
                                                    activity))))
               (summary (append (loki-assistant-llm-usage--transcript-counts activity)
                                (when names
                                  (list (concat (string-join (seq-take names 3) "、")
                                                (when (> (length names) 3) "等")))))))
          (loki-assistant-llm-usage--insert-fold
           (string-join (cons "工具活动" summary) " · ") t
           (lambda ()
             (dolist (block activity)
               (loki-assistant-llm-usage--insert-transcript-block block))))
          (setq blocks (and end (nthcdr end blocks))))
      (loki-assistant-llm-usage--insert-transcript-block (pop blocks) history))))

(defun loki-assistant-llm-usage--user-message-p (block)
  (and (equal (alist-get 'kind block) "message")
       (equal (alist-get 'role block) "user")))

(defun loki-assistant-llm-usage--insert-section (name &optional summary)
  (insert "\n" (propertize name 'face 'bold 'loki-assistant-llm-section name))
  (when summary
    (insert " · " (propertize summary 'face (loki-assistant-llm-usage--detail-face
                                            'agent-shell-secondary 'shadow))))
  (insert "\n"))

(defun loki-assistant-llm-usage-jump-detail ()
  "跳到当前请求详情的某一节。"
  (interactive)
  (let ((position (point-min)) sections)
    (while (< position (point-max))
      (when-let ((section (get-text-property position 'loki-assistant-llm-section)))
        (push (cons section position) sections))
      (setq position (next-single-property-change
                      position 'loki-assistant-llm-section nil (point-max))))
    (unless sections (user-error "当前详情没有可跳转的章节"))
    (setq sections (nreverse sections))
    (goto-char (cdr (assoc (completing-read "跳到：" sections nil t) sections)))))

(defun loki-assistant-llm-usage--insert-transcript (item)
  (let* ((input (loki-assistant-llm-usage--value 'request_content item))
         (response (loki-assistant-llm-usage--value 'response_content item))
         (tools (vconcat (alist-get 'tools input)))
         (incoming (append (alist-get 'blocks input) nil))
         (outgoing (append (alist-get 'blocks response) nil))
         (settings (loki-assistant-llm-usage--value 'settings input)))
    (loki-assistant-llm-usage--insert-section "对话")
    (if (null incoming)
        (insert (if input "请求中无文本或工具消息\n" "请求输入未采集\n"))
      (let ((latest (or (cl-position-if #'loki-assistant-llm-usage--user-message-p incoming :from-end t)
                        (1- (length incoming)))))
        (while (and (> latest 0)
                    (loki-assistant-llm-usage--user-message-p (nth (1- latest) incoming)))
          (cl-decf latest))
        (let ((history (seq-take incoming latest)))
          (when history
            (loki-assistant-llm-usage--insert-fold
             (string-join (cons "历史对话" (or (loki-assistant-llm-usage--transcript-counts history)
                                                (list (format "%d 项" (length history)))))
                          " · ") t
             (lambda () (loki-assistant-llm-usage--insert-dialogue history t)))))
        (loki-assistant-llm-usage--insert-dialogue (seq-drop incoming latest))))
    (when (eq t (alist-get 'truncated input))
      (insert (propertize "请求内容已截取\n" 'face 'shadow)))
    (let ((counts (loki-assistant-llm-usage--transcript-counts outgoing))
          (active (member (alist-get 'phase item) '("waiting" "responding"))))
      (loki-assistant-llm-usage--insert-section
       "本次响应" (when counts (string-join counts " · ")))
      (cond (outgoing
             (dolist (block outgoing)
               (loki-assistant-llm-usage--insert-transcript-block block))
             (unless (eq t (alist-get 'complete response))
               (insert (propertize "当前仅显示已取得的响应\n" 'face 'shadow))))
            ((eq t (alist-get 'complete response))
             (insert "响应中无可显示内容\n"))
            (active (insert "等待响应内容\n"))
            (t (insert "响应内容未采集\n"))))
    (when (eq t (alist-get 'truncated response))
      (insert (propertize "响应内容已截取\n" 'face 'shadow)))
    (insert "\n")
    (let* ((start (point))
           (capture (loki-assistant-llm-usage--value 'instructions item))
           (instruction-summary (concat "系统指令 " (loki-assistant-llm-usage--instruction-label capture)))
           (tool-summary (format "工具定义 %d 项" (length tools))))
      (loki-assistant-llm-usage--insert-fold
       (string-join (delq nil (list "请求上下文" instruction-summary
                                   (when input tool-summary)
                                   (when settings (format "请求设置 %d 项" (length settings)))))
                    " · ") t
       (lambda ()
         (loki-assistant-llm-usage--insert-instructions capture)
         (when settings
           (loki-assistant-llm-usage--insert-fold
            (format "请求设置 · %d 项" (length settings)) t
            (lambda ()
              (dolist (setting settings)
                (loki-assistant-llm-usage--insert-block
                 (symbol-name (car setting))
                 (loki-assistant-llm-usage--pretty-json (json-encode (cdr setting))) t)))))
         (unless (seq-empty-p tools)
           (loki-assistant-llm-usage--insert-fold
            (format "工具定义 · %d 项" (length tools)) t
            (lambda ()
              (cl-loop for tool across tools
                       do (loki-assistant-llm-usage--insert-block
                           (or (alist-get 'name tool) "工具")
                           (loki-assistant-llm-usage--pretty-json (alist-get 'text tool)) t)))))))
      (put-text-property start (1+ start) 'loki-assistant-llm-section "请求上下文"))))

(defun loki-assistant-llm-usage--detail (data &optional live)
  (let* ((buffer (get-buffer-create "*LLM 请求详情*"))
         (item (alist-get 'item data))
         (windows (when live
                    (or (and (eq (window-buffer) buffer)
                             (buffer-local-value 'loki-assistant-llm-usage--detail-windows buffer))
                        (current-window-configuration)))))
    (with-current-buffer buffer
      (loki-assistant-llm-usage-detail-mode)
      (setq loki-assistant-llm-usage--detail-windows windows
            loki-assistant-llm-usage--detail-id (when live (alist-get 'id item)))
      (let ((inhibit-read-only t)
            (json-null :null)
            (json-false :false))
        (remove-overlays)
        (erase-buffer)
        (if live
            (insert (propertize (format "%s  ·  %s"
                                       (loki-assistant-llm-usage--cell 'model item "未知模型")
                                       (loki-assistant-llm-usage--phase (alist-get 'phase item)))
                                'face 'bold)
                    "\n"
                    (format "%s  ·  %s  ·  %s\n"
                            (loki-assistant-llm-usage--client-label item data)
                            (loki-assistant-llm-usage--cell 'upstream item)
                            (loki-assistant-llm-usage--cell 'protocol item))
                    (format "HTTP %s  ·  首包 %s ms  ·  耗时 %s ms\n"
                            (loki-assistant-llm-usage--cell 'upstream_status item)
                            (loki-assistant-llm-usage--cell 'first_byte_ms item)
                            (loki-assistant-llm-usage--cell 'duration_ms item))
                    (format "请求 %s  ·  %s\n"
                            (loki-assistant-llm-usage--cell 'id item)
                            (loki-assistant-llm-usage--time (alist-get 'started_at item))))
          (insert (format "请求 %s\n\n" (loki-assistant-llm-usage--cell 'id item)))
          (insert (format "客户端  %s\n" (loki-assistant-llm-usage--client-label item data)))
          (cl-loop for (key . label) in '((session_id . "会话")
                                      (upstream . "供应商") (protocol . "协议") (model . "模型")
                                      (upstream_status . "HTTP 状态") (first_byte_ms . "首包 ms")
                                      (duration_ms . "耗时 ms"))
                   do (insert (format "%s  %s\n" label (loki-assistant-llm-usage--cell key item)))))
        (when-let ((usage (loki-assistant-llm-usage--value 'usage item)))
          (insert "\n用量  " (loki-assistant-llm-usage--usage-label usage) "\n")
          (cl-loop for (key . label) in '((input_reported_tokens . "上游输入")
                                          (input_tokens . "输入") (input_uncached_tokens . "未缓存输入")
                                          (cache_read_tokens . "缓存读取") (cache_write_tokens . "缓存写入")
                                          (output_tokens . "输出"))
                   when (assq key usage)
                   do (insert (format "%s  %s\n" label
                                      (loki-assistant-llm-usage--cell key usage "未上报")))))
        (when live
          (unless (loki-assistant-llm-usage--value 'usage item)
            (insert "用量  待上报\n"))
          (loki-assistant-llm-usage--insert-transcript item)
          (setq mode-line-process
                (loki-assistant--mode-line-keys
                 "q 返回  g 刷新  j 章节  Tab/n 下项  S-Tab/p 上项  RET 展开或折叠  C-M-u 上一级  C-s 搜索  w 复制")))
        (when (assq 'raw_usage data)
          (insert "\n原始用量\n")
          (if-let ((raw (loki-assistant-llm-usage--value 'raw_usage data)))
              (insert (json-encode raw))
            (insert "未上报"))
          (insert "\n"))
        (goto-char (point-min))))
    (cond (live
           (when-let ((side (get-buffer-window buffer)))
             (when (and (not (eq side (selected-window))) (window-parameter side 'window-side))
               (delete-window side)))
           (delete-other-windows)
           (switch-to-buffer buffer))
          ((>= (window-total-width) 110)
           (select-window (display-buffer-in-side-window buffer '((side . right) (window-width . 0.42)))))
          (t (pop-to-buffer buffer)))))

(defvar loki-assistant-llm-usage-mode-map
  (let ((map (make-sparse-keymap)))
    (define-key map (kbd "1") #'loki-assistant-llm-usage-overview)
    (define-key map (kbd "2") #'loki-assistant-llm-usage-history)
    (define-key map (kbd "3") #'loki-assistant-llm-usage-sessions)
    (define-key map (kbd "4") #'loki-assistant-llm-usage-totals)
    (define-key map (kbd "5") #'loki-assistant-llm-usage-live)
    (define-key map (kbd "g") #'loki-assistant-llm-usage-refresh)
    (define-key map (kbd "t") #'loki-assistant-llm-usage-toggle-chart)
    (define-key map (kbd "e") #'loki-assistant-llm-usage-export-png)
    (define-key map (kbd "f") #'loki-assistant-llm-usage-filter)
    (define-key map (kbd "F") #'loki-assistant-llm-usage-clear-filters)
    (define-key map (kbd "l") #'loki-assistant-llm-usage-more)
    (define-key map (kbd "b") #'loki-assistant-llm-usage-back)
    (define-key map (kbd "RET") #'loki-assistant-llm-usage-open)
    (define-key map (kbd "C-c C-q") #'quit-window)
    (define-key map (kbd "?") #'describe-mode)
    map))

(define-derived-mode loki-assistant-llm-usage-mode tabulated-list-mode "LLM 用量"
  "查看用量概览、请求历史、会话、累计用量和实时请求。

按 1 至 5 切换视图，g 刷新。按 RET 查看选中行的请求或详情。
在概览中按 t 切换表格和用量图，按 e 导出 PNG。
概览、历史和会话支持 f 筛选和 F 清除筛选。列表有下一页时按 l。
下钻后按 b 返回上一视图。按 C-c C-q 退出，按 ? 查看模式帮助。"
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
