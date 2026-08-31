# 06: Fix TUI pin toggle on active pin and throttle disconnected daemon event stream

**What to build:** In the terminal TUI dashboard, pressing the `P` key shortcut toggles an active session pin off even when no files are currently being held, properly releasing or ending the pin instead of generating duplicate pin errors. When the TUI is launched without an active daemon process, the UI displays a clear disconnected indicator in the header, throttles reconnection attempts with exponential backoff, and suppresses repetitive stream closure messages in the activity feed.

**Blocked by:** 05: One-click pairing, short-code join, and discovered devices in Web UI and GUI

**Status:** complete

- [x] Pressing `P` in the TUI when a pin is active dispatches a release command regardless of whether files are actively held
- [x] TUI activity feed displays a single clean status update on pin state transition
- [x] When the daemon is offline, the TUI event listener applies exponential backoff on retry attempts
- [x] Consecutive disconnect error messages are deduplicated from the visual activity feed
- [x] TUI header displays an explicit disconnected status banner when the backend stream is unreachable
- [x] Automated unit tests verify key event handling and reconnect throttle behavior
