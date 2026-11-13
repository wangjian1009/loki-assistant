;;; loki-assistant.el --- Local control endpoint for Loki Assistant -*- lexical-binding: t; -*-

;; Version: 0.1.0
;; Package-Requires: ((emacs "28.1"))
;; Keywords: convenience, processes

;;; Commentary:
;; Load this package in an existing Emacs server.  The initial endpoint
;; supports a connection handshake; agent-shell operations are not yet wired.

;;; Code:

(require 'loki-assistant-protocol)

(defconst loki-assistant-version "0.1.0"
  "Version of the Loki Assistant Emacs plugin.")

(defun loki-assistant--dispatch (operation _params)
  "Dispatch the validated OPERATION with _PARAMS."
  (pcase operation
    ("ping" (list :emacs_version emacs-version
                  :plugin_version loki-assistant-version))
    (_ (loki-assistant-protocol--fail "unknown-operation" "不支持此操作"))))

;;;###autoload
(defun loki-assistant-request (encoded)
  "Handle Base64 ENCODED JSON and return a Base64 encoded JSON response."
  (loki-assistant-protocol-handle encoded #'loki-assistant--dispatch))

(provide 'loki-assistant)
;;; loki-assistant.el ends here
