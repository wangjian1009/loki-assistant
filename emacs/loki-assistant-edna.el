;;; loki-assistant-edna.el --- Org Edna 的星期排程与后继扩展 -*- lexical-binding: t; -*-

;;; Commentary:

;; 为 Org Edna 增加 loki-scheduled-next!、loki-deadline-next! 和
;; loki-spawn-next!，与 Org Tasks Core 共用日期和后继关联约定。
;; 由 loki-assistant 在 Org Edna 加载后自动加载，不修改个人配置。

;;; Code:

(require 'org)
(require 'org-edna)
(require 'org-id)
(require 'cl-lib)
(require 'calendar)

(defvar loki-assistant-edna-transaction-active nil)
(defvar loki-assistant-edna-action-seen nil)
(defvar loki-assistant-edna-now nil)

(defun loki-assistant-edna-stable-targets (targets)
  "插入发生在标题起点时，让已选中节点的 marker 跟随原标题。"
  (dolist (target targets)
    (when (markerp target) (set-marker-insertion-type target t)))
  targets)

(unless (advice-member-p #'loki-assistant-edna-stable-targets 'org-edna--handle-finder)
  (advice-add 'org-edna--handle-finder :filter-return #'loki-assistant-edna-stable-targets))

(defun loki-assistant-edna-check-node ()
  "拒绝写入自身或祖先带 CONFLICT 标签的受保护节点。"
  (save-excursion
    (org-back-to-heading t)
    (let ((more t))
      (while more
        (when (member "CONFLICT" (org-get-tags nil t))
          (error "Loki 不能修改冲突节点"))
        (setq more (org-up-heading-safe))))))

(defun loki-assistant-edna-check-rule (base weekdays)
  "校验日期基准 BASE 与非空星期集合 WEEKDAYS。"
  (setq loki-assistant-edna-action-seen t)
  (unless (memq base '(source target today))
    (error "Loki 日期基准必须为 source、target 或 today"))
  (unless (and weekdays
               (cl-every (lambda (day) (memq day '(mon tue wed thu fri sat sun)))
                         weekdays))
    (error "Loki 星期必须为 mon 至 sun 的小写符号，且集合不能为空")))

(defun loki-assistant-edna-clock (stamp)
  "读取 STAMP 中的时刻或同日时刻范围。"
  (when (and stamp (string-match "[0-9]\\{2\\}:[0-9]\\{2\\}\\(?:-[0-9]\\{2\\}:[0-9]\\{2\\}\\)?" stamp))
    (match-string 0 stamp)))

(defun loki-assistant-edna-next-stamp (last-entry base weekdays property)
  "按 BASE 和 WEEKDAYS 计算 PROPERTY 的下一个纯日期时刻。"
  (unless (and (markerp last-entry)
               (eq (marker-buffer last-entry) (current-buffer)))
    (error "Loki 动作只支持当前文件"))
  (when (eq base 'source)
    (org-with-point-at last-entry (loki-assistant-edna-check-node)))
  (let* ((raw (org-entry-get (if (eq base 'source) last-entry (point)) property))
         (clock (loki-assistant-edna-clock raw))
         (time (if (eq base 'today) (or loki-assistant-edna-now (org-current-time))
                 (unless raw (error "Loki 日期基准没有对应规划时间戳"))
                 (org-time-string-to-time raw)))
         (decoded (decode-time time))
         (day (1+ (calendar-absolute-from-gregorian
                   (list (nth 4 decoded) (nth 3 decoded) (nth 5 decoded)))))
         (date (calendar-gregorian-from-absolute day))
         (allowed (mapcar (lambda (weekday) (cl-position weekday '(sun mon tue wed thu fri sat)))
                          weekdays)))
    ;; 自然日与时刻分开计算，避免沿用旧 UTC offset 而跨越或重复夏令时边界的日期。
    (while (not (memq (calendar-day-of-week date) allowed))
      (setq day (1+ day)
            date (calendar-gregorian-from-absolute day)))
    (unless (<= 1 (nth 2 date) 9999)
      (error "Loki 日期超出有效范围"))
    (format "<%04d-%02d-%02d %s%s>"
            (nth 2 date) (nth 0 date) (nth 1 date)
            (nth (calendar-day-of-week date) '("Sun" "Mon" "Tue" "Wed" "Thu" "Fri" "Sat"))
            (if clock (concat " " clock) ""))))

(defun loki-assistant-edna-put-planning (property stamp)
  "只写当前标题的 PROPERTY，保留其他规划和正文。"
  (save-excursion
    (org-back-to-heading t)
    (end-of-line)
    (if (eobp) (insert "\n") (forward-char 1))
    (if (looking-at-p org-planning-line-re)
        (if (re-search-forward (concat property ":[ \t]*\\(<[^>]+>\\)")
                               (line-end-position) t)
            (replace-match stamp t t nil 1)
          (insert property ": " stamp " "))
      (insert property ": " stamp "\n"))))

(defun loki-assistant-edna-set-next (last-entry base weekdays property)
  "修改 PROPERTY 的日期时刻，保留目标原有重复器与预警。"
  (loki-assistant-edna-check-rule base weekdays)
  (loki-assistant-edna-check-node)
  (let* ((stamp (loki-assistant-edna-next-stamp last-entry base weekdays property))
         (old (org-entry-get nil property))
         (modifiers (cl-remove-if-not
                     (lambda (part) (string-match-p "\\`\\(?:\\.\\+\\|[+-]\\)" part))
                     (split-string (or (and old (string-trim old "<" ">")) "")))))
    (when (and (not (loki-assistant-edna-clock stamp))
               (cl-some (lambda (part) (string-match-p "\\`\\(?:\\.\\+\\|[+]\\{1,2\\}\\)[0-9]+h" part))
                        modifiers))
      (error "按小时重复的时间戳必须保留时刻"))
    (when modifiers
      (setq stamp (concat (substring stamp 0 -1) " " (string-join modifiers " ") ">")))
    (loki-assistant-edna-put-planning property stamp)))

(defun org-edna-action/loki-scheduled-next! (last-entry &optional base &rest weekdays)
  "将目标计划安排到 BASE 之后 WEEKDAYS 中的下一个日期。"
  (atomic-change-group
    (loki-assistant-edna-set-next last-entry base weekdays "SCHEDULED")))

(defun org-edna-action/loki-deadline-next! (last-entry &optional base &rest weekdays)
  "将目标截止安排到 BASE 之后 WEEKDAYS 中的下一个日期。"
  (atomic-change-group
    (loki-assistant-edna-set-next last-entry base weekdays "DEADLINE")))

(defun loki-assistant-edna-local-id (id)
  "在本文件中查找唯一 ID，不读取全局 ID 数据库或其他文件。"
  (save-excursion
    (save-restriction
      (widen)
      (goto-char (point-min))
      (let (found)
        (while (re-search-forward org-heading-regexp nil t)
          (beginning-of-line)
          (when (equal id (org-entry-get nil "ID"))
            (when found (error "Loki 后继 ID 在本文件中不唯一"))
            (setq found (point-marker)))
          (forward-line 1))
        found))))

(defun loki-assistant-edna-next-title (title)
  "将 TITLE 末尾的 ASCII 数字串递增，保留前导零及其他文字。"
  (if (not (string-match "[0-9]+\\'" title))
      title
    (let* ((start (match-beginning 0))
           (result (copy-sequence title))
           (index (1- (length result))))
      ;; 逐位进位，和 Core 一样不依赖固定宽度整数。
      (while (and (>= index start) (= (aref result index) ?9))
        (aset result index ?0)
        (setq index (1- index)))
      (if (< index start)
          (concat (substring result 0 start) "1" (substring result start))
        (aset result index (1+ (aref result index)))
        result))))

(defun org-edna-action/loki-spawn-next! (last-entry &optional base &rest weekdays)
  "在目标子树后创建下一次兄弟任务，递增标题末尾数字；已有后继时复用关联。"
  (loki-assistant-edna-check-rule base weekdays)
  (loki-assistant-edna-check-node)
  (unless (and (markerp last-entry)
               (eq (marker-buffer last-entry) (current-buffer)))
    (error "Loki 动作只支持当前文件"))
  (set-marker-insertion-type last-entry t)
  (save-excursion
    (save-restriction
      (widen)
      (org-back-to-heading t)
      (atomic-change-group
        (let* ((target (point-marker))
               (next-id (org-entry-get nil "LOKI_NEXT_ID")))
          (if next-id
              (let ((next (loki-assistant-edna-local-id next-id)))
                (unless (and next (/= next target))
                  (error "Loki 后继关联失效，请检查本文件 LOKI_NEXT_ID"))
                (org-with-point-at next (loki-assistant-edna-check-node)))
            (let* ((stamp (loki-assistant-edna-next-stamp last-entry base weekdays "SCHEDULED"))
                   (level (org-outline-level))
                   (title (loki-assistant-edna-next-title (org-get-heading t t t)))
                   (priority (org-element-property :priority (org-element-at-point)))
                   (tags (org-get-tags nil t))
                   (trigger (org-entry-get nil "TRIGGER"))
                   (old-state (org-get-todo-state))
                   (flow (cl-find-if (lambda (items) (member old-state items)) org-todo-sets))
                   (state (or (cl-find-if (lambda (item) (member item org-not-done-keywords)) flow)
                              (car org-not-done-keywords)))
                   (id (org-id-new))
                   (system-time-locale "C")
                   (created (format-time-string "[%Y-%m-%d %a %H:%M]"
                                                (or loki-assistant-edna-now (org-current-time)))))
              (unless state (error "Loki 后继没有可用的待办状态"))
              (org-end-of-subtree t t)
              (unless (bolp) (insert "\n"))
              (let ((next (point-marker)))
                (insert (make-string level ?*) " " state " "
                        (if priority (format "[#%c] " priority) "") title
                        (if tags (concat " :" (string-join tags ":") ":") "") "\n"
                        "SCHEDULED: " stamp "\n:PROPERTIES:\n:CREATED: " created
                        "\n:ID: " id "\n"
                        (if trigger (concat ":TRIGGER: " trigger "\n") "") ":END:\n")
                (goto-char target)
                (org-entry-put nil "LOKI_NEXT_ID" id)
                ;; 新增待办后恢复已完成祖先；由 Org 更新统计与日志。
                (goto-char next)
                (org-update-parent-todo-statistics)
                (while (org-up-heading-safe)
                  (when (member (org-get-todo-state) org-done-keywords)
                    (let* ((flow (cl-find-if (lambda (items) (member (org-get-todo-state) items)) org-todo-sets))
                           (pending (or (cl-find-if (lambda (item) (member item org-not-done-keywords)) flow)
                                        (car org-not-done-keywords))))
                      (org-todo pending))))))))))))

(defun loki-assistant-edna-todo-transaction (original &rest args)
  "Loki 动作失败时回滚本次状态变化及全部文件内联动。"
  (if (or loki-assistant-edna-transaction-active (not (bound-and-true-p org-edna-mode)))
      (apply original args)
    (let ((loki-assistant-edna-transaction-active t)
          (loki-assistant-edna-action-seen nil)
          (loki-assistant-edna-now (org-current-time))
          (org-block-entry-blocking nil))
      (atomic-change-group
        (prog1 (apply original args)
          (when (and loki-assistant-edna-action-seen org-block-entry-blocking)
            (error "Loki 联动失败，本次状态变化已回滚")))))))

(unless (advice-member-p #'loki-assistant-edna-todo-transaction 'org-todo)
  (advice-add 'org-todo :around #'loki-assistant-edna-todo-transaction))

(defun loki-assistant-edna-unload-function ()
  "卸载扩展时移除事务及节点 marker 建议。"
  (advice-remove 'org-todo #'loki-assistant-edna-todo-transaction)
  (advice-remove 'org-edna--handle-finder #'loki-assistant-edna-stable-targets)
  nil)

(provide 'loki-assistant-edna)
;;; loki-assistant-edna.el ends here
