# 02: Network rendezvous over P2P topic and mutual key wrapping for share and join

**What to build:** A developer running the share command on Machine A generates a 6-character short code and QR code, broadcasting an encrypted pairing offer across the network on a rendezvous topic derived from the code. A developer on Machine B running the join command with the 6-character code connects over the network, completes the cryptographic pairing handshake, decrypts the folder master key, and adopts the folder. Both devices update their configuration allow-lists with each other's device public keys and wrapped folder keys so subsequent background sync sessions authorize without manual file transfers.

**Blocked by:** 01: Persist un-adopted remote manifests into store during holds and fix pin release

**Status:** complete

- [x] Running the share command publishes an encrypted pairing offer to the network rendezvous topic derived from the 6-character pairing code
- [x] Running the join command with a 6-character code discovers the offer over the network, completes the cryptographic handshake, and adopts the folder
- [x] The sharing device receives the joiner's public key, wraps the folder master key, and commits the updated allow-list
- [x] The joining device records the sharer's public key wrap in its folder configuration
- [x] Pairing sessions expire after their configured validity period and cannot be reused once consumed
- [x] Automated multi-process tests verify cross-process and network share and join workflows complete and allow subsequent sync without authorization denials
