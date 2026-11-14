;;; run-tests.el --- Run Loki Assistant ERT in an existing server -*- lexical-binding: t; -*-

;;; Commentary:
;; Loaded by make test-emacs.  Signal failures to emacsclient without exiting
;; the user's Emacs.  Resolve the repository from this file, not default-directory.

;;; Code:

(require 'ert)

(let* ((test-directory (file-name-directory
                        (or load-file-name (error "Load this file to run tests"))))
       (load-path (cons (expand-file-name "../lisp" test-directory) load-path)))
  ;; Reload production code so repeat runs test current source, not cached features.
  (load "loki-assistant-protocol" nil t)
  (load "loki-assistant-agent-shell" nil t)
  (load "loki-assistant" nil t)
  (load (expand-file-name "loki-assistant-protocol-test.el" test-directory) nil t)
  (let* ((stats (ert-run-tests-batch "^loki-assistant-"))
         (failed (ert-stats-completed-unexpected stats)))
    (unless (zerop failed)
      (error "Loki Assistant: %d ERT test(s) failed" failed))
    (message "Loki Assistant: %d ERT tests passed"
             (ert-stats-completed-expected stats))))

;;; run-tests.el ends here
