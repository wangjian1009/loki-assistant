;;; loki-assistant-protocol.el --- Loki Assistant wire protocol -*- lexical-binding: t; -*-

;; Copyright (C) 2026

;;; Commentary:
;; Versioned JSON requests and responses carried in UTF-8 Base64 strings.

;;; Code:

(require 'json)
(require 'subr-x)

(define-error 'loki-assistant-protocol-error "Loki Assistant protocol error")

(defun loki-assistant-protocol--fail (code message)
  "Signal a protocol failure with CODE and MESSAGE."
  (signal 'loki-assistant-protocol-error (list code message)))

(defun loki-assistant-protocol--decode (encoded)
  "Decode ENCODED into a JSON object, rejecting invalid transport data."
  (unless (and (stringp encoded)
               (<= (length encoded) 1048576)
               (string-match-p "\\`[A-Za-z0-9+/]*=\\{0,2\\}\\'" encoded))
    (loki-assistant-protocol--fail "invalid-request" "请求编码无效或过长"))
  (condition-case nil
      (let ((object (json-parse-string
                     (decode-coding-string (base64-decode-string encoded) 'utf-8)
                     :object-type 'hash-table :array-type 'array
                     :null-object nil :false-object :false)))
        (unless (hash-table-p object)
          (loki-assistant-protocol--fail "invalid-request" "请求必须是 JSON 对象"))
        object)
    (error (loki-assistant-protocol--fail "invalid-request" "无法解码请求"))))

(defun loki-assistant-protocol--encode (object)
  "Encode JSON OBJECT as a single-line UTF-8 Base64 string."
  (base64-encode-string
   (encode-coding-string (json-serialize object :null-object nil :false-object :false)
                        'utf-8)
   t))

(defun loki-assistant-protocol-handle (encoded dispatch)
  "Validate ENCODED, call DISPATCH with operation and params, and encode a response."
  (let (request-id)
    (loki-assistant-protocol--encode
     (condition-case err
         (let* ((request (loki-assistant-protocol--decode encoded))
                (id (gethash "id" request))
                (version (gethash "version" request))
                (operation (gethash "operation" request))
                (params (gethash "params" request)))
           (unless (and (stringp id) (not (string-empty-p id)) (<= (length id) 128))
             (loki-assistant-protocol--fail "invalid-request" "缺少有效请求标识"))
           (setq request-id id)
           (unless (and (integerp version) (= version 1))
             (loki-assistant-protocol--fail "unsupported-version" "不支持此协议版本"))
           (unless (and (stringp operation) (not (string-empty-p operation))
                        (hash-table-p params))
             (loki-assistant-protocol--fail "invalid-request" "操作或参数格式无效"))
           (list :version 1 :id request-id :ok t
                 :result (funcall dispatch operation params)))
       (loki-assistant-protocol-error
        (list :version 1 :id request-id :ok :false
              :error (list :code (nth 1 err) :message (nth 2 err))))
       (error
        (message "Loki Assistant: %S" err)
        (list :version 1 :id request-id :ok :false
              :error (list :code "internal-error" :message "Emacs 插件发生内部错误")))))))

(provide 'loki-assistant-protocol)
;;; loki-assistant-protocol.el ends here
