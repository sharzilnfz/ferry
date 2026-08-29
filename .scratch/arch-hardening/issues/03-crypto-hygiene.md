# T-03: Crypto hygiene — honest constant-time compare + one key-derivation home

Status: done

1. **Constant-time MAC compare** (`crates/ferry-crypto/src/pairing.rs:292-
296`): `PairingResponse::verify` docstring claims constant-time comparison
but uses plain `!=` on `[u8;32]`. Fix: use the `subtle` crate's
`ConstantTimeEq` (already transitively available via the AEAD stack — verify,
then declare it directly if used). Keep the docstring truthful afterwards.

2. **Move grant-key derivation into ferry-crypto** (`crates/ferry-cli/src/
commands/pairing.rs:342` does raw `hk.expand(GRANT_INFO, ...)`): add a named
function on ferry-crypto returning `Result`, delete ferry-cli's direct
dependencies on chacha20poly1305/hkdf/sha2/blake3/qrcode where the operation
is already (or should be) behind a ferry-crypto function. The goal: ferry-cli
depends on ferrry-crypto only, not on raw crypto crates. Where the CLI truly
needs qrcode rendering, keep it, but key handling moves wholesale. Remove now
unused deps from crates/ferry-cli/Cargo.toml.

3. While in pairing.rs, remove panic-shaped fixed-offset parsing
(`try_into().expect(...)` around lines 192-195, 284-286): replace with a
length-checked reader pattern so the invariant lives in code, not the
reader's head.

Acceptance: ferry-cli Cargo.toml has no direct chacha20poly1305/hkdf/sha2
deps (unless genuinely irreplaceable — document any exception); round-trip
pairing tests still green; a test asserts MAC mismatch fails and match passes.
