;;; loki-assistant-xbloom-service-tests.el --- xBloom 配方回归测试 -*- lexical-binding: t; -*-

(require 'ert)
(require 'loki-assistant-doc-tests)
(require 'loki-assistant-xbloom-service)

(defun loki-assistant-xbloom-service-test--request (json)
  "通过与 services 相同的入口发送 JSON 并解析响应。"
  (let ((file (make-temp-file "loki-xbloom-test-")))
    (unwind-protect
        (progn
          (let ((coding-system-for-write 'utf-8-unix))
            (with-temp-file file (insert json)))
          (json-parse-string
           (decode-coding-string
            (base64-decode-string (loki-assistant-xbloom-service-request file)) 'utf-8)
           :object-type 'alist :null-object nil))
      (delete-file file))))

(ert-deftest loki-assistant-xbloom-service-test-org-roundtrip ()
  (loki-assistant-doc-test--with-library
    (let* ((org-roam-db-update-on-save nil)
           (org-roam-dailies-directory "journal/")
           (org-roam-dailies-capture-templates
            '(("d" "日记" entry "* %?"
               :target (file+head+olp "%<%Y>/%<%m>/%<%F>.org" "#+TITLE: 日记\n"
                                      ("冲煮记录")))))
           (library (expand-file-name "recipes.org" root))
           (daily (expand-file-name (format-time-string "journal/%Y/%m/%F.org") root))
           (request #'loki-assistant-xbloom-service-test--request))
      (with-temp-file library
        (insert "* [[https://example.org/r][官方咖啡]] :15克:\n:PROPERTIES:\n"
                ":ID: official\n:TABLE_ID: 123\n:MODEL: Studio\n:END:\n\n"
                "- 产地 :: 埃塞俄比亚\n\n** 冲煮参数\n"
                "| 参数 | 值 |\n|-\n| 粉量 (g) | 15.0 |\n| 粉水比 | 1:17 |\n"
                "** 分段注水\n"
                "| 段 | 段前振动 | 水量 (mL) | 轨迹 |\n|-\n"
                "| 1 | 关闭 | 50 | 螺旋 |\n| 2 | 开启 | 100 | 中心 |\n"))
      (let* ((official (alist-get 'result (funcall request "{\"operation\":\"get\",\"id\":\"official\"}")))
             (data (alist-get 'data official))
             (pours (alist-get 'pours data)))
        (should (equal (alist-get 'table_id official) "123"))
        (should (equal (alist-get 'model data) "studio"))
        (should (equal (alist-get 'origin data) "埃塞俄比亚"))
        (should (= (alist-get 'water_ratio (alist-get 'brewing data)) 17))
        (should (eq (alist-get 'vibrate_before (aref pours 0)) :false))
        (should (eq (alist-get 'vibrate_before (aref pours 1)) t))
        (should (= (alist-get 'volume_ml (aref pours 1)) 100))
        (should (equal (alist-get 'error
                                  (funcall request "{\"operation\":\"update\",\"id\":\"official\",\"recipe\":{\"title\":\"测试\"}}"))
                       "recipe-read-only")))
      (let* ((created (alist-get
                       'result
                       (funcall request
                                (concat "{\"operation\":\"create\",\"info\":{\"based_on_id\":\"official\","
                                        "\"recipe\":{\"title\":\"手冲咖啡 %(literal)\",\"tags\":[\"手冲\"],"
                                        "\"origin\":\"云南\",\"brewing\":{\"dose_g\":15.5,\"grinder_enabled\":false},"
                                        "\"pours\":[{\"volume_ml\":51,\"pattern\":\"spiral\",\"pause_s\":0}]}}}"))))
             (recipe (alist-get 'recipe created))
             (id (alist-get 'id recipe))
             (data (alist-get 'data recipe))
             (brew (alist-get 'brewing data)))
        (should (eq (alist-get 'indexed created) t))
        (should (file-regular-p daily))
        (should (equal (alist-get 'based_on_id recipe) "official"))
        (should (= (alist-get 'dose_g brew) 15.5))
        (should (eq (alist-get 'grinder_enabled brew) :false))
        (should (= (alist-get 'pause_s (aref (alist-get 'pours data) 0)) 0))
        (with-current-buffer (find-file-noselect daily)
          (goto-char (point-max))
          (insert "\n风味笔记保留。\n")
          (should (equal
                   (alist-get 'error
                              (funcall request (json-serialize
                                                `((operation . "update") (id . ,id)
                                                  (recipe . ((title . "暂存") (pours . [])))))))
                   "daily-conflict"))
          (save-buffer))
        (let* ((updated (alist-get 'result
                                   (funcall request (json-serialize
                                                     `((operation . "update") (id . ,id)
                                                       (recipe . ((title . "修改后的咖啡") (pours . []))))))))
               (saved (alist-get 'recipe updated))
               (saved-data (alist-get 'data saved)))
          (should (equal (alist-get 'id saved) id))
          (should (equal (alist-get 'title saved-data) "修改后的咖啡"))
          (should-not (alist-get 'origin saved-data))
          (should (equal (alist-get 'pours saved-data) []))
          (with-temp-buffer
            (insert-file-contents daily)
            (goto-char (point-min))
            (should (search-forward "风味笔记保留。" nil t))))
        (let ((page (alist-get 'result
                               (funcall request "{\"operation\":\"list\",\"q\":\"咖啡\",\"limit\":1,\"offset\":0}")))
              (next (alist-get 'result
                               (funcall request "{\"operation\":\"list\",\"q\":\"咖啡\",\"limit\":1,\"offset\":1}"))))
          (should (equal (alist-get 'id (aref (alist-get 'recipes page) 0)) id))
          (should (= (alist-get 'next_offset page) 1))
          (should (equal (alist-get 'id (aref (alist-get 'recipes next) 0)) "official"))
          (should-not (alist-get 'next_offset next)))))))

(provide 'loki-assistant-xbloom-service-tests)
;;; loki-assistant-xbloom-service-tests.el ends here
