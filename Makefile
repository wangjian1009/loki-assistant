.DEFAULT_GOAL := help

PROJECT_ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))
BUILD_DIR ?= $(PROJECT_ROOT)/apps/center/mac/.build
RELEASE_BUILD_DIR ?= $(BUILD_DIR)/release
TEST_BUILD_DIR ?= $(BUILD_DIR)/tests
CONFIG ?= Debug
CMAKE ?= cmake
CTEST ?= ctest
CARGO ?= cargo
BREW ?= brew
OPEN ?= open
EMACSCLIENT ?= emacsclient

.PHONY: emacs-deps emacs-load emacs-test emacs-config emacs-devices chrome-build chrome-test export-icons
.PHONY: relay-image harmony-test

.PHONY: help configure build test test-mac services-test services-fmt services-run install-service uninstall-service start-service stop-service restart-service service-status run restart-center xcode release deploy harmony-help harmony-deps harmony-check-sdk harmony-build harmony-install harmony-run harmony-deploy harmony-devices harmony-clean clean

help:
	@printf '%s\n' \
	  'make configure       生成 Mac 菜单栏应用 Xcode 工程' \
	  'make build           构建 Mac 菜单栏应用（默认 Debug）' \
	  'make run             构建并启动 Mac 菜单栏应用' \
	  'make restart-center  重建并重启 Mac 菜单栏应用' \
	  'make xcode           生成并打开 Xcode 工程' \
	  'make test            执行 Mac XCTest、services Rust 和 Chrome 插件测试' \
	  'make test-mac        构建并执行 Mac XCTest' \
	  'make emacs-config    打开 Emacs 服务配置管理界面' \
	  'make emacs-devices   打开 Emacs 已绑定设备管理界面' \
	  'make chrome-build    构建 Chrome 插件' \
	  'make chrome-test     构建并执行 Chrome 插件测试' \
	  'make export-icons    从公共 SVG 母版导出 macOS、HarmonyOS 和 Chrome 图标' \
	  'make services-test   执行 services Rust 测试' \
	  'make services-fmt    检查 services Rust 格式' \
	  'make services-run    运行 services 本地服务' \
	  'make relay-image     构建 ECS 使用的固定版本 iroh-relay Docker 镜像' \
	  'make install-service 构建并注册 services 到 brew services' \
	  'make uninstall-service 停止并卸载 services Homebrew formula' \
	  'make start-service   通过 brew services 启动 services' \
	  'make stop-service    通过 brew services 停止 services' \
	  'make restart-service 重建、更新并重启 services Homebrew 服务' \
	  'make service-status  查询 brew services 中的 services 状态' \
	  'make emacs-deps      安装 Emacs 包声明的依赖' \
	  'make emacs-load      安装缺失依赖并通过 emacsclient 重新加载 Emacs 包' \
	  'make emacs-test      通过 emacsclient 执行 Emacs ERT 测试' \
	  'make release         构建 Release Mac 菜单栏应用' \
	  'make deploy          检查 HarmonyOS 真机，构建、安装并启动' \
	  'make harmony-deps    检查 SDK 并安装 HarmonyOS 工程依赖' \
	  'make harmony-check-sdk 检查 HarmonyOS SDK 安装' \
	  'make harmony-build   构建 HarmonyOS 手机应用并使用 OrgTasksDebug 签名' \
	  'make harmony-test    检查并测试 HarmonyOS 应用层与 Rust 网络模块' \
	  'make harmony-install 构建并安装 HarmonyOS 手机应用' \
	  'make harmony-run     启动 HarmonyOS 手机应用，可指定 PAGE=Devices、PAGE=Index,Settings 和 HTTP_API_ENABLED=true/false' \
	  'make harmony-deploy  同 make deploy，检查真机后构建、安装并启动' \
	  'make harmony-devices 查看 HarmonyOS hdc 设备' \
	  'make harmony-clean   清理 HarmonyOS 构建产物' \
	  'make clean           清理构建产物' \
	  '' \
	  '可覆盖变量：CONFIG BUILD_DIR RELEASE_BUILD_DIR TEST_BUILD_DIR CMAKE CTEST CARGO BREW OPEN EMACSCLIENT DEVECO_HOME HDC_TARGET'

emacs-deps:
	"$(EMACSCLIENT)" --eval "(progn \
	  (require 'package) \
	  (unless package--initialized (package-initialize)) \
	  (let* ((requirements (with-temp-buffer \
	                         (insert-file-contents \"$(PROJECT_ROOT)/emacs/loki-assistant.el\") \
	                         (package-desc-reqs (package-buffer-info)))) \
	         (missing (seq-remove (lambda (requirement) (apply #'package-installed-p requirement)) \
	                              requirements))) \
	    (when missing \
	      (let ((package-archives (copy-tree package-archives))) \
	        (unless (assoc \"nongnu\" package-archives) \
	          (push '(\"nongnu\" . \"https://elpa.nongnu.org/nongnu/\") package-archives)) \
	        (package-read-all-archive-contents) \
	        (package-download-transaction \
	         (or (ignore-errors (package-compute-transaction nil missing)) \
	             (progn \
	               (package-refresh-contents) \
	               (package-compute-transaction nil missing)))) \
	        (dolist (requirement missing) \
	          (unless (apply #'package-installed-p requirement) \
	            (error \"Emacs 依赖安装失败：%s\" (car requirement))))))) \
	  t)"

emacs-load: emacs-deps
	"$(EMACSCLIENT)" --eval "(progn \
	  (let ((directory \"$(PROJECT_ROOT)/emacs\")) \
	    (setq load-path (cons directory (delete directory load-path)))) \
	  (dolist (feature '(loki-assistant-llm-usage-tests loki-assistant-llm-usage loki-assistant-slack-chat-tests loki-assistant-devices-tests loki-assistant-devices loki-assistant-doc-tests loki-assistant-read-later loki-assistant-doc loki-assistant-capture-tests loki-assistant-capture loki-assistant-edna-tests loki-assistant-edna loki-assistant-monitor-tests loki-assistant-config-tests loki-assistant-config loki-assistant-monitor loki-assistant-command-menu loki-assistant)) \
	    (when (featurep feature) (unload-feature feature t))) \
	  (require 'loki-assistant) \
	  (require 'loki-assistant-config) \
	  (require 'loki-assistant-doc) \
	  (require 'loki-assistant-read-later) \
	  (require 'loki-assistant-devices) \
	  (require 'loki-assistant-llm-usage) \
	  (when (featurep 'loki-assistant-mail) \
	    (load \"$(PROJECT_ROOT)/emacs/loki-assistant-mail.el\" nil t)) \
	  (if (featurep 'loki-assistant-slack-chat) \
	      (load \"$(PROJECT_ROOT)/emacs/loki-assistant-slack-chat.el\" nil t) \
	    (require 'loki-assistant-slack-chat)))"

emacs-config: emacs-load
	"$(EMACSCLIENT)" --eval "(progn \
	  (loki-assistant-config) \
	  t)"

emacs-devices: emacs-load
	"$(EMACSCLIENT)" --eval "(progn \
	  (loki-assistant-devices) \
	  t)"

emacs-test: emacs-load
	"$(EMACSCLIENT)" --eval "(let ((monitor-enabled loki-assistant-monitor-mode)) \
	  (unwind-protect \
	      (progn \
	        (loki-assistant-monitor-mode -1) \
	        (load \"$(PROJECT_ROOT)/emacs/loki-assistant-doc-tests.el\" nil t) \
	        (load \"$(PROJECT_ROOT)/emacs/loki-assistant-tests.el\" nil t) \
	        (load \"$(PROJECT_ROOT)/emacs/loki-assistant-config-tests.el\" nil t) \
	        (load \"$(PROJECT_ROOT)/emacs/loki-assistant-devices-tests.el\" nil t) \
	        (load \"$(PROJECT_ROOT)/emacs/loki-assistant-monitor-tests.el\" nil t) \
	        (load \"$(PROJECT_ROOT)/emacs/loki-assistant-llm-usage-tests.el\" nil t) \
	        (load \"$(PROJECT_ROOT)/emacs/loki-assistant-slack-chat-tests.el\" nil t) \
	        (let ((stats (ert-run-tests-batch '(or \"^loki-assistant-llm-usage-test-\" \"^loki-assistant-test-\" \"^loki-assistant-config-test-\" \"^loki-assistant-devices-test-\" \"^loki-assistant-monitor-test-\" \"^loki-assistant-doc-test-\" \"^loki-assistant-slack-chat-test-\"))) \
	              (edna-stats \
	               (when (require 'org-edna nil t) \
	                 (load \"$(PROJECT_ROOT)/emacs/loki-assistant-edna-tests.el\" nil t) \
	                 (ert-run-tests-batch \"^loki-assistant-edna-test-\"))) \
	              (agent-shell-stats \
	               (when (require 'agent-shell nil t) \
	                 (load \"$(PROJECT_ROOT)/emacs/loki-assistant-agent-shell-tests.el\" nil t) \
	                 (ert-run-tests-batch \"^loki-assistant-agent-shell-\")))) \
	          (if (zerop (+ (ert-stats-completed-unexpected stats) \
	                        (if edna-stats (ert-stats-completed-unexpected edna-stats) 0) \
	                        (if agent-shell-stats (ert-stats-completed-unexpected agent-shell-stats) 0))) \
	              (format \"通过 %d 项 Emacs 测试\" \
	                      (+ (ert-stats-total stats) (if edna-stats (ert-stats-total edna-stats) 0) \
	                         (if agent-shell-stats (ert-stats-total agent-shell-stats) 0))) \
	            (error \"Emacs 测试失败\")))) \
	    (when monitor-enabled (loki-assistant-monitor-mode 1))))"

chrome-build:
	npm --prefix "$(PROJECT_ROOT)/chrome-plugin" run build

chrome-test:
	npm --prefix "$(PROJECT_ROOT)/chrome-plugin" test

export-icons:
	python3 "$(PROJECT_ROOT)/scripts/export-icons.py"

configure:
	"$(CMAKE)" -S "$(PROJECT_ROOT)/apps/center/mac" -B "$(BUILD_DIR)" -G Xcode -DBUILD_TESTING=ON

build: configure
	"$(CMAKE)" --build "$(BUILD_DIR)" --config "$(CONFIG)"

test: test-mac services-test chrome-test

test-mac:
	"$(CMAKE)" -S "$(PROJECT_ROOT)/apps/center/mac" -B "$(TEST_BUILD_DIR)" -G Xcode -DBUILD_TESTING=ON -DLOKI_APP_BUNDLE_IDENTIFIER=cc.loki-assistant.center.testhost -DLOKI_APP_URL_SCHEME=loki-assistant-testhost -DLOKI_APP_REGISTER_MAILTO=OFF
	"$(CMAKE)" --build "$(TEST_BUILD_DIR)" --config "$(CONFIG)"
	"$(CTEST)" --test-dir "$(TEST_BUILD_DIR)" -C "$(CONFIG)" --output-on-failure

services-test:
	"$(CARGO)" test --workspace --manifest-path "$(PROJECT_ROOT)/services/Cargo.toml"

services-fmt:
	"$(CARGO)" fmt --manifest-path "$(PROJECT_ROOT)/services/Cargo.toml" -- --check

services-run:
	"$(CARGO)" run --manifest-path "$(PROJECT_ROOT)/services/Cargo.toml"

relay-image:
	python3 "$(PROJECT_ROOT)/scripts/build-relay-image.py"

install-service:
	python3 "$(PROJECT_ROOT)/scripts/homebrew-service.py" install --brew "$(BREW)" --cargo "$(CARGO)"

uninstall-service:
	python3 "$(PROJECT_ROOT)/scripts/homebrew-service.py" uninstall --brew "$(BREW)" --cargo "$(CARGO)"

start-service:
	python3 "$(PROJECT_ROOT)/scripts/homebrew-service.py" start --brew "$(BREW)" --cargo "$(CARGO)"

stop-service:
	python3 "$(PROJECT_ROOT)/scripts/homebrew-service.py" stop --brew "$(BREW)" --cargo "$(CARGO)"

restart-service:
	python3 "$(PROJECT_ROOT)/scripts/homebrew-service.py" restart --brew "$(BREW)" --cargo "$(CARGO)"

service-status:
	python3 "$(PROJECT_ROOT)/scripts/homebrew-service.py" status --brew "$(BREW)" --cargo "$(CARGO)"

run: build
	"$(OPEN)" "$(BUILD_DIR)/$(CONFIG)/LokiAssistant.app"

restart-center: build
	@for center_pid in $$(pgrep -x LokiAssistant 2>/dev/null || true); do \
	  kill "$$center_pid" 2>/dev/null || true; \
	done
	@sleep 1
	"$(OPEN)" -n "$(BUILD_DIR)/$(CONFIG)/LokiAssistant.app"

xcode: configure
	"$(OPEN)" "$(BUILD_DIR)/LokiAssistant.xcodeproj"

release:
	"$(CMAKE)" -S "$(PROJECT_ROOT)/apps/center/mac" -B "$(RELEASE_BUILD_DIR)" -G Xcode -DBUILD_TESTING=OFF
	"$(CMAKE)" --build "$(RELEASE_BUILD_DIR)" --config Release

harmony-help:
	$(MAKE) -C "$(PROJECT_ROOT)/apps/harmony" help

harmony-deps:
	$(MAKE) -C "$(PROJECT_ROOT)/apps/harmony" deps

harmony-check-sdk:
	$(MAKE) -C "$(PROJECT_ROOT)/apps/harmony" check-sdk

harmony-build:
	$(MAKE) -C "$(PROJECT_ROOT)/apps/harmony" build

harmony-test:
	$(MAKE) -C "$(PROJECT_ROOT)/apps/harmony" test

harmony-install:
	$(MAKE) -C "$(PROJECT_ROOT)/apps/harmony" install

harmony-run:
	$(MAKE) -C "$(PROJECT_ROOT)/apps/harmony" run

deploy: harmony-deploy

harmony-deploy:
	$(MAKE) -C "$(PROJECT_ROOT)/apps/harmony" deploy

harmony-devices:
	$(MAKE) -C "$(PROJECT_ROOT)/apps/harmony" devices

harmony-clean:
	@if [ -f "$(TEST_BUILD_DIR)/CMakeCache.txt" ]; then \
	  "$(CMAKE)" --build "$(TEST_BUILD_DIR)" --config "$(CONFIG)" --target clean; \
	fi
	$(MAKE) -C "$(PROJECT_ROOT)/apps/harmony" clean

clean:
	@if [ -f "$(BUILD_DIR)/CMakeCache.txt" ]; then \
	  "$(CMAKE)" --build "$(BUILD_DIR)" --config "$(CONFIG)" --target clean; \
	fi
	@if [ -f "$(RELEASE_BUILD_DIR)/CMakeCache.txt" ]; then \
	  "$(CMAKE)" --build "$(RELEASE_BUILD_DIR)" --config Release --target clean; \
	fi
