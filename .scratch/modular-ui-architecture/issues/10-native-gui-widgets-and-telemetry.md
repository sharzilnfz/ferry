# 10: Native GUI Widgets (Pulse Beacon, Telemetry Strip & Fleet List)

**What to build:** Build the primary stage widgets for `ferry-gui`, including the pulsating status beacon (supporting Synced, Syncing, Holding, and Conflict states), the hairline telemetry strip (displaying Root Hash, Held Edits, Conflicts, Cipher, and Transport), and the connected device fleet rows.

**Blocked by:** 09 (Native GUI Crate Bootstrap), 04 (Push Event Streaming)

**Status:** ready-for-human

- [x] Custom `egui` painter renders the pulsating status beacon with animated aura expansions matching the web design specifications.
- [x] The hairline telemetry strip renders root manifest hex, held changes count, conflict count, encryption cipher (`Age-X25519`), and transport (`QUIC`/`TCP`).
- [x] Connected device fleet list renders paired device IDs, agreement timestamps, and connectivity status pills.
- [x] Hero action buttons (Sync Now, Hold Edits / Release Pin) trigger corresponding `UiBackend` asynchronous calls cleanly.
