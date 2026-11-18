#!/usr/bin/env python3
"""在独立模拟器上验证真实 iroh 中继、双向消息、身份保持与中继不可用。"""

import argparse
import json
import pathlib
import re
import socket
import subprocess
import time
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[1]
BUNDLE = "cc.loki.assistant.mobile"
ARTIFACTS = ROOT / ".build/network-probe"


def checked(args, timeout=45):
    result = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    if result.returncode or "[Fail]" in result.stdout:
        raise RuntimeError(f"命令失败：{args}\n{result.stdout}\n{result.stderr}")
    return result.stdout


def wait_until(check, description, seconds=45):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(0.5)
    raise RuntimeError(f"等待超时：{description}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", default="")
    parser.add_argument("--deveco", default="/Applications/DevEco-Studio.app/Contents")
    args = parser.parse_args()
    hdc = str(pathlib.Path(args.deveco) / "sdk/default/openharmony/toolchains/hdc")
    devices = checked([hdc, "list", "targets", "-v"])
    targets = [line.split()[0] for line in devices.splitlines()
               if "Connected" in line and "localhost" in line]
    target = args.target or (targets[0] if len(targets) == 1 else "")
    if target not in targets:
        raise RuntimeError("请用 HDC_TARGET 指定已连接的本地模拟器；不向真机安装验证程序。")
    device = [hdc, "-t", target]

    def shell(*words):
        return checked([*device, "shell", *words])

    with socket.socket() as sock:
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.bind(("127.0.0.1", 3340))
    ARTIFACTS.mkdir(parents=True, exist_ok=True)
    (ARTIFACTS / "result.json").unlink(missing_ok=True)
    binary = str(ROOT / "services/target/debug/loki-services")
    processes = []
    handles = []
    forward_created = False

    def start(command, name):
        log = ARTIFACTS / f"{name}.log"
        handle = log.open("w")
        handles.append(handle)
        process = subprocess.Popen([binary, command], stdout=handle, stderr=subprocess.STDOUT)
        processes.append(process)
        return process, log

    def stop(process):
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)

    def ready_relay():
        try:
            with urllib.request.urlopen("http://127.0.0.1:3340/", timeout=1) as response:
                return b"Iroh Relay" in response.read()
        except OSError:
            return False

    def ready_services(process, log):
        if process.poll() is not None:
            raise RuntimeError(log.read_text())
        text = log.read_text()
        if "SERVICES_READY" not in text:
            return False
        address = re.search(r"http://127\.0\.0\.1:\d+", text)
        with urllib.request.urlopen(address.group() + "/healthz", timeout=2) as response:
            return response.status == 200

    def launch():
        shell("aa", "force-stop", BUNDLE)
        if "start ability successfully" not in shell("aa", "start", "-a", "EntryAbility", "-b", BUNDLE):
            raise RuntimeError("无法启动手机验证页面")
        pid = wait_until(lambda: shell("pidof", BUNDLE).strip(), "手机进程启动", 10)

        def read_report():
            logs = shell("hilog", "-x", "-P", pid, "-T", "LokiNetworkProbe")
            (ARTIFACTS / "mobile.log").write_text(logs)
            for line in logs.splitlines():
                if '{"' in line:
                    try:
                        return json.loads(line[line.index('{"'):])
                    except json.JSONDecodeError:
                        pass
            return None

        return wait_until(read_report, "手机验证结果", 70)

    try:
        relay, _ = start("network-probe-relay", "relay")
        wait_until(ready_relay, "本地中继启动", 10)
        service, service_log = start("network-probe", "services")
        wait_until(lambda: ready_services(service, service_log), "services 注册身份")
        if "tcp:3340 tcp:3340" not in checked([*device, "fport", "ls"]):
            checked([*device, "rport", "tcp:3340", "tcp:3340"])
            forward_created = True
        hap = ROOT / "apps/harmony/entry/build/default/outputs/default/entry-default-signed.hap"
        install = checked([*device, "install", "-r", str(hap)])
        if "install bundle successfully" not in install:
            raise RuntimeError(install)
        first = launch()
        if first.get("ok") is not True or first.get("transport") != "relay-only":
            raise RuntimeError(f"双向消息验证失败：{first}")
        wait_until(lambda: "PROBE_PASS" in service_log.read_text(), "services 确认双向消息", 10)
        print("通过：模拟器 ↔ 本地中继 ↔ services 双向消息", flush=True)

        stop(service)
        service, service_log = start("network-probe", "services-restart")
        wait_until(lambda: ready_services(service, service_log), "services 重启")
        second = launch()
        if second != first:
            raise RuntimeError(f"重启后身份或双向消息结果变化：{second}")
        wait_until(lambda: "PROBE_PASS" in service_log.read_text(), "重启后的 services 确认", 10)
        print("通过：services 与手机重启后保持固定身份", flush=True)

        stop(relay)
        failure = launch()
        if failure.get("ok") is not False or "中继连接超时" not in failure.get("error", ""):
            raise RuntimeError("中继停止后仍报告成功，未能证明强制中继路径")
        print(f"通过：中继停止后明确失败：{failure.get('error')}", flush=True)

        relay, _ = start("network-probe-relay", "relay-restored")
        wait_until(ready_relay, "恢复本地中继", 10)
        stop(service)
        service, service_log = start("network-probe", "services-restored")
        wait_until(lambda: ready_services(service, service_log), "恢复 services")
        final = launch()
        if final != first:
            raise RuntimeError(f"恢复后双向消息失败：{final}")
        wait_until(lambda: "PROBE_PASS" in service_log.read_text(), "恢复后的 services 确认", 10)
        shell("uitest", "dumpLayout", "-p", "/data/local/tmp/loki-network-probe.json", "-b", BUNDLE)
        checked([*device, "file", "recv", "/data/local/tmp/loki-network-probe.json", str(ARTIFACTS / "layout.json")])
        layout = (ARTIFACTS / "layout.json").read_text()
        if "双向消息验证通过" not in layout:
            raise RuntimeError("手机界面未展示验证成功")
        shell("uitest", "screenCap", "-p", "/data/local/tmp/loki-network-probe.png")
        checked([*device, "file", "recv", "/data/local/tmp/loki-network-probe.png", str(ARTIFACTS / "screenshot.png")])
        result = {"target": target, "report": final, "restart_identity_stable": True,
                  "relay_unavailable": failure, "recovery_passed": True}
        (ARTIFACTS / "result.json").write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
        print(f"本地身份组网验证通过，日志、截图与结果：{ARTIFACTS}", flush=True)
    finally:
        for process in reversed(processes):
            stop(process)
        for handle in handles:
            handle.close()
        if forward_created:
            checked([*device, "fport", "rm", "tcp:3340", "tcp:3340"])


if __name__ == "__main__":
    main()
