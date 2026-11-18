;;; loki-assistant-capture-tests.el --- 全文捕获测试 -*- lexical-binding: t; -*-

;;; Code:

(require 'ert)
(require 'loki-assistant-capture)

(ert-deftest loki-assistant-capture-test-valid-request-parses ()
  (let* ((request (json-serialize
                   '((title . "标题")
                     (url . "https://example.com/a")
                     (captured_at . "2026-11-18T12:30:00.000Z")
                     (body . "正文"))))
         (data (loki-assistant-capture--parse request)))
    (should (equal (plist-get data :title) "标题"))
    (should (equal (plist-get data :captured-at) "2026-11-18T12:30:00.000Z"))))

(ert-deftest loki-assistant-capture-test-invalid-request-is-json-error ()
  (let* ((response (loki-assistant-capture-full "{}"))
         (object (json-parse-string response :object-type 'alist)))
    (should (equal (alist-get 'code (alist-get 'error object))
                   "capture-invalid"))))

(ert-deftest loki-assistant-capture-test-template-is-local-to-module ()
  (let ((original-global '(("user" "用户" plain "%?")))
        (module-templates '(("f" "全文保存" plain "%?"))))
    (let ((org-roam-capture-templates original-global)
          (loki-assistant-capture-templates module-templates))
      (should (equal (loki-assistant-capture--template-key) "f"))
      (should (equal org-roam-capture-templates original-global)))))

(ert-deftest loki-assistant-capture-test-template-key-is-fixed-to-f ()
  (let ((loki-assistant-capture-templates '(("w" "旧网页模板" plain "%?"))))
    (should-error (loki-assistant-capture--template-key) :type 'user-error)))

(provide 'loki-assistant-capture-tests)
;;; loki-assistant-capture-tests.el ends here
