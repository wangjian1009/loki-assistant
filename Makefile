.DEFAULT_GOAL := help

PROJECT_ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))
BUILD_DIR ?= $(PROJECT_ROOT)/center/mac/.build
RELEASE_BUILD_DIR ?= $(BUILD_DIR)/release
CONFIG ?= Debug
CMAKE ?= cmake
CTEST ?= ctest
EMACSCLIENT ?= emacsclient
OPEN ?= open

.PHONY: help configure build test test-mac test-emacs run xcode release clean

help:
	@printf '%s\n' \
	  'make configure   生成 Xcode 工程' \
	  'make build       构建 Mac 应用（默认 Debug）' \
	  'make test        执行 Mac XCTest 和 Emacs ERT' \
	  'make test-mac    构建并执行 Mac XCTest' \
	  'make test-emacs  通过已有 Emacs server 执行 ERT' \
	  'make run         构建并启动 Mac 应用' \
	  'make xcode       生成并打开 Xcode 工程' \
	  'make release     构建不含测试的 Release 应用' \
	  'make clean       清理当前配置及 Release 产物，保留生成工程' \
	  '' \
	  '可覆盖变量：CONFIG BUILD_DIR RELEASE_BUILD_DIR CMAKE CTEST EMACSCLIENT OPEN'

configure:
	"$(CMAKE)" -S "$(PROJECT_ROOT)/center/mac" -B "$(BUILD_DIR)" -G Xcode -DBUILD_TESTING=ON

build: configure
	"$(CMAKE)" --build "$(BUILD_DIR)" --config "$(CONFIG)"

test: test-mac test-emacs

test-mac: build
	"$(CTEST)" --test-dir "$(BUILD_DIR)" -C "$(CONFIG)" --output-on-failure

test-emacs:
	"$(EMACSCLIENT)" --alternate-editor=false --timeout=30 --eval \
	  "$$(printf '\n  (load "%s" nil t)\n' '$(PROJECT_ROOT)/emacs/test/run-tests.el')"

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
