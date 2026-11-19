;;; loki-assistant-edna-tests.el --- Loki 动作事务与边界验证 -*- lexical-binding: t; -*-

;;; Commentary:

;; 使用隔离 Org 缓冲区验证日期、已有任务链、后继创建及事务回滚。

;;; Code:

(require 'ert)
(require 'loki-assistant-edna)

(defmacro loki-assistant-edna-test-with-org (source &rest body)
  "在隔离 Org 缓冲区中执行 BODY。"
  (declare (indent 1))
  `(let ((org-mode-hook nil)
         (org-todo-keywords '((sequence "TODO" "|" "DONE")))
         (org-edna-mode t)
         (org-edna-finder-use-cache nil)
         (org-edna-from-todo-states 'todo)
         (org-edna-use-inheritance nil)
         (org-trigger-hook '(org-edna-trigger-function))
         (org-blocker-hook nil)
         (org-enforce-todo-dependencies nil)
         (org-enforce-todo-checkbox-dependencies nil)
         (org-after-todo-state-change-hook nil)
         (org-after-todo-statistics-hook nil)
         (org-todo-statistics-hook nil)
         (org-log-done nil)
         (org-log-repeat nil)
         (org-log-into-drawer nil)
         (org-block-entry-blocking nil)
         (loki-assistant-edna-transaction-active nil)
         (loki-assistant-edna-action-seen nil)
         (loki-assistant-edna-now nil))
     (with-temp-buffer
       (insert ,source)
       (org-mode)
       (goto-char (point-min))
       ,@body)))

(ert-deftest loki-assistant-edna-test-invalid-rule-rolls-back-completion ()
  (dolist (rule '("loki-spawn-next!()" "loki-spawn-next!(source)"
                  "loki-spawn-next!(source monday)"
                  "loki-scheduled-next!(\"today\" mon)"
                  "loki-deadline-next!(source mon)"))
    (let ((source (format "* TODO Task\n:PROPERTIES:\n:TRIGGER: self %s\n:END:\n" rule)))
      (loki-assistant-edna-test-with-org source
        (should-error (org-todo "DONE"))
        (should (equal source (buffer-string)))))))

(ert-deftest loki-assistant-edna-test-later-error-rolls-back-created-node-and-link ()
  (let ((source "* TODO Task\nSCHEDULED: <2026-11-13 Fri>\n:PROPERTIES:\n:TRIGGER: self loki-spawn-next!(source mon wed fri) loki-scheduled-next!(source invalid)\n:END:\n"))
    (loki-assistant-edna-test-with-org source
      (should-error (org-todo "DONE"))
      (should (equal source (buffer-string))))))

(ert-deftest loki-assistant-edna-test-spawn-six-months-from-each-completion ()
  (let ((rule "self loki-spawn-next!(today \"+6m\")"))
    (loki-assistant-edna-test-with-org
        (concat "* TODO 复查\nSCHEDULED: <2020-01-01 Wed 09:00-10:00>\n"
                ":PROPERTIES:\n:TRIGGER: " rule "\n:END:\n")
      (let ((fixed-time (encode-time 0 34 12 19 11 2026)))
        (cl-letf (((symbol-function 'org-current-time) (lambda (&rest _) fixed-time)))
          (org-todo "DONE")))
      (goto-char (point-min))
      (should (equal (org-get-todo-state) "DONE"))
      (should (equal (org-entry-get nil "SCHEDULED") "<2020-01-01 Wed 09:00-10:00>"))
      (org-next-visible-heading 1)
      (should (equal (org-get-todo-state) "TODO"))
      (should (equal (org-entry-get nil "SCHEDULED") "<2027-05-19 Wed 09:00-10:00>"))
      (should (equal (org-entry-get nil "CREATED") "[2026-11-19 Thu 12:34]"))
      (should (equal (org-entry-get nil "TRIGGER") rule))
      ;; 第二次延期完成时，仍以本次完成日为基准。
      (let ((fixed-time (encode-time 0 0 15 22 5 2027)))
        (cl-letf (((symbol-function 'org-current-time) (lambda (&rest _) fixed-time)))
          (org-todo "DONE")))
      (goto-char (point-max))
      (org-back-to-heading t)
      (should (equal (org-entry-get nil "SCHEDULED") "<2027-11-22 Mon 09:00-10:00>"))
      (should (equal (org-entry-get nil "TRIGGER") rule)))))

(ert-deftest loki-assistant-edna-test-spawn-months-without-schedule-clamps-month-end ()
  (dolist (case '((2026 11 19 "+6m" "2027-05-19 Wed")
                  (2026 8 31 "+6m" "2027-02-28 Sun")
                  (2023 8 31 "+6m" "2024-02-29 Thu")
                  (2024 2 29 "+12m" "2025-02-28 Fri")
                  (2026 1 31 "+1m" "2026-02-28 Sat")))
    (loki-assistant-edna-test-with-org
        (format "* TODO 复查\n:PROPERTIES:\n:TRIGGER: self loki-spawn-next!(today %S)\n:END:\n"
                (nth 3 case))
      (let ((fixed-time (encode-time 0 30 23 (nth 2 case) (nth 1 case) (car case))))
        (cl-letf (((symbol-function 'org-current-time) (lambda (&rest _) fixed-time)))
          (org-todo "DONE")))
      (goto-char (point-min))
      (org-next-visible-heading 1)
      (should (equal (org-entry-get nil "SCHEDULED") (format "<%s>" (nth 4 case)))))))

(ert-deftest loki-assistant-edna-test-spawn-months-source-and-target-bases ()
  (dolist (base '(source target))
    (loki-assistant-edna-test-with-org
        "* TODO 来源\nSCHEDULED: <2026-08-31 Mon 08:00>\n* TODO 目标\nSCHEDULED: <2026-11-19 Thu 09:00>\n"
      (let ((source (point-marker)))
        (org-next-visible-heading 1)
        (org-edna-action/loki-spawn-next! source base "+6m")
        (org-next-visible-heading 1)
        (should (equal (org-entry-get nil "SCHEDULED")
                       (if (eq base 'source) "<2027-02-28 Sun 08:00>"
                         "<2027-05-19 Wed 09:00>")))))))

(ert-deftest loki-assistant-edna-test-invalid-month-rules-roll-back-completion ()
  (dolist (rule '("loki-spawn-next!(today \"+0m\")"
                  "loki-spawn-next!(today \"-6m\")"
                  "loki-spawn-next!(today \"+1.5m\")"
                  "loki-spawn-next!(today \"+6m\" mon)"
                  "loki-spawn-next!(today \"+6m\" \"+1m\")"
                  "loki-spawn-next!(today \"+999999m\")"
                  "loki-spawn-next!(source \"+6m\")"
                  "loki-scheduled-next!(today \"+6m\")"
                  "loki-deadline-next!(today \"+6m\")"))
    (let ((source (format "* TODO 复查\n:PROPERTIES:\n:TRIGGER: self %s\n:END:\n" rule)))
      (loki-assistant-edna-test-with-org source
        (should-error (org-todo "DONE"))
        (should (equal source (buffer-string)))))))

(ert-deftest loki-assistant-edna-test-month-spawn-later-error-rolls-back-completion ()
  (let ((source "* TODO 复查\n:PROPERTIES:\n:TRIGGER: self loki-spawn-next!(today \"+6m\") loki-scheduled-next!(source mon)\n:END:\n"))
    (loki-assistant-edna-test-with-org source
      (should-error (org-todo "DONE"))
      (should (equal source (buffer-string))))))

(ert-deftest loki-assistant-edna-test-legacy-successor-property-does-not-affect-creation ()
  (loki-assistant-edna-test-with-org
      "* TODO Task 1\nSCHEDULED: <2026-11-13 Fri>\n:PROPERTIES:\n:LOKI_NEXT_ID: missing\n:TRIGGER: self loki-spawn-next!(source mon wed fri)\n:END:\n"
    (org-todo "DONE")
    (goto-char (point-min))
    (should (equal (org-entry-get nil "LOKI_NEXT_ID") "missing"))
    (org-next-visible-heading 1)
    (should (equal (org-get-heading t t t) "Task 2"))
    (should-not (org-entry-get nil "LOKI_NEXT_ID"))))

(ert-deftest loki-assistant-edna-test-hourly-repeat-cannot-lose-its-clock ()
  (let ((source "* TODO Task\nSCHEDULED: <2026-11-13 Fri>\n:PROPERTIES:\n:TRIGGER: next-sibling loki-scheduled-next!(source mon)\n:END:\n* TODO Target\nSCHEDULED: <2026-11-12 Thu 09:00 +1h>\n"))
    (loki-assistant-edna-test-with-org source
      (should-error (org-todo "DONE"))
      (should (equal source (buffer-string))))))

(ert-deftest loki-assistant-edna-test-reopen-creates-another-successor-without-id ()
  (loki-assistant-edna-test-with-org
      "* TODO Task 1\nSCHEDULED: <2026-11-13 Fri>\n:PROPERTIES:\n:TRIGGER: self loki-spawn-next!(source mon wed fri)\n:END:\n"
    (org-todo "DONE")
    (goto-char (point-min))
    (org-next-visible-heading 1)
    (let ((first (copy-marker (point) t)))
      (should-not (org-entry-get nil "ID"))
      (goto-char (point-min))
      (org-todo "TODO")
      (org-todo "DONE")
      (goto-char (point-min))
      (should-not (org-entry-get nil "LOKI_NEXT_ID"))
      (org-next-visible-heading 1)
      (should (equal (org-get-heading t t t) "Task 2"))
      (should-not (org-entry-get nil "ID"))
      (should (< (point) first))
      (should (= (how-many "^\\* TODO Task 2$" (point-min) (point-max)) 2))
      (should (= (how-many "^\\* " (point-min) (point-max)) 3)))))

(ert-deftest loki-assistant-edna-test-weekdays-advance-strictly-and-handle-leap-day ()
  (dolist (case '(("2026-11-16 Mon" (mon wed fri) "2026-11-18 Wed")
                  ("2026-11-18 Wed" (fri mon wed wed) "2026-11-20 Fri")
                  ("2026-11-20 Fri" (mon wed fri) "2026-11-23 Mon")
                  ("2026-11-19 Thu" (mon wed fri) "2026-11-20 Fri")
                  ("2026-11-16 Mon" (mon) "2026-11-23 Mon")
                  ("2024-02-28 Wed" (thu) "2024-02-29 Thu")))
    (loki-assistant-edna-test-with-org
        (format "* TODO Task\nSCHEDULED: <%s>\n" (car case))
      (apply #'org-edna-action/loki-scheduled-next! (point-marker) 'target (nth 1 case))
      (should (equal (org-entry-get nil "SCHEDULED") (format "<%s>" (nth 2 case)))))))

(ert-deftest loki-assistant-edna-test-source-clock-keeps-target-modifiers-and-other-content ()
  (loki-assistant-edna-test-with-org
      "* TODO Source\nSCHEDULED: <2026-11-13 Fri 09:00-10:00>\n* TODO Target\nSCHEDULED: <2026-11-11 Wed 08:00 ++1w -2d> DEADLINE: <2026-11-30 Mon>\n保留正文。\n"
    (let ((source (point-marker)))
      (org-next-visible-heading 1)
      (org-edna-action/loki-scheduled-next! source 'source 'mon 'wed 'fri)
      (should (equal (org-entry-get nil "SCHEDULED") "<2026-11-16 Mon 09:00-10:00 ++1w -2d>"))
      (should (equal (org-entry-get nil "DEADLINE") "<2026-11-30 Mon>"))
      (should (string-match-p "保留正文。" (buffer-string)))
      (org-with-point-at source
        (should (equal (org-entry-get nil "SCHEDULED") "<2026-11-13 Fri 09:00-10:00>"))))))

(ert-deftest loki-assistant-edna-test-today-uses-target-clock-without-requiring-source-date ()
  (loki-assistant-edna-test-with-org
      "* TODO Source\n* TODO Target\nDEADLINE: <2020-01-01 Wed 07:15 -1d>\n"
    (let ((source (point-marker))
          (loki-assistant-edna-now (encode-time 0 34 12 13 11 2026)))
      (org-next-visible-heading 1)
      (org-edna-action/loki-deadline-next! source 'today 'mon 'wed 'fri)
      (should (equal (org-entry-get nil "DEADLINE") "<2026-11-16 Mon 07:15 -1d>")))))

(ert-deftest loki-assistant-edna-test-spawn-copies-task-fields-without-body-or-old-properties ()
  (loki-assistant-edna-test-with-org
      "* TODO [#A] Repeat :work:\nSCHEDULED: <2026-11-13 Fri 09:00-10:00> DEADLINE: <2026-11-30 Mon>\n:PROPERTIES:\n:ID: original\n:OWNER: 不复制\n:TRIGGER: self loki-spawn-next!(source mon wed fri)\n:END:\n保留在原任务的正文。\n** TODO 原任务的子任务\n"
    (let ((fixed-time (encode-time 0 34 12 13 11 2026)))
      (cl-letf (((symbol-function 'org-current-time) (lambda (&rest _) fixed-time)))
        (org-todo "DONE")))
    (goto-char (point-min))
    (let ((source-id (org-entry-get nil "ID")))
      (should (equal source-id "original"))
      (should-not (org-entry-get nil "LOKI_NEXT_ID"))
      (should (equal (org-get-todo-state) "DONE"))
      (org-forward-heading-same-level 1)
      (should (= (org-outline-level) 1))
      (should (equal (org-get-todo-state) "TODO"))
      (should (equal (org-get-heading t t t) "Repeat"))
      (should (= (org-element-property :priority (org-element-at-point)) ?A))
      (should (equal (org-get-tags nil t) '("work")))
      (should-not (org-entry-get nil "ID"))
      (should (equal (org-entry-get nil "CREATED") "[2026-11-13 Fri 12:34]"))
      (should (equal (org-entry-get nil "SCHEDULED") "<2026-11-16 Mon 09:00-10:00>"))
      (should (equal (org-entry-get nil "TRIGGER") "self loki-spawn-next!(source mon wed fri)"))
      (should-not (org-entry-get nil "OWNER"))
      (should-not (org-entry-get nil "DEADLINE"))
      (let ((subtree (buffer-substring-no-properties (point) (save-excursion (org-end-of-subtree t t)))))
        (should-not (string-match-p "保留在原任务的正文" subtree))
        (should-not (string-match-p "原任务的子任务" subtree))))))

(ert-deftest loki-assistant-edna-test-daylight-saving-never-skips-or-reuses-a-calendar-day ()
  (let ((original-zone (getenv "TZ")))
    (unwind-protect
        (progn
          (set-time-zone-rule "America/New_York")
          (dolist (case '(("2026-03-07 Sat 23:30" "2026-03-08 Sun 23:30")
                          ("2026-11-01 Sun 00:30" "2026-11-08 Sun 00:30")))
            (loki-assistant-edna-test-with-org
                (format "* TODO Task\nSCHEDULED: <%s>\n" (car case))
              (org-edna-action/loki-scheduled-next! (point-marker) 'target 'sun)
              (should (equal (org-entry-get nil "SCHEDULED") (format "<%s>" (cadr case)))))))
      (set-time-zone-rule original-zone))))

(ert-deftest loki-assistant-edna-test-existing-course-chain-schedules-all-25-without-creating-tasks ()
  (let* ((rule "next-sibling loki-scheduled-next!(source tue thu) chain!(\"TRIGGER\")")
         (source (concat
                  "** 乐刻健身\n"
                  "*** TODO 乐刻健身 - 1\nSCHEDULED: <2026-07-09 Thu 11:30-13:00>\n:PROPERTIES:\n:TRIGGER: " rule "\n:END:\n"
                  (mapconcat (lambda (number) (format "*** TODO 乐刻健身 - %d\n" number))
                             (number-sequence 2 25) "")
                  "** 其他课程\n*** TODO 保留原样\n"))
         (dates '("2026-07-14 Tue" "2026-07-16 Thu" "2026-07-21 Tue" "2026-07-23 Thu"
                  "2026-07-28 Tue" "2026-07-30 Thu" "2026-08-04 Tue" "2026-08-06 Thu"
                  "2026-08-11 Tue" "2026-08-13 Thu" "2026-08-18 Tue" "2026-08-20 Thu"
                  "2026-08-25 Tue" "2026-08-27 Thu" "2026-09-01 Tue" "2026-09-03 Thu"
                  "2026-09-08 Tue" "2026-09-10 Thu" "2026-09-15 Tue" "2026-09-17 Thu"
                  "2026-09-22 Tue" "2026-09-24 Thu" "2026-09-29 Tue" "2026-10-01 Thu")))
    (loki-assistant-edna-test-with-org source
      (dotimes (offset 25)
        (let ((number (1+ offset)))
          (goto-char (point-min))
          (re-search-forward (format "^\\*\\*\\* TODO 乐刻健身 - %d$" number))
          (beginning-of-line)
          (org-todo "DONE")
          (when (< number 25)
            (goto-char (point-min))
            (re-search-forward (format "^\\*\\*\\* TODO 乐刻健身 - %d$" (1+ number)))
            (beginning-of-line)
            (should (equal (org-entry-get nil "SCHEDULED")
                           (format "<%s 11:30-13:00>" (nth offset dates))))
            (should (equal (org-entry-get nil "TRIGGER") rule)))))
      (should (= (how-many "^\\*\\*\\* DONE 乐刻健身" (point-min) (point-max)) 25))
      (should (= (how-many "^\\*\\*\\* " (point-min) (point-max)) 26))
      (should-not (string-match-p ":LOKI_NEXT_ID:" (buffer-string)))
      (should-not (string-match-p ":ID:" (buffer-string)))
      (goto-char (point-min))
      (search-forward "*** TODO 保留原样")
      (beginning-of-line)
      (should-not (org-entry-get nil "SCHEDULED"))
      (should-not (org-entry-get nil "TRIGGER")))))

(ert-deftest loki-assistant-edna-test-existing-chain-rolls-back-schedule-and-copied-trigger ()
  (let ((source "* TODO 来源\nSCHEDULED: <2026-07-09 Thu 11:30-13:00>\n:PROPERTIES:\n:TRIGGER: next-sibling loki-scheduled-next!(source tue thu) chain!(\"TRIGGER\") loki-deadline-next!(source mon)\n:END:\n* TODO 目标\n"))
    (loki-assistant-edna-test-with-org source
      (should-error (org-todo "DONE"))
      (should (equal (buffer-string) source)))))

(ert-deftest loki-assistant-edna-test-native-edna-actions-continue-to-work ()
  (loki-assistant-edna-test-with-org
      "* TODO 来源\n:PROPERTIES:\n:TRIGGER: next-sibling tag!(\"marked\")\n:END:\n* TODO 目标\n"
    (org-todo "DONE")
    (goto-char (point-min))
    (should (equal (org-get-todo-state) "DONE"))
    (should-not (member "marked" (org-get-tags nil t)))
    (org-next-visible-heading 1)
    (should (equal (org-get-todo-state) "TODO"))
    (should (member "marked" (org-get-tags nil t)))))

(ert-deftest loki-assistant-edna-test-swimming-spawns-numbered-successors-and-preserves-history ()
  (let* ((history "*** DONE 游泳 @ 2026-11 @ 1\nCLOSED: [2026-11-09 Mon 17:34] SCHEDULED: <2026-11-09 Mon 13:00>\n:LOGBOOK:\n- State \"DONE\"       from \"TODO\"       [2026-11-09 Mon 17:34]\n:END:\n")
         (rule "self loki-spawn-next!(source mon wed fri)")
         (source (concat "* 游泳 @ 2026-11 [1/5]\n" history
                         "*** TODO 游泳 @ 2026-11 @ 2\nSCHEDULED: <2026-11-18 Wed 13:00>\n:PROPERTIES:\n:TRIGGER: " rule "\n:END:\n")))
    (loki-assistant-edna-test-with-org source
      (search-forward "*** TODO 游泳 @ 2026-11 @ 2")
      (beginning-of-line)
      (let ((second (point-marker)))
        (org-todo "DONE")
        (org-with-point-at second
          (should (equal (org-get-heading t t t) "游泳 @ 2026-11 @ 2"))
          (should (equal (org-get-todo-state) "DONE"))
          (should (equal (org-entry-get nil "SCHEDULED") "<2026-11-18 Wed 13:00>")))
        (org-forward-heading-same-level 1)
        (should (equal (org-get-heading t t t) "游泳 @ 2026-11 @ 3"))
        (should (= (org-outline-level) 3))
        (should (equal (org-get-todo-state) "TODO"))
        (should (equal (org-entry-get nil "SCHEDULED") "<2026-11-20 Fri 13:00>"))
        (should (equal (org-entry-get nil "TRIGGER") rule))
        (let ((third (copy-marker (point) t)))
          (should-not (org-entry-get nil "ID"))
          (should-not (org-with-point-at second (org-entry-get nil "LOKI_NEXT_ID")))
          (org-todo "DONE")
          (org-forward-heading-same-level 1)
          (should (equal (org-get-heading t t t) "游泳 @ 2026-11 @ 4"))
          (should (equal (org-entry-get nil "SCHEDULED") "<2026-11-23 Mon 13:00>"))
          (should-not (org-entry-get nil "ID"))
          (org-with-point-at second
            (org-todo "TODO")
            (org-todo "DONE"))
          (org-with-point-at second
            (org-forward-heading-same-level 1)
            (should (equal (org-get-heading t t t) "游泳 @ 2026-11 @ 3"))
            (should (< (point) third))
            (should-not (org-entry-get nil "ID")))
          (org-with-point-at third
            (should (equal (org-get-heading t t t) "游泳 @ 2026-11 @ 3"))
            (should-not (org-entry-get nil "ID"))
            (should (equal (org-entry-get nil "SCHEDULED") "<2026-11-20 Fri 13:00>"))))
        (should (= (how-many "^\\*\\*\\* " (point-min) (point-max)) 5))
        (goto-char (point-min))
        (search-forward "*** DONE 游泳 @ 2026-11 @ 1")
        (beginning-of-line)
        (should (equal (buffer-substring-no-properties (point) (save-excursion (org-forward-heading-same-level 1) (point))) history))))))

(ert-deftest loki-assistant-edna-test-successor-title-increments-only-the-trailing-counter ()
  (dolist (case '(("游泳 @ 2026-11 @ 2" "游泳 @ 2026-11 @ 3")
                  ("课程 - 9" "课程 - 10")
                  ("课程 - 009" "课程 - 010")
                  ("课程 - 099" "课程 - 100")
                  ("课程 - 999" "课程 - 1000")
                  ("0" "1")
                  ("课程 2  " "课程 2  ")
                  ("课程 2 完成" "课程 2 完成")
                  ("Repeat" "Repeat")))
    (should (equal (loki-assistant-edna-next-title (car case)) (cadr case)))))

(ert-deftest loki-assistant-edna-test-numbered-title-increments ()
  (dolist (pair '(("练习0" . "练习1") ("练习009" . "练习010")
                  ("练习99" . "练习100") ("123" . "124")
                  ("练习999999999999999999999999999999" . "练习1000000000000000000000000000000")
                  ("练习12课" . "练习12课") ("练习１２" . "练习１２")))
    (should (equal (loki-assistant-edna-next-title (car pair)) (cdr pair)))))

(ert-deftest loki-assistant-edna-test-spawn-numbered-successors-on-each-completion ()
  (loki-assistant-edna-test-with-org
      "* TODO [#A] 练习009 :work2:\nSCHEDULED: <2026-11-13 Fri>\n:PROPERTIES:\n:TRIGGER: self loki-spawn-next!(source mon wed fri)\n:END:\n"
    (org-todo "DONE")
    (let ((completed (buffer-string)))
      (goto-char (point-min))
      (org-todo "TODO")
      (org-todo "DONE")
      (should-not (equal completed (buffer-string))))
    (should (= (how-many "^\\* " (point-min) (point-max)) 3))
    (goto-char (point-min))
    (org-next-visible-heading 1)
    (should (equal (org-get-heading t t t) "练习010"))
    (should (equal (org-get-tags nil t) '("work2")))
    (org-todo "DONE")
    (goto-char (point-min))
    (org-next-visible-heading 1)
    (org-next-visible-heading 1)
    (should (equal (org-get-heading t t t) "练习011"))))

(provide 'loki-assistant-edna-tests)
;;; loki-assistant-edna-tests.el ends here
