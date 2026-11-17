# Loki Assistant HarmonyOS 手机应用编译安装入口

DEVECO_HOME ?= /Applications/DevEco-Studio.app/Contents
DEVECO_SDK_HOME ?= $(DEVECO_HOME)/sdk
export DEVECO_SDK_HOME
HVIGORW := $(DEVECO_HOME)/tools/hvigor/bin/hvigorw
HDC := $(DEVECO_HOME)/sdk/default/openharmony/toolchains/hdc
HDC_TARGET ?=
HDC_ARGS = $(if $(HDC_TARGET),-t $(HDC_TARGET),)
BUNDLE_NAME := cc.loki.assistant.mobile
ABILITY_NAME := EntryAbility
HAP_FILE := entry/build/default/outputs/default/entry-default-unsigned.hap

.PHONY: build install run deploy devices clean help

build:
	"$(HVIGORW)" assembleHap --no-daemon

install: build
	@test -f "$(HAP_FILE)" || { echo "未找到 HAP: $(HAP_FILE)" >&2; exit 1; }
	"$(HDC)" $(HDC_ARGS) install -r "$(HAP_FILE)"

run:
	"$(HDC)" $(HDC_ARGS) shell aa start -a $(ABILITY_NAME) -b $(BUNDLE_NAME)

deploy: install run

devices:
	"$(HDC)" list targets

clean:
	"$(HVIGORW)" clean --no-daemon
	rm -rf entry/build build

help:
	@printf '%s\n' \
	  'make build    构建未签名 Debug HAP' \
	  'make install  安装到已连接的 HarmonyOS 设备' \
	  'make run      启动手机应用' \
	  'make deploy   构建、安装并启动' \
	  'make devices  查看 hdc 设备' \
	  'make clean    清理构建产物' \
	  'HDC_TARGET=<设备 ID> 可指定目标设备'
