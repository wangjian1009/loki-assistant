;;; loki-assistant.el --- Local control endpoint for Loki Assistant -*- lexical-binding: t; -*-

;; Version: 0.2.0
;; Package-Requires: ((emacs "28.1"))
;; Keywords: convenience, processes

;;; Commentary:
;; Load this package in an existing Emacs server.  The endpoint supports the
;; connection handshake and agent-shell session discovery and selection.

;;; Code:

(require 'loki-assistant-protocol)
(require 'loki-assistant-agent-shell)

(defconst loki-assistant-version "0.2.0"
  "Version of the Loki Assistant Emacs plugin.")

(defun loki-assistant--dispatch (operation params)
  "Dispatch the validated OPERATION with PARAMS."
  (pcase operation
    ("ping" (list :emacs_version emacs-version
                  :plugin_version loki-assistant-version))
    ("list-sessions" (loki-assistant-agent-shell-list-sessions))
    ("select-session"
     (loki-assistant-agent-shell-select-session
      (gethash "session_id" params)))
    ("send-prompt"
     (loki-assistant-agent-shell-send-prompt
      (gethash "session_id" params)
      (gethash "prompt" params)))
    (_ (loki-assistant-protocol--fail "unknown-operation" "不支持此操作"))))

;;;###autoload
(defun loki-assistant-request (encoded)
  "Handle Base64 ENCODED JSON and return a Base64 encoded JSON response."
  (loki-assistant-protocol-handle encoded #'loki-assistant--dispatch))

(provide 'loki-assistant)
;;; loki-assistant.el ends here
