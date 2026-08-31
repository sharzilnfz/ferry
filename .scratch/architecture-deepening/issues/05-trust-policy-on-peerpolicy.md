# 05: Trust policy on PeerPolicy

**What to build:** Trust decisions live next to where policy is parsed.
PeerPolicy exposes the remote-peer derivation (the configured device set minus
self) and the expected-peer resolution for a session. An empty allow-list
refuses to connect to unpaired devices instead of falling back to trust on
first use, honoring the glossary's explicit-pairing rule and ADR-0002. The
three copies of the self-filter walk across the sync engine and the daemon
supervisor are deleted. Any trust-on-first-use behavior returns only behind an
explicit config flag, recorded in a new ADR.

**Blocked by:** None (can start immediately).

**Status:** done

- [x] Remote-peer derivation and expected-peer resolution exist once, on PeerPolicy
- [x] Empty allow-list refuses connections to unpaired devices; no TOFU default
- [x] The three duplicated self-filter walks are deleted
- [x] Existing peer-policy tests updated for the refuse default and still passing
- [x] If trust-on-first-use survives as an opt-in, it is config-flag gated and an ADR records the reversal
