;;; loki-assistant-dispatch.el --- services 到 Emacs 的转发入口 -*- lexical-binding: t; -*-

;;; Code:

(declare-function loki-assistant-slack-dispatch-messages "loki-assistant-slack-chat")
(declare-function loki-assistant-slack-open-notification "loki-assistant-slack-chat")
(declare-function loki-assistant-slack-activate "loki-assistant-slack-chat")

(defun loki-assistant-dispatch-slack-messages (payload-file)
  "将 PAYLOAD-FILE 中的消息转发给已有的 Slack 订阅。"
  (if (featurep 'loki-assistant-slack-chat)
      (loki-assistant-slack-dispatch-messages payload-file)
    'buffer-closed))

(defun loki-assistant-dispatch-slack-notification (target)
  "将 Slack 通知的 TARGET 转发到对应聊天窗口。"
  (when (require 'loki-assistant-slack-chat nil t)
    (loki-assistant-slack-open-notification target)))

(defun loki-assistant-dispatch-activate-slack ()
  "处理托盘的 Slack 聊天激活请求。"
  (when (require 'loki-assistant-slack-chat nil t)
    (loki-assistant-slack-activate)
    t))

(provide 'loki-assistant-dispatch)
;;; loki-assistant-dispatch.el ends here
