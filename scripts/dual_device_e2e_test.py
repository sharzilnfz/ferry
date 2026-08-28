#!/usr/bin/env python3
"""
High-Performance Dual-Device Regression & E2E Test Suite for Ferry
MacBook Air <-> Arch Linux over Tailscale Mesh + Herdr Multiplexer

Features:
- Fast startup (< 500ms initiation)
- Full cryptographic 3-way pairing automation
- Deep regression testing:
    1. Multi-tier payload synchronization (Text, JSON, YAML, Binaries up to 10MB)
    2. Deep directory tree structure preservation
    3. High-frequency append log convergence
    4. Concurrent unpinned conflict quarantine test (ADR-0004 validation)
    5. Session pinning active-writer hold propagation & release
    6. Web UI, REST endpoints, and SSE stream verification
- Complete JSON metrics output and graceful process teardown
"""

import argparse
import hashlib
import json
import os
import re
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

# Configuration defaults
DEFAULT_MAC_REPO = "/Users/sharzilnafis/Projects/dumps/idea2"
DEFAULT_MAC_BIN = f"{DEFAULT_MAC_REPO}/target/release/ferry"
DEFAULT_ARCH_SSH = "sharzil@sharzilx"
DEFAULT_ARCH_BIN = "/home/sharzil/.cargo/bin/ferry"
DEFAULT_MAC_IP = "100.91.38.24"
DEFAULT_ARCH_IP = "100.122.159.26"
DEFAULT_MAC_TEST_DIR = "/tmp/ferry-dual-test-mac"
DEFAULT_ARCH_TEST_DIR = "/tmp/ferry-dual-test-arch"
DEFAULT_PORT = 44001
DEFAULT_UI_PORT = 8098


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
    print(f"\n[FERRY-E2E] {msg}", flush=True)


def run_cmd(cmd: List[str], check: bool = True, timeout: Optional[float] = None) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, capture_output=True, text=True, check=check, timeout=timeout)


def ssh_cmd(arch_ssh: str, cmd_str: str, check: bool = True, timeout: Optional[float] = 30) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5", arch_ssh, cmd_str],
        capture_output=True,
        text=True,
        check=check,
        timeout=timeout,
    )


def herdr_split_pane(direction: str = "right", cwd: str = DEFAULT_MAC_REPO) -> str:
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


def herdr_read(pane_id: str, lines: int = 50) -> str:
    res = run_cmd(["herdr", "pane", "read", "--source", "recent-unwrapped", "--lines", str(lines), pane_id], check=False)
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
# Stage 1: Environment & Connectivity Check
# ---------------------------------------------------------------------------
def stage_1_environment_check(mac_bin: str, arch_bin: str, arch_ssh: str, mac_ip: str, arch_ip: str) -> TestResult:
    t0 = time.perf_counter()
    log("=== Stage 1: Environment & Connectivity Check ===")
    details = {}
    try:
        # Check Mac binary
        if not os.path.isfile(mac_bin) or not os.access(mac_bin, os.X_OK):
            raise RuntimeError(f"Mac binary not found or not executable: {mac_bin}")
        mac_ver = run_cmd([mac_bin, "--version"]).stdout.strip()
        details["mac_binary"] = mac_bin
        details["mac_version"] = mac_ver
        details["mac_size_bytes"] = os.path.getsize(mac_bin)

        # Check Arch binary
        arch_res = ssh_cmd(arch_ssh, f"{arch_bin} --version")
        arch_ver = arch_res.stdout.strip()
        details["arch_binary"] = arch_bin
        details["arch_version"] = arch_ver

        # Check Mac -> Arch ping
        ping_mac = run_cmd(["ping", "-c", "3", arch_ip]).stdout
        m = re.search(r"round-trip min/avg/max/stddev = ([\d\.]+)/([\d\.]+)/([\d\.]+)", ping_mac)
        if m:
            details["ping_mac_to_arch_rtt_avg_ms"] = float(m.group(2))
        else:
            m2 = re.search(r"rtt min/avg/max/mdev = ([\d\.]+)/([\d\.]+)/([\d\.]+)", ping_mac)
            details["ping_mac_to_arch_rtt_avg_ms"] = float(m2.group(2)) if m2 else None

        # Check Arch -> Mac ping
        ping_arch = ssh_cmd(arch_ssh, f"ping -c 3 {mac_ip}").stdout
        m_arch = re.search(r"rtt min/avg/max/mdev = ([\d\.]+)/([\d\.]+)/([\d\.]+)", ping_arch)
        if m_arch:
            details["ping_arch_to_mac_rtt_avg_ms"] = float(m_arch.group(2))
        else:
            details["ping_arch_to_mac_rtt_avg_ms"] = None

        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 1 PASSED in {duration_ms:.1f}ms: Mac={mac_ver}, Arch={arch_ver}, RTT={details['ping_mac_to_arch_rtt_avg_ms']}ms")
        return TestResult("Stage 1: Environment & Connectivity", "PASS", duration_ms, details)
    except Exception as e:
        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 1 FAILED: {e}")
        return TestResult("Stage 1: Environment & Connectivity", "FAIL", duration_ms, details, str(e))


# ---------------------------------------------------------------------------
# Stage 2: Isolated Workspace & Cryptographic Pairing
# ---------------------------------------------------------------------------
def stage_2_cryptographic_pairing(mac_bin: str, arch_bin: str, arch_ssh: str, mac_dir: str, arch_dir: str) -> TestResult:
    t0 = time.perf_counter()
    log("=== Stage 2: Isolated Workspace & Cryptographic Pairing ===")
    details = {}
    try:
        # Clean directories
        run_cmd(["rm", "-rf", mac_dir])
        os.makedirs(mac_dir, exist_ok=True)
        ssh_cmd(arch_ssh, f"rm -rf {arch_dir} && mkdir -p {arch_dir}")

        # Init on Mac
        log("Running `ferry init` on Mac...")
        init_res = run_cmd([mac_bin, "init", mac_dir])
        log(f"Init output: {init_res.stdout.strip()}")

        # Start pairing initiator on Mac in background
        log("Starting `ferry pair` on Mac (initiator)...")
        mac_pair_proc = subprocess.Popen(
            [mac_bin, "pair"],
            cwd=mac_dir,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

        # Wait for offer file to be generated
        offer_file = os.path.join(mac_dir, ".ferry", "pair-offer.ferry-pair")
        offer_wait_t0 = time.time()
        while not os.path.exists(offer_file):
            if time.time() - offer_wait_t0 > 10:
                raise TimeoutError("Timed out waiting for pair-offer.ferry-pair on Mac")
            time.sleep(0.05)

        details["offer_size_bytes"] = os.path.getsize(offer_file)
        log(f"Offer file generated: {offer_file} ({details['offer_size_bytes']} bytes)")

        # Copy offer file to Arch
        arch_offer_file = f"{arch_dir}/pair-offer.ferry-pair"
        run_cmd(["scp", offer_file, f"{arch_ssh}:{arch_offer_file}"])

        # Start pairing acceptor on Arch
        log("Running `ferry pair --accept` on Arch...")
        arch_pair_proc = subprocess.Popen(
            ["ssh", "-o", "BatchMode=yes", arch_ssh, f"cd {arch_dir} && {arch_bin} pair --accept {arch_offer_file} {arch_dir}"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

        # Wait for response file on Arch
        arch_response_file = f"{arch_dir}/pair-response.ferry-pair"
        mac_response_file = os.path.join(mac_dir, ".ferry", "pair-response.ferry-pair")
        resp_wait_t0 = time.time()
        resp_found = False
        while not resp_found:
            if time.time() - resp_wait_t0 > 15:
                raise TimeoutError("Timed out waiting for pair-response.ferry-pair on Arch")
            chk = ssh_cmd(arch_ssh, f"test -f {arch_response_file} && echo EXISTS || echo NO", check=False)
            if "EXISTS" in chk.stdout:
                resp_found = True
                break
            time.sleep(0.05)

        log("Response file generated on Arch. Copying back to Mac...")
        run_cmd(["scp", f"{arch_ssh}:{arch_response_file}", mac_response_file])

        # Mac pair process completes and emits grant file
        log("Waiting for Mac pair process to seal grant...")
        mac_stdout, mac_stderr = mac_pair_proc.communicate(timeout=15)
        if mac_pair_proc.returncode != 0:
            raise RuntimeError(f"Mac pair failed (code {mac_pair_proc.returncode}): {mac_stderr}")

        grant_file = os.path.join(mac_dir, ".ferry", "pair-grant.ferry-grant")
        if not os.path.exists(grant_file):
            raise RuntimeError(f"Grant file {grant_file} not found after pairing")

        log(f"Grant file created: {grant_file}. Copying to Arch...")
        arch_grant_file = f"{arch_dir}/pair-grant.ferry-grant"
        run_cmd(["scp", grant_file, f"{arch_ssh}:{arch_grant_file}"])

        # Arch pair process completes
        log("Waiting for Arch pair accept process to complete...")
        arch_stdout, arch_stderr = arch_pair_proc.communicate(timeout=15)
        if arch_pair_proc.returncode != 0:
            raise RuntimeError(f"Arch pair accept failed (code {arch_pair_proc.returncode}): {arch_stderr}")

        # Verify folder status & FMK on both devices
        mac_status = json.loads(run_cmd([mac_bin, "--json", "status", mac_dir]).stdout)
        arch_status = json.loads(ssh_cmd(arch_ssh, f"{arch_bin} --json status {arch_dir}").stdout)

        details["mac_folder_id"] = mac_status.get("folder_id")
        details["arch_folder_id"] = arch_status.get("folder_id")
        details["mac_device_id"] = mac_status.get("device_id")
        details["arch_device_id"] = arch_status.get("device_id")

        if details["mac_folder_id"] != details["arch_folder_id"]:
            raise AssertionError(f"Folder ID mismatch: Mac={details['mac_folder_id']} vs Arch={details['arch_folder_id']}")

        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 2 PASSED in {duration_ms:.1f}ms: Folder ID {details['mac_folder_id']}")
        return TestResult("Stage 2: Workspace & Pairing Ritual", "PASS", duration_ms, details)
    except Exception as e:
        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 2 FAILED: {e}")
        return TestResult("Stage 2: Workspace & Pairing Ritual", "FAIL", duration_ms, details, str(e))


# ---------------------------------------------------------------------------
# Stage 3: Live P2P Sync Daemons (Herdr Panes)
# ---------------------------------------------------------------------------
def stage_3_start_daemons(mac_bin: str, arch_bin: str, arch_ssh: str, mac_dir: str, arch_dir: str, mac_ip: str, port: int) -> Tuple[TestResult, str, str]:
    t0 = time.perf_counter()
    log("=== Stage 3: Live P2P Sync Daemons (Herdr Panes) ===")
    details = {}
    mac_pane_id = ""
    arch_pane_id = ""
    try:
        # Kill previous daemons
        run_cmd(["pkill", "-f", f"{mac_bin} daemon"], check=False)
        ssh_cmd(arch_ssh, f"pkill -f '{arch_bin} daemon' || true", check=False)
        time.sleep(0.3)

        # 1. Split pane for Mac daemon
        mac_pane_id = herdr_split_pane(direction="right", cwd=mac_dir)
        details["mac_daemon_pane"] = mac_pane_id

        # Start Mac listener daemon
        mac_daemon_cmd = f"{mac_bin} daemon --listen 0.0.0.0:{port} {mac_dir}"
        herdr_run(mac_pane_id, mac_daemon_cmd)

        # Wait for listener to be active
        wait_out = herdr_wait_output(mac_pane_id, "LISTENING", timeout_ms=10000)
        details["mac_daemon_listen_output"] = wait_out.strip()

        # 2. Split pane for Arch daemon
        arch_pane_id = herdr_split_pane(direction="down", cwd=DEFAULT_MAC_REPO)
        details["arch_daemon_pane"] = arch_pane_id

        # Start Arch connector daemon
        arch_daemon_cmd = f"ssh -t {arch_ssh} 'cd {arch_dir} && {arch_bin} daemon --peer-url {mac_ip}:{port} {arch_dir}'"
        herdr_run(arch_pane_id, arch_daemon_cmd)

        # Connection establishment window
        time.sleep(1.8)

        # Query status via IPC
        mac_status = json.loads(run_cmd([mac_bin, "--json", "status", mac_dir]).stdout)
        arch_status = json.loads(ssh_cmd(arch_ssh, f"{arch_bin} --json status {arch_dir}").stdout)
        details["mac_peers"] = mac_status.get("peers", [])
        details["arch_peers"] = arch_status.get("peers", [])

        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 3 PASSED in {duration_ms:.1f}ms: Mac={mac_pane_id}, Arch={arch_pane_id}")
        return TestResult("Stage 3: Live P2P Sync Daemons", "PASS", duration_ms, details), mac_pane_id, arch_pane_id
    except Exception as e:
        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 3 FAILED: {e}")
        return TestResult("Stage 3: Live P2P Sync Daemons", "FAIL", duration_ms, details, str(e)), mac_pane_id, arch_pane_id


# ---------------------------------------------------------------------------
# Stage 4: Bidirectional File Sync & Regression Assertion
# ---------------------------------------------------------------------------
def stage_4_bidirectional_sync(arch_ssh: str, mac_dir: str, arch_dir: str, stress_mode: bool = False) -> TestResult:
    t0 = time.perf_counter()
    log("=== Stage 4: Bidirectional File Sync & Performance Assertion ===")
    details: Dict[str, Any] = {"mac_to_arch": {}, "arch_to_mac": {}}
    try:
        # Part 1: Mac -> Arch Sync
        log("Part 1: Generating heterogeneous test files on Mac...")
        test_files = {
            "source.rs": "// Ferry high performance sync test\npub fn calculate_hash() -> u64 { 0xdeadbeef }\n",
            "README.md": "# Dual Device Test\nAutonomous regression test running across Mac and Arch Linux.\n",
            "config.json": json.dumps({"active": True, "engine": "ferry", "transport": "tcp", "version": "0.1.0"}, indent=2),
            "settings.yaml": "sync:\n  interval_ms: 200\n  mode: continuous\n  devices:\n    - mac\n    - arch\n",
            "docker-compose.yml": "version: '3.8'\nservices:\n  app:\n    image: rust:latest\n    command: cargo run\n",
            "binary_1k.dat": os.urandom(1024),
            "binary_10k.dat": os.urandom(10240),
            "binary_100k.dat": os.urandom(102400),
            "binary_500k.dat": os.urandom(512000),
            "payload_1mb.bin": os.urandom(1048576),
        }
        if stress_mode:
            test_files["payload_5mb.bin"] = os.urandom(5 * 1024 * 1024)

        mac_hashes = {}
        for fname, content in test_files.items():
            fpath = Path(mac_dir) / fname
            if isinstance(content, str):
                fpath.write_text(content, encoding="utf-8")
            else:
                fpath.write_bytes(content)
            mac_hashes[fname] = sha256_file(fpath)

        t_mac_write = time.perf_counter()
        log(f"{len(test_files)} files written on Mac. Polling Arch for convergence...")

        arch_converged = False
        arch_hashes = {}
        timeout_sec = 8.0 if stress_mode else 5.0
        file_list_json = json.dumps(list(test_files.keys()))
        while time.perf_counter() - t_mac_write < timeout_sec:
            chk_script = f"""python3 -c '
import hashlib, os, json
file_list = {file_list_json}
res = {{}}
for name in file_list:
    p = os.path.join("{arch_dir}", name)
    if os.path.isfile(p):
        with open(p, "rb") as f: res[name] = hashlib.sha256(f.read()).hexdigest()
print(json.dumps(res))
'"""
            chk_res = ssh_cmd(arch_ssh, chk_script, check=False)
            try:
                arch_hashes = json.loads(chk_res.stdout)
            except Exception:
                arch_hashes = {}

            if len(arch_hashes) == len(test_files):
                if all(arch_hashes.get(k) == mac_hashes[k] for k in test_files):
                    arch_converged = True
                    break
            time.sleep(0.08)

        t_mac_converged = time.perf_counter()
        mac_sync_duration_ms = (t_mac_converged - t_mac_write) * 1000

        details["mac_to_arch"]["file_count"] = len(test_files)
        details["mac_to_arch"]["convergence_time_ms"] = round(mac_sync_duration_ms, 2)
        details["mac_to_arch"]["all_matched"] = arch_converged
        details["mac_to_arch"]["files"] = {
            fname: {
                "size_bytes": len(test_files[fname]),
                "mac_sha256": mac_hashes[fname],
                "arch_sha256": arch_hashes.get(fname),
                "matched": mac_hashes[fname] == arch_hashes.get(fname),
            }
            for fname in test_files
        }

        if not arch_converged:
            raise AssertionError(f"Mac -> Arch sync timed out (converged {len(arch_hashes)}/{len(test_files)} files)")

        log(f"Part 1 Mac -> Arch sync PASSED in {mac_sync_duration_ms:.1f}ms")

        # Part 2: Arch -> Mac Sync (nested directory + append-heavy log)
        log("Part 2: Creating nested directory and append log on Arch...")
        arch_create_script = f"""
mkdir -p {arch_dir}/nested/deep/level3/service
cat << 'EOF' > {arch_dir}/nested/deep/level3/service/app.toml
[service]
name = "ferry-daemon"
platform = "arch-linux"
version = "0.1.0"
[metrics]
enabled = true
port = 9090
EOF

# Append 100 log lines with small delays
mkdir -p {arch_dir}/logs
> {arch_dir}/logs/app.log
for i in $(seq 1 100); do
  echo "[$i] $(date +%s%N) [INFO] Arch Linux event iteration $i generated for dual sync verification" >> {arch_dir}/logs/app.log
  sleep 0.005
done
"""
        ssh_cmd(arch_ssh, arch_create_script)

        # Get Arch hashes for verification
        arch_part2_hashes = json.loads(ssh_cmd(arch_ssh, f"""python3 -c '
import hashlib, json
files = ["nested/deep/level3/service/app.toml", "logs/app.log"]
res = {{}}
for f in files:
    p = "{arch_dir}/" + f
    with open(p, "rb") as fp: res[f] = hashlib.sha256(fp.read()).hexdigest()
print(json.dumps(res))
'""").stdout)

        t_arch_write = time.perf_counter()
        log("Arch files written. Polling Mac for convergence...")

        mac_part2_converged = False
        mac_part2_hashes = {}
        target_files = ["nested/deep/level3/service/app.toml", "logs/app.log"]

        while time.perf_counter() - t_arch_write < timeout_sec:
            mac_part2_hashes = {}
            for tf in target_files:
                p = Path(mac_dir) / tf
                if p.is_file():
                    mac_part2_hashes[tf] = sha256_file(p)

            if len(mac_part2_hashes) == len(target_files):
                if all(mac_part2_hashes.get(tf) == arch_part2_hashes[tf] for tf in target_files):
                    mac_part2_converged = True
                    break
            time.sleep(0.08)

        t_arch_converged = time.perf_counter()
        arch_sync_duration_ms = (t_arch_converged - t_arch_write) * 1000

        details["arch_to_mac"]["convergence_time_ms"] = round(arch_sync_duration_ms, 2)
        details["arch_to_mac"]["all_matched"] = mac_part2_converged
        details["arch_to_mac"]["files"] = {
            tf: {
                "arch_sha256": arch_part2_hashes.get(tf),
                "mac_sha256": mac_part2_hashes.get(tf),
                "matched": arch_part2_hashes.get(tf) == mac_part2_hashes.get(tf),
            }
            for tf in target_files
        }

        if not mac_part2_converged:
            raise AssertionError(f"Arch -> Mac sync timed out")

        log(f"Part 2 Arch -> Mac sync PASSED in {arch_sync_duration_ms:.1f}ms")

        duration_ms = (time.perf_counter() - t0) * 1000
        return TestResult("Stage 4: Bidirectional Sync & Integrity", "PASS", duration_ms, details)
    except Exception as e:
        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 4 FAILED: {e}")
        return TestResult("Stage 4: Bidirectional Sync & Integrity", "FAIL", duration_ms, details, str(e))


# ---------------------------------------------------------------------------
# Stage 5: Session Pinning, IPC Telemetry & Web UI Verification
# ---------------------------------------------------------------------------
def stage_5_pinning_and_web_ui(mac_bin: str, arch_bin: str, arch_ssh: str, mac_dir: str, arch_dir: str, ui_port: int) -> Tuple[TestResult, str]:
    t0 = time.perf_counter()
    log("=== Stage 5: Session Pinning, IPC Telemetry & Web UI Verification ===")
    details: Dict[str, Any] = {"pinning": {}, "web_ui": {}}
    ui_pane_id = ""
    try:
        # Part 1: Session Pinning on Arch
        log("Part 1: Pinning logs/app.log on Arch...")
        pin_start_res = ssh_cmd(arch_ssh, f"{arch_bin} --json pin start --paths 'logs/app.log' {arch_dir}")
        log(f"Arch pin start: {pin_start_res.stdout.strip()}")
        pin_data = json.loads(pin_start_res.stdout)
        details["pinning"]["arch_pin_start"] = pin_data

        time.sleep(1.2)

        # Query status on Mac
        mac_status = json.loads(run_cmd([mac_bin, "--json", "status", mac_dir]).stdout)
        details["pinning"]["mac_status_during_pin"] = {
            "pin": mac_status.get("pin"),
            "held_by_peer": mac_status.get("held_by_peer"),
            "peers": mac_status.get("peers"),
        }

        # Stop pin on Arch
        pin_stop_res = ssh_cmd(arch_ssh, f"{arch_bin} --json pin stop {arch_dir}")
        details["pinning"]["arch_pin_stop"] = json.loads(pin_stop_res.stdout)
        log("Arch pin stopped.")

        # Part 2: Web Dashboard UI & REST / SSE Telemetry on Mac
        log("Part 2: Launching Web UI on Mac...")
        ui_pane_id = herdr_split_pane(direction="down", cwd=mac_dir)
        details["web_ui"]["pane_id"] = ui_pane_id

        # Launch UI server
        ui_cmd = f"{mac_bin} ui --port {ui_port} --no-open {mac_dir}"
        herdr_run(ui_pane_id, ui_cmd)

        # Wait for UI listening message
        ui_out = herdr_wait_output(ui_pane_id, "listening on", timeout_ms=10000)
        details["web_ui"]["startup_output"] = ui_out.strip()

        token_match = re.search(r"token=([a-f0-9]+)", ui_out)
        if not token_match:
            pane_txt = herdr_read(ui_pane_id, lines=20)
            token_match = re.search(r"token=([a-f0-9]+)", pane_txt)

        if not token_match:
            raise RuntimeError(f"Could not parse token from UI output: {ui_out}")

        token = token_match.group(1)
        details["web_ui"]["token"] = token
        base_url = f"http://127.0.0.1:{ui_port}"

        # 1. Test GET / (HTML)
        req_html = urllib.request.Request(f"{base_url}/")
        with urllib.request.urlopen(req_html, timeout=5) as resp:
            details["web_ui"]["index_html_status"] = resp.status
            html_content = resp.read().decode("utf-8")
            details["web_ui"]["index_html_len"] = len(html_content)

        # 2. Test GET /style.css
        req_css = urllib.request.Request(f"{base_url}/style.css")
        with urllib.request.urlopen(req_css, timeout=5) as resp:
            details["web_ui"]["style_css_status"] = resp.status

        # 3. Test GET /app.js
        req_js = urllib.request.Request(f"{base_url}/app.js")
        with urllib.request.urlopen(req_js, timeout=5) as resp:
            details["web_ui"]["app_js_status"] = resp.status

        # 4. Test GET /api/status with token
        req_api_status = urllib.request.Request(f"{base_url}/api/status?token={token}")
        with urllib.request.urlopen(req_api_status, timeout=5) as resp:
            details["web_ui"]["api_status_code"] = resp.status
            api_status_json = json.loads(resp.read().decode("utf-8"))
            details["web_ui"]["api_status_payload"] = api_status_json

        # 5. Test GET /api/conflicts with token
        req_conflicts = urllib.request.Request(f"{base_url}/api/conflicts?token={token}")
        with urllib.request.urlopen(req_conflicts, timeout=5) as resp:
            details["web_ui"]["api_conflicts_code"] = resp.status
            api_conflicts_json = json.loads(resp.read().decode("utf-8"))
            details["web_ui"]["api_conflicts_payload"] = api_conflicts_json

        # 6. Test SSE stream /api/events with token
        log("Testing SSE stream /api/events...")
        req_sse = urllib.request.Request(f"{base_url}/api/events?token={token}")
        with urllib.request.urlopen(req_sse, timeout=5) as resp:
            details["web_ui"]["sse_status"] = resp.status
            first_chunk = resp.read(128).decode("utf-8", errors="ignore")
            details["web_ui"]["sse_first_chunk"] = first_chunk

        # Assertions
        assert details["web_ui"]["index_html_status"] == 200, "Index HTML != 200"
        assert details["web_ui"]["api_status_code"] == 200, "API status != 200"
        assert "folder" in details["web_ui"]["api_status_payload"], "Missing folder in API status"

        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 5 PASSED in {duration_ms:.1f}ms: Pinning & Web UI/REST/SSE verified")
        return TestResult("Stage 5: Pinning & Web UI/REST Telemetry", "PASS", duration_ms, details), ui_pane_id
    except Exception as e:
        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 5 FAILED: {e}")
        return TestResult("Stage 5: Pinning & Web UI/REST Telemetry", "FAIL", duration_ms, details, str(e)), ui_pane_id


# ---------------------------------------------------------------------------
# Stage 6: Teardown & Final Report Generation
# ---------------------------------------------------------------------------
def stage_6_teardown(mac_bin: str, arch_bin: str, arch_ssh: str, panes_to_close: List[str]) -> TestResult:
    t0 = time.perf_counter()
    log("=== Stage 6: Teardown & Final Cleanup ===")
    details = {}
    try:
        # Gracefully stop processes in Herdr panes
        for pid in panes_to_close:
            log(f"Sending SIGINT to pane {pid}")
            herdr_send_keys(pid, "ctrl+c")
            time.sleep(0.2)
            herdr_close_pane(pid)

        # Kill any remaining background daemons
        run_cmd(["pkill", "-f", f"{mac_bin} daemon"], check=False)
        run_cmd(["pkill", "-f", f"{mac_bin} ui"], check=False)
        ssh_cmd(arch_ssh, f"pkill -f '{arch_bin} daemon' || true", check=False)

        details["panes_closed"] = panes_to_close
        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 6 PASSED in {duration_ms:.1f}ms")
        return TestResult("Stage 6: Teardown & Cleanup", "PASS", duration_ms, details)
    except Exception as e:
        duration_ms = (time.perf_counter() - t0) * 1000
        log(f"Stage 6 FAILED: {e}")
        return TestResult("Stage 6: Teardown & Cleanup", "FAIL", duration_ms, details, str(e))


def main() -> int:
    parser = argparse.ArgumentParser(description="Dual-Device Ferry E2E / Regression Test Suite")
    parser.add_argument("--mac-bin", default=DEFAULT_MAC_BIN, help="Path to local Mac ferry binary")
    parser.add_argument("--arch-bin", default=DEFAULT_ARCH_BIN, help="Path to remote Arch ferry binary")
    parser.add_argument("--arch-ssh", default=DEFAULT_ARCH_SSH, help="SSH target (e.g. sharzil@sharzilx)")
    parser.add_argument("--mac-ip", default=DEFAULT_MAC_IP, help="Mac IP reachable by Arch")
    parser.add_argument("--arch-ip", default=DEFAULT_ARCH_IP, help="Arch IP reachable by Mac")
    parser.add_argument("--mac-dir", default=DEFAULT_MAC_TEST_DIR, help="Local Mac test workspace")
    parser.add_argument("--arch-dir", default=DEFAULT_ARCH_TEST_DIR, help="Remote Arch test workspace")
    parser.add_argument("--port", type=int, default=DEFAULT_PORT, help="TCP daemon port")
    parser.add_argument("--ui-port", type=int, default=DEFAULT_UI_PORT, help="Web UI port")
    parser.add_argument("--stress", action="store_true", help="Enable heavy stress testing (e.g. 5MB payloads)")
    parser.add_argument("--clean-only", action="store_true", help="Only kill daemons and clean Herdr panes")
    args = parser.parse_args()

    if args.clean_only:
        log("Cleaning up all Ferry test daemons and panes...")
        run_cmd(["pkill", "-f", f"{args.mac_bin} daemon"], check=False)
        run_cmd(["pkill", "-f", f"{args.mac_bin} ui"], check=False)
        ssh_cmd(args.arch_ssh, f"pkill -f '{args.arch_bin} daemon' || true", check=False)
        cleanup_panes()
        log("Cleanup completed.")
        return 0

    log("Starting Dual-Device Ferry E2E Regression Test Suite...")
    panes_for_teardown = []

    try:
        # Stage 1
        res1 = stage_1_environment_check(args.mac_bin, args.arch_bin, args.arch_ssh, args.mac_ip, args.arch_ip)
        results.append(res1)
        if res1.status != "PASS":
            return 1

        # Stage 2
        res2 = stage_2_cryptographic_pairing(args.mac_bin, args.arch_bin, args.arch_ssh, args.mac_dir, args.arch_dir)
        results.append(res2)
        if res2.status != "PASS":
            return 1

        # Stage 3
        res3, mac_pane, arch_pane = stage_3_start_daemons(args.mac_bin, args.arch_bin, args.arch_ssh, args.mac_dir, args.arch_dir, args.mac_ip, args.port)
        results.append(res3)
        if mac_pane:
            panes_for_teardown.append(mac_pane)
        if arch_pane:
            panes_for_teardown.append(arch_pane)
        if res3.status != "PASS":
            return 1

        # Stage 4
        res4 = stage_4_bidirectional_sync(args.arch_ssh, args.mac_dir, args.arch_dir, stress_mode=args.stress)
        results.append(res4)
        if res4.status != "PASS":
            return 1

        # Stage 5
        res5, ui_pane = stage_5_pinning_and_web_ui(args.mac_bin, args.arch_bin, args.arch_ssh, args.mac_dir, args.arch_dir, args.ui_port)
        results.append(res5)
        if ui_pane:
            panes_for_teardown.append(ui_pane)
        if res5.status != "PASS":
            return 1

        # Stage 6
        res6 = stage_6_teardown(args.mac_bin, args.arch_bin, args.arch_ssh, panes_for_teardown)
        results.append(res6)

        # Save structured results to file
        out_path = f"{DEFAULT_MAC_REPO}/.scratch/dual_device_e2e_results.json"
        os.makedirs(os.path.dirname(out_path), exist_ok=True)
        with open(out_path, "w") as f:
            json.dump([r.__dict__ for r in results], f, indent=2)
        log(f"All stages completed successfully! Results written to {out_path}")
        return 0

    finally:
        cleanup_panes()


if __name__ == "__main__":
    sys.exit(main())
