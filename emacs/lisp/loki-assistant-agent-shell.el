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

(defun loki-assistant-agent-shell--find-buffer (session-id)
  "Find the live agent-shell buffer identified by SESSION-ID."
  (catch 'found
    (dolist (buffer (agent-shell-buffers))
      (when (equal session-id (agent-shell-session-id :shell-buffer buffer))
        (throw 'found buffer)))))

(defun loki-assistant-agent-shell-select-session (session-id)
  "Display the agent-shell session identified by SESSION-ID."
  (loki-assistant-agent-shell--require)
  (unless (and (stringp session-id) (not (string-empty-p session-id)))
    (loki-assistant-protocol--fail
     "invalid-request" "缺少有效会话标识"))
  (let ((buffer (loki-assistant-agent-shell--find-buffer session-id)))
    (unless buffer
      (loki-assistant-protocol--fail "session-not-found" "目标会话已失效"))
    (condition-case err
        (progn
          (pop-to-buffer buffer)
          (list :session_id session-id))
      (error
       (message "Loki Assistant agent-shell select failed: %S" err)
       (loki-assistant-protocol--fail
        "internal-error" "无法显示目标 agent-shell 会话")))))

(defun loki-assistant-agent-shell-send-prompt (session-id prompt)
  "Send PROMPT to the ready agent-shell session SESSION-ID."
  (loki-assistant-agent-shell--require)
  (unless (and (stringp session-id) (not (string-empty-p session-id)))
    (loki-assistant-protocol--fail
     "invalid-request" "缺少有效会话标识"))
  (unless (and (stringp prompt) (not (string-empty-p prompt)))
    (loki-assistant-protocol--fail
     "invalid-request" "需求不能为空"))
  (let ((buffer (loki-assistant-agent-shell--find-buffer session-id)))
    (unless buffer
      (loki-assistant-protocol--fail "session-not-found" "目标会话已失效"))
    (unless (eq (agent-shell-status :shell-buffer buffer) 'ready)
      (loki-assistant-protocol--fail
       "session-not-ready" "目标会话当前无法接收需求"))
    (condition-case err
        (with-current-buffer buffer
          ;; `shell-maker-submit' is the documented programmatic entry point;
          ;; the ready-state check above prevents implicit queueing or steering.
          (shell-maker-submit :input prompt)
          (list :session_id session-id :accepted t))
      (error
       (message "Loki Assistant agent-shell send failed: %S" err)
       (loki-assistant-protocol--fail
        "send-failed" "需求未能提交给 agent-shell")))))

(provide 'loki-assistant-agent-shell)
;;; loki-assistant-agent-shell.el ends here
