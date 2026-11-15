.DEFAULT_GOAL := help

PROJECT_ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))
BUILD_DIR ?= $(PROJECT_ROOT)/center/mac/.build
RELEASE_BUILD_DIR ?= $(BUILD_DIR)/release
CONFIG ?= Debug
CMAKE ?= cmake
CTEST ?= ctest
CARGO ?= cargo
EMACSCLIENT ?= emacsclient
EMACS_SOCKET ?=
OPEN ?= open

.PHONY: help configure build test test-mac test-emacs services-test services-fmt services-run test-connection run xcode release clean

help:
	@printf '%s\n' \
	  'make configure   生成 Xcode 工程' \
	  'make build       构建 Mac 应用（默认 Debug）' \
	  'make test        执行 Mac XCTest、Emacs ERT 和 services Rust 测试' \
	  'make test-mac    构建并执行 Mac XCTest' \
	  'make test-emacs  通过已有 Emacs server 执行 ERT' \
	  'make services-test 执行 services Rust 测试' \
	  'make services-fmt  检查 services Rust 格式' \
	  'make services-run  运行 services 本地服务' \
	  'make test-connection 加载插件并验证真实连接、会话列表与点击激活' \
	  'make run         构建并启动 Mac 应用' \
	  'make xcode       生成并打开 Xcode 工程' \
	  'make release     构建不含测试的 Release 应用' \
	  'make clean       清理当前配置及 Release 产物，保留生成工程' \
	  '' \
	  '可覆盖变量：CONFIG BUILD_DIR RELEASE_BUILD_DIR CMAKE CTEST CARGO EMACSCLIENT EMACS_SOCKET OPEN'

configure:
	"$(CMAKE)" -S "$(PROJECT_ROOT)/center/mac" -B "$(BUILD_DIR)" -G Xcode -DBUILD_TESTING=ON

build: configure
	"$(CMAKE)" --build "$(BUILD_DIR)" --config "$(CONFIG)"

test: test-mac test-emacs services-test

test-mac: build
	"$(CTEST)" --test-dir "$(BUILD_DIR)" -C "$(CONFIG)" --output-on-failure

test-emacs:
	"$(EMACSCLIENT)" --alternate-editor=false --timeout=30 $(if $(strip $(EMACS_SOCKET)),--socket-name "$(EMACS_SOCKET)",) --eval \
	  "$$(printf '\n  (load "%s" nil t)\n' '$(PROJECT_ROOT)/emacs/test/run-tests.el')"

services-test:
	"$(CARGO)" test --workspace --manifest-path "$(PROJECT_ROOT)/services/Cargo.toml"

services-fmt:
	"$(CARGO)" fmt --manifest-path "$(PROJECT_ROOT)/services/Cargo.toml" -- --check

services-run:
	"$(CARGO)" run --manifest-path "$(PROJECT_ROOT)/services/Cargo.toml"

test-connection: build
	"$(EMACSCLIENT)" --alternate-editor=false --timeout=5 $(if $(strip $(EMACS_SOCKET)),--socket-name "$(EMACS_SOCKET)",) --eval \
	  "$$(printf '\n  (progn\n    (add-to-list (quote load-path) "%s")\n    (load "loki-assistant-protocol" nil t)\n    (load "loki-assistant-agent-shell" nil t)\n    (load "loki-assistant" nil t)\n    loki-assistant-version)\n' '$(PROJECT_ROOT)/emacs/lisp')"
	TEST_RUNNER_LOKI_TEST_CONNECTION=1 \
	TEST_RUNNER_LOKI_EMACSCLIENT="$$(command -v "$(EMACSCLIENT)")" \
	TEST_RUNNER_LOKI_EMACS_SOCKET="$(EMACS_SOCKET)" \
	  "$(CTEST)" --test-dir "$(BUILD_DIR)" -C "$(CONFIG)" --output-on-failure -V

run: build
	"$(OPEN)" "$(BUILD_DIR)/$(CONFIG)/LokiAssistant.app"

xcode: configure
	"$(OPEN)" "$(BUILD_DIR)/LokiAssistant.xcodeproj"

release:
	"$(CMAKE)" -S "$(PROJECT_ROOT)/center/mac" -B "$(RELEASE_BUILD_DIR)" -G Xcode -DBUILD_TESTING=OFF
	"$(CMAKE)" --build "$(RELEASE_BUILD_DIR)" --config Release

clean:
	@if [ -f "$(BUILD_DIR)/CMakeCache.txt" ]; then \
	  "$(CMAKE)" --build "$(BUILD_DIR)" --config "$(CONFIG)" --target clean; \
	fi
	@if [ -f "$(RELEASE_BUILD_DIR)/CMakeCache.txt" ]; then \
	  "$(CMAKE)" --build "$(RELEASE_BUILD_DIR)" --config Release --target clean; \
	fi
