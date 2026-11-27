;;; loki-assistant-tests.el --- HTTP 与通知验证 -*- lexical-binding: t; -*-

;;; Commentary:

;; 使用临时 pfile 和真实 loopback HTTP listener，不修改用户配置或发送系统通知。

;;; Code:

(require 'ert)
(require 'loki-assistant)
(require 'loki-assistant-mail)

(ert-deftest loki-assistant-test-mail-inbox-opens-configured-bookmark ()
  (let ((loki-assistant-mail-inbox-bookmark-key ?g)
        opened)
    (cl-letf (((symbol-function 'loki-assistant-mail--open) #'funcall)
              ((symbol-function 'mu4e-bookmarks)
               (lambda () '((:key ?i :query "maildir:/other")
                            (:key ?g :query (lambda () "maildir:/configured-inbox")))))
              ((symbol-function 'mu4e-search-bookmark)
               (lambda (query) (setq opened query))))
      (loki-assistant-mail-open-inbox)
      (should (equal opened "maildir:/configured-inbox"))
      (let ((loki-assistant-mail-inbox-bookmark-key ?x))
        (should-error (loki-assistant-mail-open-inbox) :type 'user-error)))))

(ert-deftest loki-assistant-test-mail-index-reports-progress-and-one-final-result ()
  (let ((loki-assistant-mail--index-job nil)
        (mu4e--server-process 'test-mu)
        (mu4e--server-indexing t)
        (mu4e-hide-index-messages nil)
        starts tick cancelled results)
    (cl-letf (((symbol-function 'mu4e-running-p) (lambda () t))
              ((symbol-function 'run-at-time)
               (lambda (_delay _repeat function job)
                 (setq tick (lambda () (funcall function job)))
                 'test-timer))
              ((symbol-function 'cancel-timer) (lambda (timer) (setq cancelled timer)))
              ((symbol-function 'mu4e-update-index-nonlazy)
               (lambda ()
                 (should mu4e-hide-index-messages)
                 (push 'started starts)))
              ((symbol-function 'process-command) (lambda (_process) '("/opt/mu" "server")))
              ((symbol-function 'mu4e-database-path) (lambda () "/test/mail/xapian"))
              ((symbol-function 'loki-assistant-request)
               (lambda (method path &rest args)
                 (push (list method path (plist-get args :body)) results))))
      (should (eq (loki-assistant-mail-start-index "fixture" 60) 'accepted))
      (funcall tick)
      (loki-assistant-mail--handle-index-reply
       (lambda (_reply) (should-not mu4e-hide-index-messages))
       '(:info index :status complete :checked 20 :updated 0))
      (should-not starts)
      (should-not results)
      (setq mu4e--server-indexing nil)
      (funcall tick)
      (funcall tick)
      (should (equal starts '(started)))
      (dotimes (_ 2)
        (loki-assistant-mail--handle-index-reply #'ignore
         '(:info index :status running :checked 2 :updated 1)))
      (should (equal results
                     '(("POST" "/mail/index/result/fixture"
                        ((status . "running") (index (checked . 2) (updated . 1)))))))
      (should loki-assistant-mail--index-job)
      (dotimes (_ 2)
        (loki-assistant-mail--handle-index-reply #'ignore
         '(:info index :status complete :checked 20 :updated 1 :cleaned-up 0)))
      (should (equal (car results)
                     '("POST" "/mail/index/result/fixture"
                       ((status . "completed")
                        (database (program . "/opt/mu") (home . "/test/mail")
                                  (index (checked . 20) (updated . 1) (cleaned_up . 0)))))))
      (should (equal (cdr results)
                     '(("POST" "/mail/index/result/fixture"
                        ((status . "running") (index (checked . 2) (updated . 1)))))))
      (should (eq cancelled 'test-timer))
      (should-not loki-assistant-mail--index-job))))

(ert-deftest loki-assistant-test-command-menu-dispatch ()
  (skip-unless (require 'consult nil t))
  (let ((entry-map (make-sparse-keymap))
        (completion-styles '(basic))
        choice calls)
    (define-key entry-map (kbd "C-x C-a") #'loki-assistant-command-map)
    (cl-letf (((symbol-function 'consult--read)
               (lambda (collection &rest options)
                 (should (plist-get options :require-match))
                 (should (eq (lookup-key (plist-get options :keymap) (kbd "C-c C-q"))
                             #'abort-recursive-edit))
                 (unless (equal choice "")
                   (let ((matches (completion-all-completions
                                   choice collection nil (length choice))))
                     (should (and (car matches) (not (consp (cdr matches)))))
                     (funcall (plist-get options :lookup) (car matches) collection)))))
              ((symbol-function 'loki-assistant-config)
               (lambda () (interactive) (push 'config calls)))
              ((symbol-function 'loki-assistant-devices)
               (lambda () (interactive) (push 'devices calls)))
              ((symbol-function 'loki-assistant-read-later)
               (lambda () (interactive) (push 'read-later calls)))
              ((symbol-function 'loki-assistant-llm-usage)
               (lambda () (interactive) (push 'llm-usage calls))))
      (dolist (candidate '("c" "d" "r" "u" "fuwupeizhi" "sbgl" "yuedu" "yl" ""))
        (setq choice candidate)
        (call-interactively (lookup-key entry-map (kbd "C-x C-a")))))
    (should (equal (nreverse calls)
                   '(config devices read-later llm-usage
                     config devices read-later llm-usage)))))

(defun loki-assistant-test--filter (handler process chunk)
  "收集 PROCESS 的 HTTP 请求，将完整请求交给 HANDLER；CHUNK 为新数据。"
  (let ((request (concat (process-get process 'request) chunk)))
    (process-put process 'request request)
    (when (and (not (process-get process 'handled))
               (string-match "\r\n\r\n" request))
      (let* ((end (match-end 0))
             (headers (substring request 0 end))
             (length (if (string-match "[Cc]ontent-[Ll]ength: *\\([0-9]+\\)" headers)
                         (string-to-number (match-string 1 headers)) 0)))
        (when (>= (- (length request) end) length)
          (process-put process 'handled t)
          (let ((response (funcall handler headers (substring request end (+ end length)))))
            ;; nil 响应刻意保持连接，用于验证超时。
            (when response
              (let ((body (encode-coding-string (or (cadr response) "") 'utf-8)))
                (process-send-string
                 process
                 (concat (format "HTTP/1.1 %d Test\r\nContent-Type: application/json\r\nContent-Length: %d\r\nConnection: close\r\n\r\n"
                                 (car response) (string-bytes body))
                         body)))
              (process-send-eof process))))))))

(defmacro loki-assistant-test--with-server (handler &rest body)
  "运行临时 HTTP 服务，使用 HANDLER 处理请求并执行 BODY。"
  (declare (indent 1))
  `(let* ((handler ,handler)
          (server (make-network-process
                   :name "loki-assistant-test-http" :server t :noquery t
                   :host "127.0.0.1" :family 'ipv4 :service t :coding 'binary
                   :log (lambda (listener client _message)
                          (set-process-query-on-exit-flag client nil)
                          (process-put client 'loki-assistant-test-server listener))
                   :filter (lambda (process chunk)
                             (loki-assistant-test--filter handler process chunk))))
          (loki-assistant-service-file (make-temp-file "loki-assistant-service-")))
     (unwind-protect
         (progn
           (with-temp-file loki-assistant-service-file
             (insert (json-encode `((host . "127.0.0.1")
                                    (port . ,(process-contact server :service))))))
           ,@body)
       (dolist (process (process-list))
         (when (or (eq process server)
                   (eq (process-get process 'loki-assistant-test-server) server))
           (delete-process process)))
       (delete-file loki-assistant-service-file))))

(defun loki-assistant-test--await (start)
  "调用 START 并等待其回调，返回响应。"
  (let ((deadline (+ (float-time) 3)) response completed)
    (funcall start (lambda (value) (setq response value completed t)))
    (while (and (not completed) (< (float-time) deadline))
      (accept-process-output nil 0.01))
    (should completed)
    response))

(ert-deftest loki-assistant-test-http-method-query-and-json ()
  (let (received)
    (loki-assistant-test--with-server
        (lambda (headers body)
          (setq received (list headers (decode-coding-string body 'utf-8)))
          '(200 "{\"回答\":\"成功\",\"empty\":null,\"enabled\":false}"))
      (let ((response
             (loki-assistant-test--await
              (lambda (callback)
                (loki-assistant-request
                 'put "/echo?existing=1" :params '((buffer . "中文 &?\"\\") (tag . "a") (tag . "b"))
                 :body '((title . "中文通知 👋") (enabled . :false) (empty . :null))
                 :headers (list (cons "X-Loki-Test" (string-to-multibyte "yes")))
                 :callback callback)))))
        (should (plist-get response :ok))
        (should (= (plist-get response :status) 200))
        (should (equal (alist-get '回答 (plist-get response :data)) "成功"))
        (should (eq (alist-get 'enabled (plist-get response :data)) :false))
        (should (eq (alist-get 'empty (plist-get response :data)) :null))
        (should (string-match-p "HTTP/1.1 200" (plist-get response :headers)))
        (should (string-prefix-p "PUT /echo?existing=1&buffer=" (car received)))
        (should (string-match-p (regexp-quote "%E4%B8%AD%E6%96%87%20%26%3F%22%5C&tag=a&tag=b") (car received)))
        (should (string-match-p "X-Loki-Test: yes" (car received)))
        (should (equal (alist-get 'title (json-parse-string (cadr received) :object-type 'alist)) "中文通知 👋"))))))

(ert-deftest loki-assistant-test-http-errors-preserve-details ()
  (dolist (status '(400 404 503))
    (loki-assistant-test--with-server
        (lambda (_headers _body)
          (list status "{\"api_version\":\"1\",\"status\":404,\"error\":{\"source\":\"business\",\"code\":\"notification-handler-not-found\",\"message\":\"未注册处理器\"}}"))
      (let ((response (loki-assistant-test--await
                       (lambda (callback)
                         (loki-assistant-request "GET" "/missing" :callback callback)))))
        (should-not (plist-get response :ok))
        (should (= (plist-get response :status) status))
        (should (equal (plist-get response :error-code) "notification-handler-not-found"))
        (should (equal (plist-get response :error-message) "未注册处理器"))
        (should (string-match-p "未注册处理器" (plist-get response :body)))))))

(ert-deftest loki-assistant-test-http-slack-send-receipts-with-null-error ()
  (dolist (receipt '((202 . "pending") (200 . "sent")))
    (loki-assistant-test--with-server
        (lambda (_headers _body)
          (list (car receipt) (format "{\"state\":%s,\"error\":null}" (json-encode (cdr receipt)))))
      (let* ((response (loki-assistant-test--await
                        (lambda (callback)
                          (loki-assistant-request "GET" "/send-record" :callback callback))))
             (data (plist-get response :data)))
        (should (plist-get response :ok))
        (should (= (plist-get response :status) (car receipt)))
        (should-not (plist-get response :error-message))
        (should (eq (alist-get 'error data) :null))
        (should (equal (alist-get 'state data) (cdr receipt)))))))

(ert-deftest loki-assistant-test-empty-and-invalid-responses ()
  (dolist (case '((204 "" t nil) (200 "无效 JSON" nil "invalid-response")
                  (502 "upstream failed" nil "http-error")))
    (loki-assistant-test--with-server
        (lambda (_headers _body) (list (car case) (cadr case)))
      (let ((response (loki-assistant-test--await
                       (lambda (callback)
                         (loki-assistant-request 'get "/response" :callback callback)))))
        (should (eq (plist-get response :ok) (nth 2 case)))
        (should (equal (plist-get response :error-code) (nth 3 case)))
        (should (equal (plist-get response :body) (cadr case)))))))

(ert-deftest loki-assistant-test-raw-body-and-scalar-json ()
  (let (received)
    (loki-assistant-test--with-server
        (lambda (_headers body) (setq received body) '(200 "[1,true,false,null]"))
      (let ((response (loki-assistant-test--await
                       (lambda (callback)
                         (loki-assistant-request 'post "/raw" :body "{\"raw\":1}"
                                                 :callback callback)))))
        (should (equal received "{\"raw\":1}"))
        (should (equal (plist-get response :data) [1 t :false :null]))))))

(ert-deftest loki-assistant-test-timeout-cleans-up-and-calls-once ()
  (loki-assistant-test--with-server (lambda (_headers _body) nil)
    (let ((count 0) request-buffer)
      (let ((response
             (loki-assistant-test--await
              (lambda (callback)
                (setq request-buffer
                      (loki-assistant-request
                       'get "/slow" :timeout 0.05
                       :callback (lambda (value) (cl-incf count) (funcall callback value))))))))
        (should (equal (plist-get response :error-code) "timeout"))
        (should-not (buffer-live-p request-buffer))
        (accept-process-output nil 0.1)
        (should (= count 1))))))

(ert-deftest loki-assistant-test-discovery-failures-and-ipv6 ()
  (let ((loki-assistant-service-file (make-temp-file "loki-assistant-pfile-")))
    (unwind-protect
        (progn
          (dolist (content '("not json" "{}" "{\"host\":\"example.com\",\"port\":80}"
                             "{\"host\":\"127.0.0.1\",\"port\":0}"
                             "{\"host\":\"127.300.0.1\",\"port\":80}"))
            (with-temp-file loki-assistant-service-file (insert content))
            (let ((response (loki-assistant-test--await
                             (lambda (callback)
                               (loki-assistant-request 'get "/healthz" :callback callback)))))
              (should (equal (plist-get response :error-code) "service-discovery-failed"))
              (should (stringp (plist-get response :error-message)))))
          (with-temp-file loki-assistant-service-file
            (insert "{\"host\":\"::1\",\"port\":12345}"))
          (should (equal (loki-assistant--service-url) "http://[::1]:12345"))
          (delete-file loki-assistant-service-file)
          (let ((response (loki-assistant-test--await
                           (lambda (callback)
                             (loki-assistant-request 'get "/healthz" :callback callback)))))
            (should (equal (plist-get response :error-code) "service-discovery-failed"))))
      (when (file-exists-p loki-assistant-service-file)
        (delete-file loki-assistant-service-file)))))

(ert-deftest loki-assistant-test-refused-connection ()
  (loki-assistant-test--with-server (lambda (_headers _body) '(200 "{}"))
    (delete-process server)
    (let ((response (loki-assistant-test--await
                     (lambda (callback)
                       (loki-assistant-request 'get "/healthz" :timeout 0.5 :callback callback)))))
      (should-not (plist-get response :ok))
      (should (equal (plist-get response :error-code) "transport-error")))))

(ert-deftest loki-assistant-test-notify-buffer-and-handler ()
  (let (received)
    (loki-assistant-test--with-server
        (lambda (headers body)
          (setq received (list headers (json-parse-string
                                       (decode-coding-string body 'utf-8) :object-type 'alist)))
          '(200 "{\"module_id\":\"tasks\",\"result\":{\"accepted\":true}}"))
      (with-temp-buffer
        (rename-buffer " *loki-assistant-中文\"\\测试*" t)
        (let ((target (current-buffer)))
          (should
           (plist-get
            (loki-assistant-test--await
             (lambda (callback)
               (loki-assistant-notify target "任务完成" :body "正文" :app "emacs"
                                      :module-id "tasks" :payload '((task_id . "1"))
                                      :callback callback))) :ok))
          (should (buffer-live-p target))
          (should (string-prefix-p "POST /emacs/receiver/notification " (car received)))
          (should (equal (alist-get 'buffer (cadr received)) (buffer-name target)))
          (should (equal (alist-get 'module_id (cadr received)) "tasks"))
          (should (equal (alist-get 'title (cadr received)) "任务完成"))
          (should (equal (alist-get 'body (cadr received)) "正文"))
          (should (equal (alist-get 'app (cadr received)) "emacs"))
          (should (equal (alist-get 'task_id (alist-get 'payload (cadr received))) "1")))))))

(ert-deftest loki-assistant-test-notify-defaults-and-failed-receipt ()
  (let (received)
    (loki-assistant-test--with-server
        (lambda (_headers body)
          (setq received (json-parse-string body :object-type 'alist))
          '(200 "{\"result\":{\"status\":\"failed\",\"code\":\"permission-denied\",\"message\":\"未授权\"}}"))
      (let ((response (loki-assistant-test--await
                       (lambda (callback)
                         (loki-assistant-notify nil "完成" :callback callback)))))
        (should (equal (alist-get 'module_id received) "emacs"))
        (dolist (key '(buffer body app icon payload)) (should-not (assq key received)))
        (should-not (plist-get response :ok))
        (should (= (plist-get response :status) 200))
        (should (equal (plist-get response :error-code) "permission-denied"))
        (should (equal (plist-get response :error-message) "未授权"))))))

(ert-deftest loki-assistant-test-invalid-arguments ()
  (dolist (path '("https://example.com" "//example.com/path" "/path#fragment" "/bad path"))
    (should-error (loki-assistant-request 'get path) :type 'user-error))
  (should-error (loki-assistant-request 'get "/" :timeout 0) :type 'user-error)
  (should-error (loki-assistant-request "GET\r\n" "/") :type 'user-error)
  (should-error (loki-assistant-notify nil " ") :type 'user-error)
  (should-error (loki-assistant-notify nil "标题" :module-id "") :type 'user-error)
  (let ((target (generate-new-buffer " *loki-assistant-dead*")))
    (kill-buffer target)
    (should-error (loki-assistant-notify target "标题") :type 'user-error)))

(ert-deftest loki-assistant-test-default-error-feedback ()
  (let ((loki-assistant-service-file (make-temp-file "loki-assistant-missing-"))
        feedback)
    (delete-file loki-assistant-service-file)
    (cl-letf (((symbol-function 'message)
               (lambda (format-string &rest args)
                 (setq feedback (apply #'format format-string args)))))
      (should-not (loki-assistant-notify nil "验证错误反馈")))
    (should (string-match-p "services 请求失败" feedback))
    (should (string-match-p "service-discovery-failed" feedback))))

(provide 'loki-assistant-tests)
;;; loki-assistant-tests.el ends here
