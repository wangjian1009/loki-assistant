;;; loki-assistant-devices-tests.el --- 设备管理验证 -*- lexical-binding: t; -*-

;;; Commentary:

;; 使用临时 HTTP 服务和 buffer，验证展开、二维码展示、绑定和生命周期。

;;; Code:

(require 'loki-assistant-tests)
(require 'loki-assistant-devices)

(defconst loki-assistant-devices-test--id "550e8400-e29b-41d4-a716-446655440000")
(defconst loki-assistant-devices-test--endpoint (make-string 64 ?a))
(defconst loki-assistant-devices-test--png
  "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAADklEQVR4nGP4DwYMEAoAU7oL9ZisIGcAAAAASUVORK5CYII=")

(defun loki-assistant-devices-test--peer ()
  "构造公开的测试设备。"
  `((endpoint_id . ,loki-assistant-devices-test--endpoint)
    (name . "我的手机") (bound_at . 1795104100)))

(defun loki-assistant-devices-test--invitation (&optional image)
  "构造带 IMAGE 的测试邀请，不使用真实凭据。"
  `((api_version . "1") (pairing_id . ,loki-assistant-devices-test--id) (status . "pending")
    (qr_payload . ((version . 1) (pairing_id . ,loki-assistant-devices-test--id)
                   (endpoint_id . ,loki-assistant-devices-test--endpoint)
                   (relay_url . "https://relay.example") (relay_token . "relay-secret")
                   (pairing_token . "pairing-secret") (expires_at . 1795104300)))
    (qr_code_png_base64 . ,(or image loki-assistant-devices-test--png))))

(defun loki-assistant-devices-test--wait (condition)
  "等待 CONDITION 成立，最多三秒。"
  (let ((deadline (+ (float-time) 3)))
    (while (and (not (funcall condition)) (< (float-time) deadline))
      (accept-process-output nil 0.01))
    (should (funcall condition))))

(defmacro loki-assistant-devices-test--with-buffer (&rest body)
  "在临时设备界面中执行 BODY 并清理所有计时器。"
  (declare (indent 0))
  `(with-temp-buffer
     (loki-assistant-devices-mode)
     (unwind-protect (progn ,@body)
       (loki-assistant-devices--cleanup))))

(ert-deftest loki-assistant-devices-test-commands-protect-unrelated-buffers ()
  (with-temp-buffer
    (insert "用户正在编辑的正文")
    (dolist (command '(loki-assistant-devices-refresh loki-assistant-devices-add
                       loki-assistant-devices-quit))
      (should-error (funcall command) :type 'user-error)
      (should (equal (buffer-string) "用户正在编辑的正文")))))

(ert-deftest loki-assistant-devices-test-shortcut-and-home-only-loads-bound-peers ()
  (should (eq (lookup-key loki-assistant-command-map (kbd "d")) #'loki-assistant-devices))
  (let (requests)
    (loki-assistant-test--with-server
        (lambda (headers _body)
          (push headers requests)
          (list 200 (json-encode `((api_version . "1") (peers . [,(loki-assistant-devices-test--peer)])))))
      (loki-assistant-devices-test--with-buffer
        (loki-assistant-devices-refresh)
        (loki-assistant-devices-test--wait (lambda () (not loki-assistant-devices--loading)))
        (should (= (length requests) 1))
        (should (string-prefix-p "GET /network/peers " (car requests)))
        (should (string-match-p "我的手机" (buffer-string)))
        (should (string-match-p "新增设备" (buffer-string)))
        (should-not (string-match-p "二维码" (buffer-string)))
        (should-not loki-assistant-devices--expanded)
        (should-not loki-assistant-devices--timer)))))

(ert-deftest loki-assistant-devices-test-server-image-expands-in-current-buffer-and-completes ()
  (let (bound requests)
    (loki-assistant-test--with-server
        (lambda (headers _body)
          (push headers requests)
          (list (if (string-prefix-p "POST" headers) 201 200)
                (json-encode
                 (cond
                  ((string-prefix-p "POST /network/pairings " headers)
                   (loki-assistant-devices-test--invitation))
                  ((string-prefix-p "GET /network/peers " headers)
                   `((api_version . "1") (peers . ,(if bound (vector (loki-assistant-devices-test--peer)) []))))
                  (t (setq bound t)
                     `((api_version . "1") (pairing_id . ,loki-assistant-devices-test--id)
                       (status . "completed") (expires_at . 1795104300)
                       (peer . ,(loki-assistant-devices-test--peer))))))))
      (loki-assistant-devices-test--with-buffer
        (let ((buffer (current-buffer)))
          (loki-assistant-devices-add)
          (loki-assistant-devices-test--wait (lambda () (not loki-assistant-devices--busy)))
          (should (eq buffer (current-buffer)))
          (should loki-assistant-devices--expanded)
          (should (imagep (plist-get loki-assistant-devices--panel :image)))
          (should (string-match-p "目标电脑：" (buffer-string)))
          (should (string-match-p "有效期至：" (buffer-string)))
          (should-not (string-match-p "relay-secret\\|pairing-secret" (buffer-string)))
          (cl-letf (((symbol-function 'get-buffer-window) (lambda (&rest _) t)))
            (loki-assistant-devices--poll buffer loki-assistant-devices--generation
                                          loki-assistant-devices--pair-generation))
          (loki-assistant-devices-test--wait
           (lambda () (and (equal (plist-get loki-assistant-devices--panel :status) "completed")
                           (not loki-assistant-devices--loading))))
          (should (= (length loki-assistant-devices--peers) 1))
          (should-not loki-assistant-devices--timer)
          (should-not (plist-get loki-assistant-devices--panel :image))
          (should (string-match-p "已绑定：我的手机" (buffer-string))))))))

(ert-deftest loki-assistant-devices-test-collapse-and-kill-cancel-late-creation ()
  (dolist (close '(collapse kill))
    (let ((buffer (generate-new-buffer " *loki-devices-late*")) callback requests)
      (cl-letf (((symbol-function 'loki-assistant-request)
                 (lambda (method path &rest args)
                   (push (list method path) requests)
                   (when (equal method "POST") (setq callback (plist-get args :callback))))))
        (unwind-protect
            (progn
              (with-current-buffer buffer
                (loki-assistant-devices-mode)
                (loki-assistant-devices-add)
                (if (eq close 'kill) (kill-buffer buffer)
                  (loki-assistant-devices--collapse)))
              (funcall callback (list :ok t :data (loki-assistant-devices-test--invitation)))
              (should (member (list "DELETE" (concat "/network/pairings/" loki-assistant-devices-test--id)) requests))
              (when (buffer-live-p buffer)
                (with-current-buffer buffer
                  (should-not loki-assistant-devices--expanded)
                  (should-not loki-assistant-devices--timer))))
          (when (buffer-live-p buffer) (kill-buffer buffer)))))))

(ert-deftest loki-assistant-devices-test-polling-skips-hidden-and-overlapping-requests ()
  (let (requests progress)
    (cl-letf (((symbol-function 'loki-assistant-request)
               (lambda (method path &rest args)
                 (push (list method path) requests)
                 (if (equal method "POST")
                     (funcall (plist-get args :callback)
                              (list :ok t :data (loki-assistant-devices-test--invitation)))
                   (when (equal method "GET") (setq progress (plist-get args :callback)))))))
      (loki-assistant-devices-test--with-buffer
        (loki-assistant-devices-add)
        (let ((buffer (current-buffer)) (generation loki-assistant-devices--generation)
              (request loki-assistant-devices--pair-generation))
          (cl-letf (((symbol-function 'get-buffer-window) (lambda (&rest _) nil)))
            (loki-assistant-devices--poll buffer generation request))
          (should (= (length requests) 1))
          (cl-letf (((symbol-function 'get-buffer-window) (lambda (&rest _) t)))
            (loki-assistant-devices--poll buffer generation request)
            (loki-assistant-devices--poll buffer generation request))
          (should (= (length requests) 2))
          (loki-assistant-devices--collapse)
          (funcall progress (list :ok t :data `((api_version . "1") (pairing_id . ,loki-assistant-devices-test--id)
                                                (status . "completed") (peer . ,(loki-assistant-devices-test--peer)))))
          (should-not loki-assistant-devices--panel)
          (should-not loki-assistant-devices--timer)
          (should (= (length requests) 3)))))))

(ert-deftest loki-assistant-devices-test-invalid-images-and-responses-cancel-invitation ()
  (dolist (data (list (loki-assistant-devices-test--invitation "bm90IGEgcG5n") []))
    (let (cancelled)
      (cl-letf (((symbol-function 'loki-assistant-request)
                 (lambda (method _path &rest args)
                   (if (equal method "DELETE") (setq cancelled t)
                     (funcall (plist-get args :callback) (list :ok t :data data))))))
        (loki-assistant-devices-test--with-buffer
          (loki-assistant-devices-add)
          (should loki-assistant-devices--panel-error)
          (should-not loki-assistant-devices--timer)
          (should-not loki-assistant-devices--panel)
          (when (listp data) (should cancelled)))))))

(ert-deftest loki-assistant-devices-test-refresh-ignores-old-response-and-retains-peers-on-error ()
  (let (callbacks)
    (cl-letf (((symbol-function 'loki-assistant-request)
               (lambda (_method _path &rest args) (push (plist-get args :callback) callbacks))))
      (loki-assistant-devices-test--with-buffer
        (loki-assistant-devices-refresh)
        (loki-assistant-devices-refresh)
        (funcall (car callbacks) (list :ok t :data `((api_version . "1") (peers . [,(loki-assistant-devices-test--peer)]))))
        (funcall (cadr callbacks) (list :ok t :data '((api_version . "1") (peers . []))))
        (should (= (length loki-assistant-devices--peers) 1))
        (loki-assistant-devices-refresh)
        (funcall (car callbacks) '(:ok nil :error-message "服务暂不可用"))
        (should (= (length loki-assistant-devices--peers) 1))
        (should (string-match-p "服务暂不可用" (buffer-string)))))))

(ert-deftest loki-assistant-devices-test-unbind-preserves-failure-and-refreshes-after-success ()
  (let ((success nil))
    (loki-assistant-test--with-server
        (lambda (headers _body)
          (cond
           ((string-prefix-p "DELETE /network/peers/" headers)
            (if success '(200 "{\"api_version\":\"1\",\"removed\":true}")
              '(500 "{\"api_version\":\"1\",\"error\":{\"code\":\"peer-storage-error\",\"message\":\"无法保存设备记录\"}}")))
           (t '(200 "{\"api_version\":\"1\",\"peers\":[]}"))))
      (cl-letf (((symbol-function 'yes-or-no-p) (lambda (&rest _) t)))
        (loki-assistant-devices-test--with-buffer
          (setq loki-assistant-devices--peers (vector (loki-assistant-devices-test--peer)))
          (loki-assistant-devices-remove loki-assistant-devices-test--endpoint "我的手机")
          (loki-assistant-devices-test--wait (lambda () (not loki-assistant-devices--busy)))
          (should (= (length loki-assistant-devices--peers) 1))
          (should (string-match-p "无法保存设备记录" (buffer-string)))
          (setq success t)
          (loki-assistant-devices-remove loki-assistant-devices-test--endpoint "我的手机")
          (loki-assistant-devices-test--wait
           (lambda () (and (not loki-assistant-devices--busy) (not loki-assistant-devices--loading))))
          (should (zerop (length loki-assistant-devices--peers))))))))

(provide 'loki-assistant-devices-tests)
;;; loki-assistant-devices-tests.el ends here
