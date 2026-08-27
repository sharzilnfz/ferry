# 11: Native GUI Pairing Ritual & Secret Warning Modal

**What to build:** Implement modal dialogs in `ferry-gui` for the device pairing workflow: generating share offers, displaying secret scanning warnings with override checkboxes, copying short pairing codes, and accepting peer payload files with QR render support.

**Blocked by:** 09 (Native GUI Crate Bootstrap), 03 (Daemon IPC Adapter)

**Status:** ready-for-agent

- [ ] "Pair Device" modal allows initiating a share or accepting an incoming offer payload.
- [ ] If unignored secrets (e.g. `.env` files) are detected, a warning banner displays the offending paths and requires an explicit confirmation before proceeding.
- [ ] Successful share generation displays the 32-character pairing token / short code with a one-click copy button.
- [ ] Accepting a valid offer payload completes the key exchange and updates the connected device fleet list in real-time.
