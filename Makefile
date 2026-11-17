.DEFAULT_GOAL := help

PROJECT_ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))
BUILD_DIR ?= $(PROJECT_ROOT)/apps/center/mac/.build
RELEASE_BUILD_DIR ?= $(BUILD_DIR)/release
CONFIG ?= Debug
CMAKE ?= cmake
CTEST ?= ctest
CARGO ?= cargo
OPEN ?= open
EMACSCLIENT ?= emacsclient

.PHONY: emacs-load emacs-test emacs-config export-icons

.PHONY: help configure build test test-mac services-test services-fmt services-run install-service uninstall-service start-service stop-service restart-service service-status run restart-center xcode release clean

help:
	@printf '%s\n' \
	  'make configure       生成 Mac 菜单栏应用 Xcode 工程' \
	  'make build           构建 Mac 菜单栏应用（默认 Debug）' \
	  'make run             构建并启动 Mac 菜单栏应用' \
	  'make restart-center  重建并重启 Mac 菜单栏应用' \
	  'make xcode           生成并打开 Xcode 工程' \
	  'make test            执行 Mac XCTest 和 services Rust 测试' \
	  'make test-mac        构建并执行 Mac XCTest' \
	  'make emacs-config    打开 Emacs 服务配置管理界面' \
	  'make export-icons    从 SVG 母版导出应用和菜单栏图标' \
	  'make services-test   执行 services Rust 测试' \
	  'make services-fmt    检查 services Rust 格式' \
	  'make services-run    运行 services 本地服务' \
	  'make install-service 注册 services 用户级 LaunchAgent' \
	  'make uninstall-service 取消注册 services 用户级 LaunchAgent' \
	  'make start-service   启动 services 用户级 LaunchAgent' \
	  'make stop-service    停止 services 用户级 LaunchAgent' \
	  'make restart-service 重启 services 用户级 LaunchAgent' \
	  'make service-status  查询 services 用户级 LaunchAgent 状态' \
	  'make emacs-load      通过 emacsclient 重新加载 Emacs 包' \
	  'make emacs-test      通过 emacsclient 执行 Emacs ERT 测试' \
	  'make release         构建 Release Mac 菜单栏应用' \
	  'make clean           清理构建产物' \
	  '' \
	  '可覆盖变量：CONFIG BUILD_DIR RELEASE_BUILD_DIR CMAKE CTEST CARGO OPEN EMACSCLIENT'

emacs-load:
	"$(EMACSCLIENT)" --eval "(progn \
	  (add-to-list 'load-path \"$(PROJECT_ROOT)/emacs\") \
	  (dolist (feature '(loki-assistant-config-tests loki-assistant-config loki-assistant)) \
	    (when (featurep feature) (unload-feature feature t))) \
	  (require 'loki-assistant) \
	  (require 'loki-assistant-config))"

emacs-config: emacs-load
	"$(EMACSCLIENT)" --eval "(progn \
	  (loki-assistant-config) \
	  t)"

emacs-test: emacs-load
	"$(EMACSCLIENT)" --eval "(progn \
	  (load \"$(PROJECT_ROOT)/emacs/loki-assistant-tests.el\" nil t) \
	  (load \"$(PROJECT_ROOT)/emacs/loki-assistant-config-tests.el\" nil t) \
	  (let ((stats (ert-run-tests-batch '(or \"^loki-assistant-test-\" \"^loki-assistant-config-test-\"))) \
	        (agent-shell-stats \
	         (when (require 'agent-shell nil t) \
	           (load \"$(PROJECT_ROOT)/emacs/loki-assistant-agent-shell-tests.el\" nil t) \
	           (ert-run-tests-batch \"^loki-assistant-agent-shell-\")))) \
	    (if (zerop (+ (ert-stats-completed-unexpected stats) \
	                  (if agent-shell-stats \
                      (ert-stats-completed-unexpected agent-shell-stats) 0))) \
	        (format \"通过 %d 项 Emacs 测试%s\" \
	                (+ (ert-stats-total stats) \
	                   (if agent-shell-stats (ert-stats-total agent-shell-stats) 0)) \
	                (if agent-shell-stats \"（含 agent-shell 扩展）\" \"\")) \
	      (error \"Emacs 测试失败\"))))"

export-icons:
	python3 "$(PROJECT_ROOT)/apps/center/mac/scripts/export-icons.py"

configure:
	"$(CMAKE)" -S "$(PROJECT_ROOT)/apps/center/mac" -B "$(BUILD_DIR)" -G Xcode -DBUILD_TESTING=ON

build: configure
	"$(CMAKE)" --build "$(BUILD_DIR)" --config "$(CONFIG)"

test: test-mac services-test

test-mac: build
	"$(CTEST)" --test-dir "$(BUILD_DIR)" -C "$(CONFIG)" --output-on-failure

services-test:
	"$(CARGO)" test --workspace --manifest-path "$(PROJECT_ROOT)/services/Cargo.toml"

services-fmt:
	"$(CARGO)" fmt --manifest-path "$(PROJECT_ROOT)/services/Cargo.toml" -- --check

services-run:
	"$(CARGO)" run --manifest-path "$(PROJECT_ROOT)/services/Cargo.toml"

install-service:
	"$(CARGO)" run --quiet --manifest-path "$(PROJECT_ROOT)/services/Cargo.toml" -- install-service

uninstall-service:
	"$(CARGO)" run --quiet --manifest-path "$(PROJECT_ROOT)/services/Cargo.toml" -- uninstall-service

start-service:
	"$(CARGO)" run --quiet --manifest-path "$(PROJECT_ROOT)/services/Cargo.toml" -- start-service

stop-service:
	"$(CARGO)" run --quiet --manifest-path "$(PROJECT_ROOT)/services/Cargo.toml" -- stop-service

restart-service:
	"$(CARGO)" run --quiet --manifest-path "$(PROJECT_ROOT)/services/Cargo.toml" -- restart-service

service-status:
	"$(CARGO)" run --quiet --manifest-path "$(PROJECT_ROOT)/services/Cargo.toml" -- service-status

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

clean:
	@if [ -f "$(BUILD_DIR)/CMakeCache.txt" ]; then \
	  "$(CMAKE)" --build "$(BUILD_DIR)" --config "$(CONFIG)" --target clean; \
	fi
	@if [ -f "$(RELEASE_BUILD_DIR)/CMakeCache.txt" ]; then \
	  "$(CMAKE)" --build "$(RELEASE_BUILD_DIR)" --config Release --target clean; \
	fi
