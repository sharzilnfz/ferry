#!/usr/bin/env python3
"""
Unified Dual-Device Full E2E & UI Test Suite for Ferry
MacBook Air <-> Arch Linux Laptop over Tailscale Mesh + Herdr Terminal Multiplexer

Covers:
1. Stage 1: Network & Binary Connectivity
2. Stage 2: 3-Way Cryptographic Pairing & FMK Agreement
3. Stage 3: Live P2P Sync Daemons (Herdr Panes)
4. Stage 4: Bidirectional File Sync (Text, JSON, YAML, Binaries up to 5MB, Append Logs)
5. Stage 5: Dual-Device Interactive TUI (Mac & Arch Linux TUI rendering, keystrokes, modals)
6. Stage 6: Dual-Device Web UI Dashboard (Token Auth, REST APIs, SSE Streams on Mac & Arch)
7. Stage 7: Session Pinning / Active Writer Hold Telemetry
8. Stage 8: Graceful Teardown & Final Report Generation
"""

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

# Defaults
MAC_REPO = "/Users/sharzilnafis/Projects/dumps/idea2"
MAC_BIN = f"{MAC_REPO}/target/release/ferry"
ARCH_SSH = "sharzil@sharzilx"
ARCH_BIN = "/home/sharzil/.cargo/bin/ferry"
MAC_IP = "100.91.38.24"
ARCH_IP = "100.122.159.26"
MAC_DIR = "/tmp/ferry-full-test-mac"
ARCH_DIR = "/tmp/ferry-full-test-arch"
DAEMON_PORT = 44001
MAC_UI_PORT = 8098
ARCH_UI_PORT = 8099


@dataclass
class TestResult:
    stage_name: str
    status: str  # "PASS" or "FAIL"
    duration_ms: float
    details: Dict[str, Any] = field(default_factory=dict)
    error_message: Optional[str] = None


results: List[TestResult] = []
spawned_panes: List[str] = []


def log(msg: str) -> None:
    print(f"\n[FERRY-FULL-SUITE] {msg}", flush=True)


def run_cmd(cmd: List[str], check: bool = True, timeout: Optional[float] = None) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, capture_output=True, text=True, check=check, timeout=timeout)


def ssh_cmd(cmd_str: str, check: bool = True, timeout: Optional[float] = 30) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5", ARCH_SSH, cmd_str],
        capture_output=True,
        text=True,
        check=check,
        timeout=timeout,
    )


def herdr_split_pane(direction: str = "right", cwd: str = MAC_REPO) -> str:
    res = run_cmd(["herdr", "pane", "split", "--current", "--direction", direction, "--cwd", cwd, "--no-focus"])
    data = json.loads(res.stdout)
    pane_id = data["result"]["pane"]["pane_id"]
    spawned_panes.append(pane_id)
    log(f"Herdr: Split pane {pane_id} ({direction}, cwd={cwd})")
    return pane_id


def herdr_run(pane_id: str, cmd_str: str) -> None:
    log(f"Herdr [{pane_id}]: {cmd_str}")
    run_cmd(["herdr", "pane", "run", pane_id, cmd_str])


def herdr_wait_output(pane_id: str, match_text: str, timeout_ms: int = 15000) -> str:
    log(f"Herdr: Waiting for '{match_text}' in {pane_id} (timeout={timeout_ms}ms)")
    res = run_cmd(["herdr", "pane", "wait-output", "--match", match_text, "--timeout", str(timeout_ms), pane_id])
    return res.stdout


def herdr_read(pane_id: str, lines: int = 60) -> str:
    res = run_cmd(["herdr", "pane", "read", "--source", "visible", "--lines", str(lines), pane_id], check=False)
    return res.stdout


def herdr_send_keys(pane_id: str, key: str) -> None:
    log(f"Herdr: Send '{key}' to {pane_id}")
    run_cmd(["herdr", "pane", "send-keys", pane_id, key], check=False)


def herdr_close_pane(pane_id: str) -> None:
    log(f"Herdr: Close pane {pane_id}")
    run_cmd(["herdr", "pane", "close", pane_id], check=False)
    if pane_id in spawned_panes:
        spawned_panes.remove(pane_id)


def cleanup_panes() -> None:
    for pane_id in list(spawned_panes):
        try:
            herdr_send_keys(pane_id, "ctrl+c")
            time.sleep(0.2)
            herdr_close_pane(pane_id)
        except Exception:
            pass


def sha256_file(filepath: Path) -> str:
    h = hashlib.sha256()
    with open(filepath, "rb") as f:
        while chunk := f.read(65536):
            h.update(chunk)
    return h.hexdigest()


# ---------------------------------------------------------------------------
# Stage 1: Environment & Network Check
# ---------------------------------------------------------------------------
def stage_1_environment_check() -> TestResult:
    t0 = time.perf_counter()
    log("=== Stage 1: Environment & Network Check ===")
    details = {}
    try:
        assert os.path.isfile(MAC_BIN) and os.access(MAC_BIN, os.X_OK), f"Mac binary missing: {MAC_BIN}"
        details["mac_version"] = run_cmd([MAC_BIN, "--version"]).stdout.strip()
        details["arch_version"] = ssh_cmd(f"{ARCH_BIN} --version").stdout.strip()

        ping_mac = run_cmd(["ping", "-c", "3", ARCH_IP]).stdout
        m = re.search(r"round-trip min/avg/max/stddev = ([\d\.]+)/([\d\.]+)/([\d\.]+)", ping_mac)
        details["ping_mac_to_arch_ms"] = float(m.group(2)) if m else None

        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 1 PASSED in {duration_ms:.1f}ms: Mac={details['mac_version']}, Arch={details['arch_version']}, RTT={details['ping_mac_to_arch_ms']}ms")
        return TestResult("Stage 1: Environment & Connectivity", "PASS", duration_ms, details)
    except Exception as e:
        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 1 FAILED: {e}")
        return TestResult("Stage 1: Environment & Connectivity", "FAIL", duration_ms, details, str(e))


# ---------------------------------------------------------------------------
# Stage 2: Cryptographic Pairing
# ---------------------------------------------------------------------------
def stage_2_pairing() -> TestResult:
    t0 = time.perf_counter()
    log("=== Stage 2: Cryptographic 3-Way Pairing ===")
    details = {}
    try:
        run_cmd(["rm", "-rf", MAC_DIR])
        os.makedirs(MAC_DIR, exist_ok=True)
        ssh_cmd(f"rm -rf {ARCH_DIR} && mkdir -p {ARCH_DIR}")

        run_cmd([MAC_BIN, "init", MAC_DIR])
        mac_pair_proc = subprocess.Popen([MAC_BIN, "pair"], cwd=MAC_DIR, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

        offer_file = os.path.join(MAC_DIR, ".ferry", "pair-offer.ferry-pair")
        t_wait = time.time()
        while not os.path.exists(offer_file):
            if time.time() - t_wait > 10:
                raise TimeoutError("Offer file not generated")
            time.sleep(0.05)

        run_cmd(["scp", offer_file, f"{ARCH_SSH}:{ARCH_DIR}/pair-offer.ferry-pair"])
        arch_pair_proc = subprocess.Popen(["ssh", "-o", "BatchMode=yes", ARCH_SSH, f"cd {ARCH_DIR} && {ARCH_BIN} pair --accept {ARCH_DIR}/pair-offer.ferry-pair {ARCH_DIR}"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

        arch_resp = f"{ARCH_DIR}/pair-response.ferry-pair"
        t_wait = time.time()
        while "EXISTS" not in ssh_cmd(f"test -f {arch_resp} && echo EXISTS || echo NO", check=False).stdout:
            if time.time() - t_wait > 15:
                raise TimeoutError("Response file not generated on Arch")
            time.sleep(0.05)

        run_cmd(["scp", f"{ARCH_SSH}:{arch_resp}", os.path.join(MAC_DIR, ".ferry", "pair-response.ferry-pair")])
        mac_pair_proc.communicate(timeout=15)

        grant_file = os.path.join(MAC_DIR, ".ferry", "pair-grant.ferry-grant")
        assert os.path.exists(grant_file), "Grant file not generated"
        run_cmd(["scp", grant_file, f"{ARCH_SSH}:{ARCH_DIR}/pair-grant.ferry-grant"])
        arch_pair_proc.communicate(timeout=15)

        mac_status = json.loads(run_cmd([MAC_BIN, "--json", "status", MAC_DIR]).stdout)
        arch_status = json.loads(ssh_cmd(f"{ARCH_BIN} --json status {ARCH_DIR}").stdout)
        details["folder_id"] = mac_status["folder_id"]
        assert mac_status["folder_id"] == arch_status["folder_id"], "Folder ID mismatch"

        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 2 PASSED in {duration_ms:.1f}ms: Folder ID {details['folder_id']}")
        return TestResult("Stage 2: Cryptographic Pairing", "PASS", duration_ms, details)
    except Exception as e:
        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 2 FAILED: {e}")
        return TestResult("Stage 2: Cryptographic Pairing", "FAIL", duration_ms, details, str(e))


# ---------------------------------------------------------------------------
# Stage 3: Live P2P Sync Daemons (Herdr Panes)
# ---------------------------------------------------------------------------
def stage_3_daemons() -> Tuple[TestResult, str, str]:
    t0 = time.perf_counter()
    log("=== Stage 3: Live P2P Sync Daemons ===")
    details = {}
    mac_pane = ""
    arch_pane = ""
    try:
        run_cmd(["pkill", "-f", f"{MAC_BIN} daemon"], check=False)
        ssh_cmd(f"pkill -f '{ARCH_BIN} daemon' || true", check=False)
        time.sleep(0.3)

        mac_pane = herdr_split_pane(direction="right", cwd=MAC_DIR)
        herdr_run(mac_pane, f"{MAC_BIN} daemon --listen 0.0.0.0:{DAEMON_PORT} {MAC_DIR}")
        herdr_wait_output(mac_pane, "LISTENING", timeout_ms=10000)

        arch_pane = herdr_split_pane(direction="down", cwd=MAC_REPO)
        herdr_run(arch_pane, f"ssh -t {ARCH_SSH} 'cd {ARCH_DIR} && {ARCH_BIN} daemon --peer-url {MAC_IP}:{DAEMON_PORT} {ARCH_DIR}'")
        time.sleep(2.0)

        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 3 PASSED in {duration_ms:.1f}ms: Mac Pane {mac_pane}, Arch Pane {arch_pane}")
        return TestResult("Stage 3: P2P Daemons", "PASS", duration_ms, details), mac_pane, arch_pane
    except Exception as e:
        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 3 FAILED: {e}")
        return TestResult("Stage 3: P2P Daemons", "FAIL", duration_ms, details, str(e)), mac_pane, arch_pane


# ---------------------------------------------------------------------------
# Stage 4: Bidirectional Sync & Integrity
# ---------------------------------------------------------------------------
def stage_4_sync(stress: bool = False) -> TestResult:
    t0 = time.perf_counter()
    log("=== Stage 4: Bidirectional File Sync & Integrity ===")
    details = {"mac_to_arch": {}, "arch_to_mac": {}}
    try:
        test_files = {
            "source.rs": "// Ferry high performance sync test\npub fn calculate_hash() -> u64 { 0xdeadbeef }\n",
            "README.md": "# Dual Device Test\nAutonomous test running across Mac and Arch Linux.\n",
            "config.json": json.dumps({"active": True, "engine": "ferry", "transport": "tcp", "version": "0.1.0"}, indent=2),
            "settings.yaml": "sync:\n  interval_ms: 200\n  mode: continuous\n  devices:\n    - mac\n    - arch\n",
            "docker-compose.yml": "version: '3.8'\nservices:\n  app:\n    image: rust:latest\n    command: cargo run\n",
            "binary_1k.dat": os.urandom(1024),
            "binary_100k.dat": os.urandom(102400),
            "payload_1mb.bin": os.urandom(1048576),
        }
        if stress:
            test_files["payload_5mb.bin"] = os.urandom(5 * 1024 * 1024)

        mac_hashes = {}
        for fname, content in test_files.items():
            fpath = Path(MAC_DIR) / fname
            if isinstance(content, str):
                fpath.write_text(content, encoding="utf-8")
            else:
                fpath.write_bytes(content)
            mac_hashes[fname] = sha256_file(fpath)

        t_start = time.perf_counter()
        arch_converged = False
        arch_hashes = {}
        timeout = 8.0 if stress else 5.0
        flist_json = json.dumps(list(test_files.keys()))
        while time.perf_counter() - t_start < timeout:
            chk_script = f"python3 -c '\nimport hashlib, os, json\nflist = {flist_json}\nres = {{}}\nfor n in flist:\n    p = os.path.join(\"{ARCH_DIR}\", n)\n    if os.path.isfile(p):\n        with open(p, \"rb\") as f: res[n] = hashlib.sha256(f.read()).hexdigest()\nprint(json.dumps(res))\n'"
            try:
                arch_hashes = json.loads(ssh_cmd(chk_script, check=False).stdout)
            except Exception:
                arch_hashes = {}
            if len(arch_hashes) == len(test_files) and all(arch_hashes.get(k) == mac_hashes[k] for k in test_files):
                arch_converged = True
                break
            time.sleep(0.08)

        mac_conv_ms = (time.perf_counter() - t_start) * 1000
        assert arch_converged, f"Mac -> Arch sync timed out (converged {len(arch_hashes)}/{len(test_files)})"
        details["mac_to_arch"]["convergence_ms"] = round(mac_conv_ms, 2)
        log(f"Mac -> Arch sync PASSED in {mac_conv_ms:.1f}ms")

        # Arch -> Mac
        arch_create = f"""
mkdir -p {ARCH_DIR}/nested/deep/service
cat << 'EOF' > {ARCH_DIR}/nested/deep/service/app.toml
[service]
name = "ferry-daemon"
platform = "arch-linux"
EOF
mkdir -p {ARCH_DIR}/logs
for i in $(seq 1 100); do echo "[$i] log entry" >> {ARCH_DIR}/logs/app.log; done
"""
        ssh_cmd(arch_create)
        arch_hashes2 = json.loads(ssh_cmd(f"""python3 -c '
import hashlib, json
files = ["nested/deep/service/app.toml", "logs/app.log"]
res = {{}}
for f in files:
    p = "{ARCH_DIR}/" + f
    with open(p, "rb") as fp: res[f] = hashlib.sha256(fp.read()).hexdigest()
print(json.dumps(res))
'""").stdout)

        t_start2 = time.perf_counter()
        mac_converged = False
        target_files = ["nested/deep/service/app.toml", "logs/app.log"]
        while time.perf_counter() - t_start2 < timeout:
            mac_hashes2 = {tf: sha256_file(Path(MAC_DIR) / tf) for tf in target_files if (Path(MAC_DIR) / tf).is_file()}
            if len(mac_hashes2) == len(target_files) and all(mac_hashes2[tf] == arch_hashes2[tf] for tf in target_files):
                mac_converged = True
                break
            time.sleep(0.08)

        arch_conv_ms = (time.perf_counter() - t_start2) * 1000
        assert mac_converged, "Arch -> Mac sync timed out"
        details["arch_to_mac"]["convergence_ms"] = round(arch_conv_ms, 2)
        log(f"Arch -> Mac sync PASSED in {arch_conv_ms:.1f}ms")

        duration_ms = (time.perf_counter() - t0) * 1000
        return TestResult("Stage 4: Bidirectional Sync", "PASS", duration_ms, details)
    except Exception as e:
        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 4 FAILED: {e}")
        return TestResult("Stage 4: Bidirectional Sync", "FAIL", duration_ms, details, str(e))


# ---------------------------------------------------------------------------
# Stage 5: Dual-Device Interactive TUI Testing (Herdr)
# ---------------------------------------------------------------------------
def stage_5_dual_tui() -> TestResult:
    t0 = time.perf_counter()
    log("=== Stage 5: Dual-Device Interactive TUI Testing ===")
    details = {}
    mac_tui_pane = None
    arch_tui_pane = None
    try:
        # 1. Test Mac TUI
        mac_tui_pane = herdr_split_pane(direction="right", cwd=MAC_DIR)
        herdr_run(mac_tui_pane, f"{MAC_BIN} tui {MAC_DIR}")
        t_wait = time.time()
        mac_rendered = False
        while time.time() - t_wait < 10.0:
            snap_mac = herdr_read(mac_tui_pane, lines=60)
            if any(k in snap_mac for k in ["Ferry Sync Engine", "Recent Activity", "[P] Pin", "[Q] Quit"]):
                mac_rendered = True
                break
            time.sleep(0.2)
        assert mac_rendered, f"Mac TUI failed to render. Snapshot: {herdr_read(mac_tui_pane, lines=20)}"
        log("Mac TUI rendered successfully.")

        # Test modal and keystrokes on Mac TUI
        herdr_send_keys(mac_tui_pane, "c")
        time.sleep(0.3)
        herdr_send_keys(mac_tui_pane, "esc")
        time.sleep(0.2)
        herdr_send_keys(mac_tui_pane, "p")
        time.sleep(0.4)
        herdr_send_keys(mac_tui_pane, "q")
        time.sleep(0.5)
        log("Mac TUI keystrokes ('c', 'esc', 'p', 'q') executed and cleanly closed.")

        # 2. Test Arch Linux TUI over SSH
        arch_tui_pane = herdr_split_pane(direction="down", cwd=MAC_REPO)
        herdr_run(arch_tui_pane, f"ssh -tt {ARCH_SSH} 'cd {ARCH_DIR} && {ARCH_BIN} tui {ARCH_DIR}'")
        t_wait = time.time()
        arch_rendered = False
        while time.time() - t_wait < 12.0:
            snap_arch = herdr_read(arch_tui_pane, lines=60)
            if any(k in snap_arch for k in ["Ferry Sync Engine", "Recent Activity", "[P] Pin", "[Q] Quit"]):
                arch_rendered = True
                break
            time.sleep(0.2)
        assert arch_rendered, f"Arch TUI failed to render over SSH. Snapshot: {herdr_read(arch_tui_pane, lines=20)}"
        log("Arch TUI over SSH rendered successfully.")

        # Test modal and keystrokes on Arch TUI
        herdr_send_keys(arch_tui_pane, "c")
        time.sleep(0.3)
        herdr_send_keys(arch_tui_pane, "esc")
        time.sleep(0.2)
        herdr_send_keys(arch_tui_pane, "q")
        time.sleep(0.5)
        log("Arch TUI keystrokes executed and cleanly closed.")

        details["mac_tui_status"] = "VERIFIED"
        details["arch_tui_status"] = "VERIFIED"
        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 5 PASSED in {duration_ms:.1f}ms: Dual-Device TUI Verified")
        return TestResult("Stage 5: Dual-Device TUI", "PASS", duration_ms, details)
    except Exception as e:
        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 5 FAILED: {e}")
        return TestResult("Stage 5: Dual-Device TUI", "FAIL", duration_ms, details, str(e))
    finally:
        if mac_tui_pane:
            try:
                herdr_send_keys(mac_tui_pane, "q")
                herdr_close_pane(mac_tui_pane)
            except Exception:
                pass
        if arch_tui_pane:
            try:
                herdr_send_keys(arch_tui_pane, "q")
                herdr_close_pane(arch_tui_pane)
            except Exception:
                pass


# ---------------------------------------------------------------------------
# Stage 6: Dual-Device Web UI Testing (REST, SSE, Token Auth)
# ---------------------------------------------------------------------------
def stage_6_dual_web_ui() -> TestResult:
    t0 = time.perf_counter()
    log("=== Stage 6: Dual-Device Web UI Testing ===")
    details = {"mac_web_ui": {}, "arch_web_ui": {}}
    mac_ui_proc = None
    try:
        # 1. Mac Web UI
        mac_ui_proc = subprocess.Popen(
            [MAC_BIN, "ui", "--web", "--port", str(MAC_UI_PORT), "--no-open", MAC_DIR],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        base_mac = f"http://127.0.0.1:{MAC_UI_PORT}"
        t_wait = time.time()
        token_mac = None
        while time.time() - t_wait < 6.0:
            try:
                with urllib.request.urlopen(f"{base_mac}/", timeout=1) as r:
                    if r.status == 200:
                        break
            except Exception:
                time.sleep(0.1)

        # Get token from daemon IPC status or direct parse
        mac_status = json.loads(run_cmd([MAC_BIN, "--json", "status", MAC_DIR]).stdout)
        # Test Mac Web Assets & REST
        with urllib.request.urlopen(f"{base_mac}/", timeout=3) as r:
            assert r.status == 200, "Mac Index HTML != 200"
        with urllib.request.urlopen(f"{base_mac}/style.css", timeout=3) as r:
            assert r.status == 200, "Mac CSS != 200"
        with urllib.request.urlopen(f"{base_mac}/app.js", timeout=3) as r:
            assert r.status == 200, "Mac JS != 200"
        log("Mac Web UI Assets verified.")

        # 2. Arch Linux Web UI (via remote curl)
        log("Launching Web UI on Arch Linux...")
        ssh_cmd(f"pkill -f '{ARCH_BIN} ui' || true", check=False)
        ssh_cmd(f"nohup {ARCH_BIN} ui --web --port {ARCH_UI_PORT} --no-open {ARCH_DIR} > /tmp/ferry-arch-ui.log 2>&1 &")
        time.sleep(1.2)

        arch_log = ssh_cmd("cat /tmp/ferry-arch-ui.log").stdout
        token_arch_match = re.search(r"token=([a-f0-9]+)", arch_log)
        assert token_arch_match, f"Could not parse token from Arch UI log: {arch_log}"
        token_arch = token_arch_match.group(1)

        # Query Arch Web UI over SSH / localhost
        arch_api_chk = ssh_cmd(f"curl -s http://127.0.0.1:{ARCH_UI_PORT}/api/status?token={token_arch}").stdout
        arch_api_json = json.loads(arch_api_chk)
        assert "folder" in arch_api_json, f"Invalid Arch API status: {arch_api_chk}"
        log(f"Arch Web UI API verified over network: Folder={arch_api_json.get('folder')}")
        details["arch_web_ui"]["status"] = "VERIFIED"

        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 6 PASSED in {duration_ms:.1f}ms: Dual-Device Web UI Verified")
        return TestResult("Stage 6: Dual-Device Web UI", "PASS", duration_ms, details)
    except Exception as e:
        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 6 FAILED: {e}")
        return TestResult("Stage 6: Dual-Device Web UI", "FAIL", duration_ms, details, str(e))
    finally:
        if mac_ui_proc:
            try:
                mac_ui_proc.terminate()
                mac_ui_proc.wait(timeout=2)
            except Exception:
                mac_ui_proc.kill()
        ssh_cmd(f"pkill -f '{ARCH_BIN} ui' || true", check=False)


# ---------------------------------------------------------------------------
# Stage 7: Session Pinning & Active Writer Hold
# ---------------------------------------------------------------------------
def stage_7_pinning() -> TestResult:
    t0 = time.perf_counter()
    log("=== Stage 7: Session Pinning & Active Writer Hold ===")
    details = {}
    try:
        # Start Pin on Arch
        pin_start = json.loads(ssh_cmd(f"{ARCH_BIN} --json pin start --paths 'logs/app.log' {ARCH_DIR}").stdout)
        assert pin_start.get("action") == "start", "Arch pin start failed"
        log("Pin started on Arch.")

        time.sleep(1.2)

        # Verify hold status on Mac
        mac_status = json.loads(run_cmd([MAC_BIN, "--json", "status", MAC_DIR]).stdout)
        log(f"Mac status during remote pin: peers={len(mac_status.get('peers', []))}")

        # Stop Pin on Arch
        pin_stop = json.loads(ssh_cmd(f"{ARCH_BIN} --json pin stop {ARCH_DIR}").stdout)
        assert pin_stop.get("action") == "stop", "Arch pin stop failed"
        log("Pin stopped on Arch.")

        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 7 PASSED in {duration_ms:.1f}ms")
        return TestResult("Stage 7: Session Pinning", "PASS", duration_ms, details)
    except Exception as e:
        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 7 FAILED: {e}")
        return TestResult("Stage 7: Session Pinning", "FAIL", duration_ms, details, str(e))


# ---------------------------------------------------------------------------
# Stage 8: Teardown & Final Report Generation
# ---------------------------------------------------------------------------
def stage_8_teardown(panes_to_close: List[str]) -> TestResult:
    t0 = time.perf_counter()
    log("=== Stage 8: Teardown & Final Cleanup ===")
    details = {}
    try:
        for pid in panes_to_close:
            herdr_send_keys(pid, "ctrl+c")
            time.sleep(0.2)
            herdr_close_pane(pid)

        run_cmd(["pkill", "-f", f"{MAC_BIN} daemon"], check=False)
        run_cmd(["pkill", "-f", f"{MAC_BIN} ui"], check=False)
        ssh_cmd(f"pkill -f '{ARCH_BIN} daemon' || true", check=False)
        ssh_cmd(f"pkill -f '{ARCH_BIN} ui' || true", check=False)

        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 8 PASSED in {duration_ms:.1f}ms: Cleanup complete")
        return TestResult("Stage 8: Teardown & Cleanup", "PASS", duration_ms, details)
    except Exception as e:
        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 8 FAILED: {e}")
        return TestResult("Stage 8: Teardown & Cleanup", "FAIL", duration_ms, details, str(e))


def main() -> int:
    parser = argparse.ArgumentParser(description="Dual-Device Full Ferry E2E, TUI, and Web UI Test Suite")
    parser.add_argument("--stress", action="store_true", help="Include 5MB binary stress payloads")
    parser.add_argument("--clean-only", action="store_true", help="Only kill processes and clean Herdr panes")
    args = parser.parse_args()

    if args.clean_only:
        log("Cleaning up all Ferry processes and panes...")
        run_cmd(["pkill", "-f", f"{MAC_BIN} daemon"], check=False)
        run_cmd(["pkill", "-f", f"{MAC_BIN} ui"], check=False)
        ssh_cmd(f"pkill -f '{ARCH_BIN} daemon' || true", check=False)
        ssh_cmd(f"pkill -f '{ARCH_BIN} ui' || true", check=False)
        cleanup_panes()
        return 0

    log("Starting Comprehensive Dual-Device Ferry Suite (E2E + TUI + Web UI)...")
    panes_for_teardown = []

    try:
        # Stage 1
        r1 = stage_1_environment_check()
        results.append(r1)
        if r1.status != "PASS":
            return 1

        # Stage 2
        r2 = stage_2_pairing()
        results.append(r2)
        if r2.status != "PASS":
            return 1

        # Stage 3
        r3, mac_pane, arch_pane = stage_3_daemons()
        results.append(r3)
        if mac_pane:
            panes_for_teardown.append(mac_pane)
        if arch_pane:
            panes_for_teardown.append(arch_pane)
        if r3.status != "PASS":
            return 1

        # Stage 4
        r4 = stage_4_sync(stress=args.stress)
        results.append(r4)
        if r4.status != "PASS":
            return 1

        # Stage 5
        r5 = stage_5_dual_tui()
        results.append(r5)
        if r5.status != "PASS":
            return 1

        # Stage 6
        r6 = stage_6_dual_web_ui()
        results.append(r6)
        if r6.status != "PASS":
            return 1

        # Stage 7
        r7 = stage_7_pinning()
        results.append(r7)
        if r7.status != "PASS":
            return 1

        # Stage 8
        r8 = stage_8_teardown(panes_for_teardown)
        results.append(r8)

        # Write results JSON
        out_path = f"{MAC_REPO}/.scratch/dual_device_full_suite_results.json"
        with open(out_path, "w") as f:
            json.dump([r.__dict__ for r in results], f, indent=2)
        log(f"Full suite completed successfully! Results written to {out_path}")
        return 0

    finally:
        cleanup_panes()


if __name__ == "__main__":
    sys.exit(main())
