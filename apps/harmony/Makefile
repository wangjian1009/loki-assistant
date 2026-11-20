# Loki Assistant HarmonyOS 手机应用编译安装入口
.DEFAULT_GOAL := build

DEVECO_HOME ?= /Applications/DevEco-Studio.app/Contents
DEVECO_SDK_HOME ?= $(DEVECO_HOME)/sdk
NODE_HOME := $(DEVECO_HOME)/tools/node
export DEVECO_SDK_HOME NODE_HOME
export PATH := $(NODE_HOME)/bin:$(PATH)
HVIGORW := $(DEVECO_HOME)/tools/hvigor/bin/hvigorw
OHPM := $(DEVECO_HOME)/tools/ohpm/bin/ohpm
HDC := $(DEVECO_HOME)/sdk/default/openharmony/toolchains/hdc
NODE := $(NODE_HOME)/bin/node
HDC_TARGET ?=
PAGE ?=
HTTP_API_ENABLED ?=
export PAGE HTTP_API_ENABLED
HDC_DEVICE = device_target="$$(sh scripts/select-hdc-target.sh "$(HDC)" "$(HDC_TARGET)")" && "$(HDC)" -t "$$device_target"
BUNDLE_NAME := cc.loki.assistant.mobile
ABILITY_NAME := EntryAbility
HAP_FILE := entry/build/default/outputs/default/entry-default-signed.hap

.PHONY: build deps check-sdk test install run deploy devices clean help

check-sdk:
	@test -d "$(DEVECO_SDK_HOME)" || { echo "未找到 HarmonyOS SDK：$(DEVECO_SDK_HOME)" >&2; exit 1; }
	@find "$(DEVECO_SDK_HOME)" -maxdepth 2 -name sdk-pkg.json -print -quit | grep -q . || { \
	  echo "HarmonyOS SDK 安装不完整，请在 DevEco Studio 的 SDK Manager 中安装 HarmonyOS 6.1.1（API 24）组件。" >&2; \
	  exit 1; \
	}

deps: check-sdk
	"$(OHPM)" install --all
	cd entry && "$(OHPM)" install

build: deps
	DEVECO_HOME="$(DEVECO_HOME)" sh scripts/build-device-network.sh
	rm -f "$(HAP_FILE)"
	"$(HVIGORW)" default@PackingCheck default@CollectDebugSymbol --mode module \
	  -p module=entry@default -p product=default -p buildMode=debug -p debuggable=true --no-daemon
	"$(NODE)" scripts/sign-hap.cjs "$(DEVECO_HOME)"

install: build
	@test -f "$(HAP_FILE)" || { echo "未找到已签名 HAP，请检查 signing/config.json 和本地私钥" >&2; exit 1; }
	$(HDC_DEVICE) install -r "$(HAP_FILE)"

run:
	@set --; \
	  if [ -n "$$PAGE" ]; then \
	    printf '%s\n' "$$PAGE" | grep -Eq '^(Index|Settings)(,(Index|Settings))*$$' || { \
	      echo 'PAGE 可指定 Index、Settings 或逗号分隔的页面路径，例如 Index,Settings' >&2; exit 1; \
	    }; \
	    set -- "$$@" --ps page "$$PAGE"; \
	  fi; \
	  case "$$HTTP_API_ENABLED" in \
	    '') ;; \
	    true|false) set -- "$$@" --pb httpApiEnabled "$$HTTP_API_ENABLED" ;; \
	    *) echo 'HTTP_API_ENABLED 可指定 true 或 false' >&2; exit 1 ;; \
	  esac; \
	  $(HDC_DEVICE) shell aa start -a $(ABILITY_NAME) -b $(BUNDLE_NAME) "$$@"

test:
	"$(NODE)" scripts/test-application.cjs "$(DEVECO_HOME)"
	cargo fmt --manifest-path entry/src/main/rust/Cargo.toml --check
	cargo test --locked --manifest-path entry/src/main/rust/Cargo.toml

# 固定同一台真机，按顺序完成安装和启动，支持并行 make。
deploy:
	@device_target="$$(sh scripts/select-hdc-target.sh "$(HDC)" "$(HDC_TARGET)" --physical)" || exit $$?; \
	  echo ">>> 部署到真机：$$device_target"; \
	  $(MAKE) install HDC_TARGET="$$device_target" && \
	  $(MAKE) run HDC_TARGET="$$device_target"

devices:
	"$(HDC)" list targets

clean:
	"$(HVIGORW)" clean --no-daemon
	rm -rf entry/build build
	rm -rf entry/libs entry/src/main/rust/target

help:
	@printf '%s\n' \
	  'make deps     检查 SDK 并安装 HarmonyOS 工程依赖' \
	  'make check-sdk 检查 HarmonyOS SDK 安装' \
	  'make build    构建 Debug HAP，使用 OrgTasksDebug 调试签名' \
	  'make test     检查并测试手机应用层与 Rust 网络模块' \
	  'make install  安装到已连接的 HarmonyOS 设备' \
	  'make run      启动手机应用，可指定 PAGE=Settings 或 PAGE=Index,Settings' \
	  'HTTP_API_ENABLED=true/false 可开启或关闭当前 Debug 会话的 HTTP 服务' \
	  'make deploy   检查真机，构建、安装并启动' \
	  'make devices  查看 hdc 设备' \
	  'make clean    清理构建产物' \
	  'HDC_TARGET=<设备 ID> 可指定目标设备；install/run 可显式选择模拟器，deploy 仅接受真机'
