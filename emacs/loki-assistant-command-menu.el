;;; loki-assistant-command-menu.el --- Loki Assistant 的命令菜单 -*- lexical-binding: t; -*-

;;; Code:

(require 'loki-assistant)

(defun loki-assistant-command-map ()
  "通过 Consult 选择并执行 Loki Assistant 命令。"
  (interactive)
  (when-let* ((command
               (loki-assistant--read-menu
                "Loki Assistant 命令: "
                '(("c 服务配置" loki-assistant-config "fuwupeizhi fwpz")
                  ("d 设备管理" loki-assistant-devices "shebeiguanli sbgl")
                  ("r 稍后阅读" loki-assistant-read-later "shaohouyuedu shyd")
                  ("u LLM 用量" loki-assistant-llm-usage "llmyongliang llmyl yl")))))
    (call-interactively command)))

(provide 'loki-assistant-command-menu)
;;; loki-assistant-command-menu.el ends here
