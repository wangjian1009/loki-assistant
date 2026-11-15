.DEFAULT_GOAL := help

PROJECT_ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))
BUILD_DIR ?= $(PROJECT_ROOT)/center/mac/.build
RELEASE_BUILD_DIR ?= $(BUILD_DIR)/release
CONFIG ?= Debug
CMAKE ?= cmake
CTEST ?= ctest
CARGO ?= cargo
OPEN ?= open

.PHONY: help configure build test test-mac services-test services-fmt services-run install-service uninstall-service start-service stop-service restart-service service-status run xcode release clean

help:
	@printf '%s\n' \
	  'make configure       生成 Mac 菜单栏应用 Xcode 工程' \
	  'make build           构建 Mac 菜单栏应用（默认 Debug）' \
	  'make run             构建并启动 Mac 菜单栏应用' \
	  'make xcode           生成并打开 Xcode 工程' \
	  'make test            执行 Mac XCTest 和 services Rust 测试' \
	  'make test-mac        构建并执行 Mac XCTest' \
	  'make services-test   执行 services Rust 测试' \
	  'make services-fmt    检查 services Rust 格式' \
	  'make services-run    运行 services 本地服务' \
	  'make install-service 注册 services 用户级 LaunchAgent' \
	  'make uninstall-service 取消注册 services 用户级 LaunchAgent' \
	  'make start-service   启动 services 用户级 LaunchAgent' \
	  'make stop-service    停止 services 用户级 LaunchAgent' \
	  'make restart-service 重启 services 用户级 LaunchAgent' \
	  'make service-status  查询 services 用户级 LaunchAgent 状态' \
	  'make release         构建 Release Mac 菜单栏应用' \
	  'make clean           清理构建产物' \
	  '' \
	  '可覆盖变量：CONFIG BUILD_DIR RELEASE_BUILD_DIR CMAKE CTEST CARGO OPEN'

configure:
	"$(CMAKE)" -S "$(PROJECT_ROOT)/center/mac" -B "$(BUILD_DIR)" -G Xcode -DBUILD_TESTING=ON

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
