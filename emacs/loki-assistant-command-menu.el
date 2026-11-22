;;; loki-assistant-command-menu.el --- Loki Assistant 的命令菜单 -*- lexical-binding: t; -*-

;;; Code:

(require 'hydra)
(require 'loki-assistant)

(defhydra loki-assistant-command-menu (:exit t)
  "Loki Assistant"
  ("c" loki-assistant-config "服务配置")
  ("d" loki-assistant-devices "设备管理")
  ("q" nil "退出"))

(defalias 'loki-assistant-command-map #'loki-assistant-command-menu/body
  "打开 Loki Assistant 的 Hydra 命令菜单。")

(provide 'loki-assistant-command-menu)
;;; loki-assistant-command-menu.el ends here
