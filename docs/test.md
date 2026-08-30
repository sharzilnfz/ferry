ui:

  Run the complete automated Dual-Device Ferry Test Suite (P2P Sync, Interactive TUI, and Web UI) across two test machines inside Herdr.

    ### Environment & System Topology
    - **Host A (Local)**: `<LOCAL_REPO_PATH>` (IP: `<DEVICE_A_IP>`)
    - **Host B (Remote)**: `<USER>@<DEVICE_B_HOST>` (IP: `<DEVICE_B_IP>`)
    - **Ferry Binaries**:
      - Host A: `target/release/ferry`
      - Host B: `~/.cargo/bin/ferry`
    - **Herdr Environment**: `HERDR_ENV=1`

    ---

    ### Execution Steps
    1. **Execute the pre-built unified test suite**:
       ```bash
       ./scripts/dual_device_full_suite.py --stress

  2. Verify that all 8 test stages pass sequentially:
      • Stage 1: Connectivity: Ping latency check (RTT < 10ms) & binary verification.
      • Stage 2: 3-Way Pairing: Out-of-band cryptographic handshake (pair-offer → pair-response → pair-grant → FMK wrap).
      • Stage 3: Live P2P Daemons: Split Herdr panes for Mac listener (:44001) and Arch connector dialing peer.
      • Stage 4: Bidirectional Sync: Rapid multi-tier payload sync (text, JSON, YAML, binaries up to 5MB, 100-line append log) with byte-exact SHA-256 matches.
      • Stage 5: Dual-Device Interactive TUI:
          • Mac TUI: Launch in Herdr pane, assert Ferry Sync Engine header, dispatch keystrokes (c for modal, esc to dismiss, p to pin, q to quit).
          • Arch TUI (over SSH): Launch in Herdr pane, assert remote header & peer table, dispatch keystrokes (c, esc, q).
      • Stage 6: Dual-Device Web UI:
          • Mac Web UI (:8098): Token authentication, static assets (HTML/CSS/JS), REST /api/status, and SSE stream /api/events.
          • Arch Web UI (:8099): Launch over SSH, query remote API over network with token.
      • Stage 7: Session Pinning: Active writer hold (ferry pin start) propagation and release (ferry pin stop).
      • Stage 8: Teardown: Clean SIGINT shutdown of all daemons, Web UI servers, and Herdr panes.
  3. Report:
      • Read .scratch/dual_device_full_suite_results.json and present a structured summary table containing stage status, convergence latencies, and checksum verifications.


cli:
    ### Environment & Pre-requisites
    - **Host A Repo**: `<LOCAL_REPO_PATH>`
    - **Host A Binary**: `target/release/ferry` (IP: `<DEVICE_A_IP>`)
    - **Host B Laptop**: `<USER>@<DEVICE_B_HOST>` (IP: `<DEVICE_B_IP>`, Binary: `~/.cargo/bin/ferry`)
    - **Herdr Multiplexer**: `HERDR_ENV=1`

    ### Execution Task
    1. Execute the packaged test harness in stress mode:
       ```bash
       ./scripts/dual_device_e2e_test.py --stress
       ```
    2. Verify that all 6 stages pass:
       - **Stage 1**: Network ping (RTT < 10ms) & binary existence.
       - **Stage 2**: Cryptographic 3-way pairing (`pair-offer` -> `pair-response` -> `pair-grant` -> FMK agreement).
       - **Stage 3**: Herdr pane daemons (Mac listener on `:44001`, Arch connector dialing peer).
       - **Stage 4**: Bidirectional file sync (11 files including 1MB & 5MB binaries + 100-line append log) converging within 8.0s with byte-exact SHA-256 matches.
       - **Stage 5**: Session pinning (`ferry pin start`) hold propagation and authenticated Web UI (`/api/status`, `/api/conflicts`, `/api/events` SSE).
       - **Stage 6**: Graceful SIGINT shutdown and closing of Herdr worker panes.
    3. Read `.scratch/dual_device_e2e_results.json` and present a concise summary table of convergence latencies and checksum verifications.