#!/usr/bin/env python3
"""
Ponytail baseline and verification harness.
Captures baseline symbol counts, crate graph metrics, and verifies test passes across key seams.
"""

import os
import subprocess
import sys
import json

TARGET_SYMBOLS = {
    "global_directory": ("crates/ferry-iroh", "GLOBAL_DIRECTORY"),
    "ferry_pin_crate_deps": ("crates", 'ferry-pin = {'),
    "direction_cipher": ("crates/ferry-sync", "DirectionCipher"),
    "backend_triplication": ("crates/ferry-daemon/src/ui/backend.rs", "struct AutoBackend"),
    "hold_matcher_helper": ("crates/ferry-sync-engine", "pub fn hold_matcher"),
    "iroh_config_builder": ("crates/ferry-iroh", "pub struct IrohConfigBuilder"),
    "civil_from_days": ("crates/ferry-platform", "pub fn civil_from_days"),
    "manual_lru_order": ("crates/ferry-store/src/pack.rs", "order: VecDeque"),
    "duplicate_format_bytes": ("crates/ferry-gui", "fn format_bytes"),
}

def count_symbol(path, pattern):
    if not os.path.exists(path):
        return 0
    cmd = ["rg", "-F", pattern, path]
    res = subprocess.run(cmd, capture_output=True, text=True)
    if res.returncode != 0:
        return 0
    return len([line for line in res.stdout.strip().split("\n") if line])

def get_workspace_crates():
    cmd = ["cargo", "metadata", "--no-deps", "--format-version", "1"]
    res = subprocess.run(cmd, capture_output=True, text=True)
    if res.returncode != 0:
        return []
    data = json.loads(res.stdout)
    return [p["name"] for p in data.get("packages", [])]

def run_seam_tests():
    seams = [
        ("ferry-sync-engine", ["cargo", "test", "-p", "ferry-sync-engine"]),
        ("ferry-crypto", ["cargo", "test", "-p", "ferry-crypto"]),
        ("ferry-proto", ["cargo", "test", "-p", "ferry-proto"]),
        ("ferry-platform", ["cargo", "test", "-p", "ferry-platform"]),
        ("ferry-store", ["cargo", "test", "-p", "ferry-store"]),
        ("ferry-scan", ["cargo", "test", "-p", "ferry-scan"]),
        ("ferry-folder", ["cargo", "test", "-p", "ferry-folder"]),
        ("ferry-ipc", ["cargo", "test", "-p", "ferry-ipc"]),
        ("ferry-tui", ["cargo", "test", "-p", "ferry-tui"]),
        ("ferry-gui", ["cargo", "test", "-p", "ferry-gui"]),
    ]
    results = {}
    all_ok = True
    for name, cmd in seams:
        proc = subprocess.run(cmd, capture_output=True, text=True)
        ok = (proc.returncode == 0)
        results[name] = ok
        if not ok:
            all_ok = False
            print(f"[FAIL] {name} test failed:\n{proc.stderr}\n{proc.stdout}")
        else:
            print(f"[PASS] {name} tests passed")
    return all_ok, results

def main():
    print("=== Ponytail Structural Baseline Snapshot ===")
    counts = {}
    for key, (path, pattern) in TARGET_SYMBOLS.items():
        c = count_symbol(path, pattern)
        counts[key] = c
        print(f"  {key} ('{pattern}' in {path}): {c}")

    crates = get_workspace_crates()
    print(f"\nWorkspace Crates ({len(crates)}): {', '.join(sorted(crates))}")

    if "--test" in sys.argv or "-t" in sys.argv:
        print("\n=== Running Core Seam Tests ===")
        all_ok, results = run_seam_tests()
        if not all_ok:
            sys.exit(1)
        print("\nAll core seam tests passed.")

if __name__ == "__main__":
    main()
