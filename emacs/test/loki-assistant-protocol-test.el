;;; loki-assistant-protocol-test.el --- Protocol tests -*- lexical-binding: t; -*-

;;; Commentary:
;; Run with `ert-run-tests-batch', never a function which exits the user's Emacs.

;;; Code:

(require 'ert)
(require 'loki-assistant)

(defun loki-assistant-test--request (object)
  "Send OBJECT through the public endpoint and decode the response."
  (json-parse-string
   (decode-coding-string
    (base64-decode-string
     (loki-assistant-request (loki-assistant-protocol--encode object)))
    'utf-8)
   :object-type 'hash-table :null-object nil :false-object :false))

(ert-deftest loki-assistant-protocol-ping ()
  (let* ((response (loki-assistant-test--request
                    (list :version 1 :id "ping-1" :operation "ping"
                          :params (make-hash-table :test 'equal))))
         (result (gethash "result" response)))
    (should (eq (gethash "ok" response) t))
    (should (equal (gethash "id" response) "ping-1"))
    (should (equal (gethash "emacs_version" result) emacs-version))))

(ert-deftest loki-assistant-agent-shell-list-sessions ()
  (let* ((response (loki-assistant-test--request
                    (list :version 1 :id "sessions-1"
                          :operation "list-sessions"
                          :params (make-hash-table :test 'equal))))
         (result (gethash "result" response))
         (sessions (gethash "sessions" result)))
    (should (eq (gethash "ok" response) t))
    (should (vectorp sessions))
    (mapc (lambda (session)
            (should (stringp (gethash "id" session)))
            (should (stringp (gethash "buffer_name" session)))
            (should (or (null (gethash "project_path" session))
                        (stringp (gethash "project_path" session))))
            (should (member (gethash "status" session)
                            '("ready" "busy" "unknown"))))
          sessions)))

(ert-deftest loki-assistant-protocol-invalid-transport ()
  (dolist (invalid '("%%%" "" "e30=\n" 123))
    (let ((response (json-parse-string
                     (decode-coding-string
                      (base64-decode-string (loki-assistant-request invalid)) 'utf-8))))
      (should (eq (gethash "ok" response) :false))
      (should (equal (gethash "code" (gethash "error" response)) "invalid-request")))))

(ert-deftest loki-assistant-protocol-validates-version-and-operation ()
  (dolist (entry '((2 "ping" "unsupported-version")
                   (1 "missing" "unknown-operation")))
    (let ((response (loki-assistant-test--request
                     (list :version (nth 0 entry) :id "error-1"
                           :operation (nth 1 entry)
                           :params (make-hash-table :test 'equal)))))
      (should (eq (gethash "ok" response) :false))
      (should (equal (gethash "id" response) "error-1"))
      (should (equal (gethash "code" (gethash "error" response)) (nth 2 entry))))))

(ert-deftest loki-assistant-protocol-preserves-text-as-data ()
  (let* ((text "中文、\"引号\"、反斜线\\与换行\n第二行")
         (encoded (loki-assistant-protocol--encode
                   (list :version 1 :id "text-1" :operation "echo"
                         :params (list :prompt text))))
         (response (loki-assistant-protocol-handle
                    encoded (lambda (_operation params) params)))
         (decoded (json-parse-string
                   (decode-coding-string (base64-decode-string response) 'utf-8))))
    (should (equal (gethash "prompt" (gethash "result" decoded)) text))))

(provide 'loki-assistant-protocol-test)
;;; loki-assistant-protocol-test.el ends here
