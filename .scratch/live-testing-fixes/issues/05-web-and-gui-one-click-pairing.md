# 05: One-click pairing, short-code join, and discovered devices in Web UI and GUI

**What to build:** The Web dashboard and Desktop GUI provide an intuitive pairing workflow: a "Share Folder" dialog displays a prominent 6-character short code with a Copy button and QR code, a "Join Folder" dialog allows joining via code and destination path, and the device table lists discovered nearby network instances with a 1-click "Pair" button that triggers the pairing handshake directly from the UI.

**Blocked by:** 04: Transparent background daemon auto-spawning for CLI and UI commands

**Status:** ready-for-agent

- [x] Web UI and GUI "Share Folder" dialogs render the 6-character pairing code, QR representation, and live completion status
- [x] Web UI and GUI provide a direct code-entry input for joining remote folders
- [x] Nearby discovered devices on the network are enumerated in the frontend device table
- [x] Clicking "Pair" next to a discovered device initiates the network pairing handshake
- [x] Automated UI backend tests verify short-code generation, network join endpoints, and discovered device listing
