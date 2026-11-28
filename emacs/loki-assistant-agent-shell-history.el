;;; loki-assistant-agent-shell-history.el --- 远程浏览和恢复 Agent 会话 -*- lexical-binding: t; -*-

(require 'loki-assistant-agent-shell)

(defvar-local loki-assistant-agent-shell--history-result nil)

(defun loki-assistant-agent-shell--history-strategy (_context)
  "标识从手机发起的历史查询和恢复。"
  (error "历史操作须由 Agent Shell 初始化处理"))

(defun loki-assistant-agent-shell--history-entries (sessions cwd)
  "转换项目 CWD 中的 ACP SESSIONS，最近更新的会话排在前面。"
  (let ((seen (make-hash-table :test 'equal)))
    (cl-loop for session in (agent-shell--sort-sessions-by-recency sessions)
             for id = (map-elt session 'sessionId)
             for path = (map-elt session 'cwd)
             for title = (map-elt session 'title)
             for updated = (or (map-elt session 'updatedAt) (map-elt session 'createdAt))
             when (and (stringp id) (not (string-blank-p id)) (not (gethash id seen))
                       (or (null path)
                           (and (stringp path) (file-name-absolute-p path)
                                (equal (directory-file-name path) (directory-file-name cwd)))))
             collect (progn
                       (puthash id t seen)
                       `((id . ,id)
                         ,@(when (stringp title) `((title . ,title)))
                         ,@(when (stringp updated) `((updated_at . ,updated))))))))

(cl-defun loki-assistant-agent-shell--history-init (original &rest args &key shell-buffer on-session-init)
  "让临时 SHELL-BUFFER 完成历史操作，其余会话交给 ORIGINAL。"
  (with-current-buffer shell-buffer
    (if (not (eq agent-shell-session-strategy 'loki-assistant-agent-shell--history-strategy))
        (apply original args)
      (let* ((state agent-shell--state)
             (id (map-elt state :resume-session-id))
             (cwd (agent-shell--resolve-path (agent-shell-cwd))))
        (if (not (and (map-elt state :supports-session-list)
                      (or (map-elt state :supports-session-load) (map-elt state :supports-session-resume))))
            (setq loki-assistant-agent-shell--history-result '((kind . "history_unsupported")))
          (agent-shell--list-sessions
           :state state :cwd cwd :buffer shell-buffer
           :on-failure (lambda (&rest _)
                         (setq loki-assistant-agent-shell--history-result '((kind . "history_failed"))))
           :on-success
           (lambda (sessions)
             (let* ((entries (loki-assistant-agent-shell--history-entries sessions cwd))
                    (entry (seq-find (lambda (entry) (equal (alist-get 'id entry) id)) entries)))
               (cond
                ((not id)
                 (setq loki-assistant-agent-shell--history-result `((kind . "listed") (sessions . ,(vconcat entries)))))
                ((not entry)
                 (setq loki-assistant-agent-shell--history-result '((kind . "history_session_not_found"))))
                (t
                 (agent-shell--emit-event :event 'session-selected :data `((:session-id . ,id)))
                 (agent-shell--initiate-session-resume-by-id
                  :session-id id :session-title (alist-get 'title entry) :shell-buffer shell-buffer
                  :on-session-init (lambda ()
                                     (funcall on-session-init)
                                     (setq loki-assistant-agent-shell--history-result
                                           `((kind . "created") (session_id . ,(buffer-name)))))
                  :on-failure (lambda ()
                                (setq loki-assistant-agent-shell--history-result '((kind . "failed")))))))))))))))

(advice-add 'agent-shell--initiate-session :around #'loki-assistant-agent-shell--history-init)

(defun loki-assistant-agent-shell--open-history-buffer (root agent-id history-id)
  "查找 ROOT 中已打开且对应 AGENT-ID 和 HISTORY-ID 的会话。"
  (seq-find (lambda (buffer)
              (with-current-buffer buffer
                (and (equal (loki-assistant-agent-shell--config-id (map-elt agent-shell--state :agent-config)) agent-id)
                     (equal (agent-shell-session-id) history-id)
                     (equal (directory-file-name default-directory) (directory-file-name root))
                     (process-live-p (map-nested-elt agent-shell--state '(:client :process))))))
            (agent-shell-buffers)))

(defun loki-assistant-agent-shell--history-failure (history-id)
  "返回查询历史或恢复 HISTORY-ID 失败的结果。"
  (if history-id '((kind . "failed")) '((kind . "history_failed"))))

(defun loki-assistant-agent-shell--request-history (project-id agent-id &optional history-id)
  "查询 PROJECT-ID 和 AGENT-ID 的历史，或恢复指定 HISTORY-ID。"
  (let* ((root (cdr (assoc project-id (loki-assistant-agent-shell--projects))))
         (config (and root (cdr (assoc agent-id (loki-assistant-agent-shell--agent-configs)))))
         (existing (and config history-id (loki-assistant-agent-shell--open-history-buffer root agent-id history-id)))
         buffer result)
    (cond
     ((not root) '((kind . "project_not_found")))
     ((not config) '((kind . "agent_not_found")))
     (existing `((kind . "created") (session_id . ,(buffer-name existing))))
     (t
      (unwind-protect
          (condition-case nil
              (let* ((default-directory root)
                     (project-current-directory-override root)
                     (deadline (+ (float-time) 7)))
                (setq buffer (agent-shell--start :config config :cwd root :no-focus t :new-session t
                                                 :session-id history-id
                                                 :session-strategy #'loki-assistant-agent-shell--history-strategy))
                (with-current-buffer buffer
                  (while (and (not loki-assistant-agent-shell--history-result)
                              (not (equal (car loki-assistant-agent-shell--outcome) "failed"))
                              (process-live-p (map-nested-elt agent-shell--state '(:client :process)))
                              (< (float-time) deadline))
                    (accept-process-output nil 0.05))
                  (setq result (or loki-assistant-agent-shell--history-result
                                   (if (>= (float-time) deadline)
                                       '((kind . "history_timeout"))
                                     (loki-assistant-agent-shell--history-failure history-id))))
                  (when (equal (alist-get 'kind result) "created")
                    (setq-local agent-shell-session-strategy 'new)
                    (loki-assistant-status-trigger-sync))
                  result))
            (error (loki-assistant-agent-shell--history-failure history-id)))
        (when (and (buffer-live-p buffer) (not (equal (alist-get 'kind result) "created")))
          (let ((kill-buffer-query-functions nil))
            (kill-buffer buffer))))))))

(defun loki-assistant-agent-shell-list-history (project-id agent-id)
  "返回 PROJECT-ID 和 AGENT-ID 的历史会话 JSON。"
  (json-serialize (loki-assistant-agent-shell--request-history project-id agent-id)))

(defun loki-assistant-agent-shell-resume-session (project-id agent-id history-id)
  "恢复 PROJECT-ID 和 AGENT-ID 的 HISTORY-ID，返回结果 JSON。"
  (json-serialize (loki-assistant-agent-shell--request-history project-id agent-id history-id)))

(provide 'loki-assistant-agent-shell-history)
