;;; loki-assistant-monitor-tests.el --- 状态上报与关注集合验证 -*- lexical-binding: t; -*-

;;; Commentary:

;; 临时 buffer 和 HTTP listener，不启动 Agent、不保存用户配置。

;;; Code:

(require 'loki-assistant-tests)
(require 'loki-assistant-monitor)

(define-derived-mode loki-assistant-monitor-test-mode fundamental-mode "关注测试"
  "用于验证 mode hook 注册的临时模式。")

(defmacro loki-assistant-monitor-test--isolated (&rest body)
  "在隔离的上报器状态中运行 BODY，并清理定时器与临时 buffer。"
  (declare (indent 0))
  `(let ((loki-assistant-monitor-mode nil)
         (loki-assistant-monitor-buffer '(loki-assistant-monitor-test-mode))
         (loki-assistant-monitor-interval 60)
         (loki-assistant-monitor--buffers (make-hash-table :test #'eq))
         (loki-assistant-status--store
          (loki-assistant-status-store-create :values (make-hash-table :test #'equal)))
         (loki-assistant-monitor--hooks nil)
         (loki-assistant-monitor--timer nil)
         (loki-assistant-monitor--pending-timer nil)
         (loki-assistant-monitor--inflight nil)
         (loki-assistant-monitor--dirty nil)
         (loki-assistant-monitor--epoch nil)
         (loki-assistant-monitor--last-error nil))
     (unwind-protect (progn ,@body)
       (loki-assistant-monitor-mode -1))))

(ert-deftest loki-assistant-monitor-test-hooks-and-buffer-lifecycle ()
  (loki-assistant-monitor-test--isolated
    (with-temp-buffer
      ;; 启用时补注册已存在的 buffer，之后由 mode hook 维护。
      (loki-assistant-monitor-test-mode)
      (loki-assistant-monitor-mode 1)
      (should (= (hash-table-count loki-assistant-monitor--buffers) 1))
      (let ((buffer (current-buffer)))
        (rename-buffer " *关注测试：改名*" t)
        (should (eq (gethash buffer loki-assistant-monitor--buffers) buffer))
        (let ((snapshot (loki-assistant-monitor--snapshot-buffers)))
          (should (= (length snapshot) 1))
          (should (equal (alist-get 'name (aref snapshot 0)) (buffer-name))))
        (fundamental-mode)
        (should (zerop (hash-table-count loki-assistant-monitor--buffers))))
      (loki-assistant-monitor-test-mode)
      (should (= (hash-table-count loki-assistant-monitor--buffers) 1)))
    (should (zerop (length (loki-assistant-monitor--snapshot-buffers))))))

(ert-deftest loki-assistant-monitor-test-closed-name-reuse-registers-buffer ()
  (loki-assistant-monitor-test--isolated
    (loki-assistant-monitor-mode 1)
    (let ((buffer (generate-new-buffer " *关注测试复用*")) name)
      (with-current-buffer buffer
        (loki-assistant-monitor-test-mode)
        (setq name (buffer-name)))
      (kill-buffer buffer)
      (with-temp-buffer
        (rename-buffer name)
        (loki-assistant-monitor-test-mode)
        (should (eq (gethash (current-buffer) loki-assistant-monitor--buffers)
                    (current-buffer)))))))

(ert-deftest loki-assistant-monitor-test-real-http-post-groups-apps-and-items ()
  (let (received headers)
    (loki-assistant-test--with-server
        (lambda (request body)
          (setq headers request received (json-parse-string (decode-coding-string body 'utf-8)
                                                           :object-type 'alist :false-object :false))
          '(200 "{\"api_version\":\"1\",\"status\":{},\"updated_at\":1}"))
      (loki-assistant-monitor-test--isolated
        (with-temp-buffer
          (loki-assistant-monitor-test-mode)
          (loki-assistant-monitor-mode 1)
          (loki-assistant-status-set-value 'tools 'enabled :false)
          (loki-assistant-status-set-value 'tools 'items [])
          (loki-assistant-status-sync)
          (let ((deadline (+ (float-time) 3)))
            (while (and loki-assistant-monitor--inflight (< (float-time) deadline))
              (accept-process-output nil 0.01)))
          (should-not loki-assistant-monitor--inflight)
          (should (string-prefix-p "POST /emacs/status " headers))
          (should (eq (alist-get 'enabled (alist-get 'tools received)) :false))
          (should (equal (alist-get 'items (alist-get 'tools received)) []))
          (let* ((emacs-data (alist-get 'emacs received))
                 (buffers (alist-get 'buffer emacs-data)))
            (should (= (length buffers) 1))
            (should (equal (alist-get 'name (aref buffers 0)) (buffer-name)))
            (should (eq (alist-get 'modified (aref buffers 0)) :false))
            (should (= (alist-get 'pid (alist-get 'runtime emacs-data)) (emacs-pid)))))))))

(ert-deftest loki-assistant-monitor-test-coalesces-and-ignores-old-callbacks ()
  (loki-assistant-monitor-test--isolated
    (let (callbacks bodies)
      (cl-letf (((symbol-function 'loki-assistant-request)
                 (lambda (_method _path &rest args)
                   (push (plist-get args :callback) callbacks)
                   (push (plist-get args :body) bodies))))
        (loki-assistant-monitor-mode 1)
        (loki-assistant-status-set-value 'tools 'count 1)
        (loki-assistant-status-sync)
        (should loki-assistant-monitor--inflight)
        (loki-assistant-status-set-value 'tools 'count 2)
        (loki-assistant-status-sync)
        (should (= (length callbacks) 1))
        (funcall (pop callbacks) '(:ok t))
        (loki-assistant-status-sync)
        (should (= (length bodies) 2))
        (should (= (alist-get 'count (alist-get 'tools (json-parse-string (car bodies) :object-type 'alist))) 2))
        (let ((old (pop callbacks)))
          (loki-assistant-monitor-mode -1)
          (loki-assistant-monitor-mode 1)
          (loki-assistant-status-sync)
          (funcall old '(:ok nil :error-message "过期错误"))
          (should loki-assistant-monitor--inflight)
          (should-not loki-assistant-monitor--last-error)
          (funcall (pop callbacks) '(:ok t)))))))

(ert-deftest loki-assistant-monitor-test-failure-retains-data-for-next-heartbeat ()
  (loki-assistant-monitor-test--isolated
    (let ((fail t) bodies)
      (cl-letf (((symbol-function 'loki-assistant-request)
                 (lambda (_method _path &rest args)
                   (push (plist-get args :body) bodies)
                   (funcall (plist-get args :callback)
                            (if fail '(:ok nil :error-message "服务停止") '(:ok t))))))
        (loki-assistant-monitor-mode 1)
        (loki-assistant-status-set-value 'tools 'count 3)
        (loki-assistant-status-sync)
        (should (equal loki-assistant-monitor--last-error "服务停止"))
        (should-not loki-assistant-monitor--pending-timer)
        (setq fail nil)
        (loki-assistant-status-sync)
        (should-not loki-assistant-monitor--last-error)
        (should (= (alist-get 'count (alist-get 'tools (json-parse-string (car bodies) :object-type 'alist))) 3))))))

(ert-deftest loki-assistant-monitor-test-customizing-reconciles-hooks-and-registry ()
  (loki-assistant-monitor-test--isolated
    (with-temp-buffer
      (loki-assistant-monitor-test-mode)
      (loki-assistant-monitor-mode 1)
      (let ((original (default-value 'loki-assistant-monitor-buffer)))
        (unwind-protect
            (progn
              ;; setter 使用默认值；让本测试的动态绑定同步更新。
              (setq loki-assistant-monitor-buffer nil)
              (loki-assistant-monitor--set-buffers 'loki-assistant-monitor-buffer nil)
              (should (zerop (hash-table-count loki-assistant-monitor--buffers)))
              (should-not (memq #'loki-assistant-monitor-register-buffer
                                loki-assistant-monitor-test-mode-hook)))
          (set-default 'loki-assistant-monitor-buffer original))))))

(provide 'loki-assistant-monitor-tests)
;;; loki-assistant-monitor-tests.el ends here

(ert-deftest loki-assistant-monitor-test-status-store-symbol-list-vector-and-clear ()
  (loki-assistant-monitor-test--isolated
    (loki-assistant-monitor-mode 1)
    (should (equal (loki-assistant-status-set-value 'emacs 'key1 'value1) 'value1))
    (should (equal (loki-assistant-status-get-value 'emacs 'key1) 'value1))
    (loki-assistant-status-set-value 'emacs 'list '(value1 2 :false :null))
    (loki-assistant-status-set-value 'emacs 'vector [value2 3])
    (should (equal (loki-assistant-status-get-value 'emacs 'list) '(value1 2 :false :null)))
    (should (equal (loki-assistant-status-get-value 'emacs 'vector) [value2 3]))
    (should (null (loki-assistant-status-set-value 'emacs 'key1 nil)))
    (should-not (loki-assistant-status-get-value 'emacs 'key1))
    (should-error (loki-assistant-status-set-value 'bad/name 'key 1) :type 'user-error)
    (should-error (loki-assistant-status-set-value 'emacs 'bad/key 1) :type 'user-error)))

(ert-deftest loki-assistant-monitor-test-status-update-supports-sequence-operations ()
  (loki-assistant-monitor-test--isolated
    (loki-assistant-monitor-mode 1)
    (loki-assistant-status-set-value 'emacs 'items '(a b))
    (loki-assistant-status-update-value 'emacs 'items (lambda (value) (append value '(c))))
    (should (equal (loki-assistant-status-get-value 'emacs 'items) '(a b c)))
    (loki-assistant-status-set-value 'emacs 'items [a b])
    (loki-assistant-status-update-value 'emacs 'items (lambda (value) (vconcat value [c])))
    (should (equal (loki-assistant-status-get-value 'emacs 'items) [a b c]))
    (loki-assistant-status-update-value 'emacs 'items (lambda (_value) nil))
    (should-not (loki-assistant-status-get-value 'emacs 'items))))

(ert-deftest loki-assistant-monitor-test-trigger-sync-restarts-delay-and-coalesces ()
  (loki-assistant-monitor-test--isolated
    (let ((loki-assistant-status-sync-delay 0.05) calls)
      (cl-letf (((symbol-function 'loki-assistant-status-sync)
                 (lambda () (push t calls))))
        (loki-assistant-monitor-mode 1)
        (loki-assistant-status-trigger-sync)
        (sit-for 0.02)
        (loki-assistant-status-trigger-sync)
        (should-not calls)
        (sit-for 0.08)
        (should (= (length calls) 1))))))
