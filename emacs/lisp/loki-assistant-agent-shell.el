;;; loki-assistant-agent-shell.el --- agent-shell adapter -*- lexical-binding: t; -*-

;;; Commentary:
;; Adapt the public agent-shell APIs to Loki Assistant's stable wire protocol.
;; The adapter is loaded with Loki Assistant, but agent-shell is required only
;; when a session operation is requested so that `ping' remains independent.

;;; Code:

(defun loki-assistant-agent-shell--require ()
  "Load agent-shell or signal a protocol-level availability error."
  (unless (require 'agent-shell nil t)
    (loki-assistant-protocol--fail
     "agent-shell-unavailable" "未找到 agent-shell 插件")))

(defun loki-assistant-agent-shell--status (buffer)
  "Return the protocol status for agent-shell BUFFER."
  (pcase (agent-shell-status :shell-buffer buffer)
    ('ready "ready")
    ('busy "busy")
    (_ "unknown")))

(defun loki-assistant-agent-shell--project-path (buffer)
  "Return BUFFER's project path, or nil when it cannot be determined."
  (with-current-buffer buffer
    (when (and (boundp 'default-directory)
               (stringp default-directory)
               (not (string-empty-p default-directory)))
      (directory-file-name (expand-file-name default-directory)))))

(defun loki-assistant-agent-shell--session (buffer)
  "Convert agent-shell BUFFER to a protocol session object."
  (let ((id (agent-shell-session-id :shell-buffer buffer)))
    (when (and (stringp id) (not (string-empty-p id)))
      (list :id id
            :buffer_name (buffer-name buffer)
            :project_path (loki-assistant-agent-shell--project-path buffer)
            :status (loki-assistant-agent-shell--status buffer)))))

(defun loki-assistant-agent-shell-list-sessions ()
  "Return the current agent-shell sessions for the wire protocol."
  (loki-assistant-agent-shell--require)
  (condition-case err
      (list :sessions
            (vconcat
             (delq nil
                   (mapcar #'loki-assistant-agent-shell--session
                           (agent-shell-buffers)))))
    (error
     (message "Loki Assistant agent-shell list failed: %S" err)
     (loki-assistant-protocol--fail
      "agent-shell-unavailable" "无法读取 agent-shell 会话"))))

(provide 'loki-assistant-agent-shell)
;;; loki-assistant-agent-shell.el ends here
