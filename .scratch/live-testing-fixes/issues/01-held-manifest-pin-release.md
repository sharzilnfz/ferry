# 01: Persist un-adopted remote manifests into store during holds and fix pin release

**What to build:** When a developer edits a project folder under an active session pin, incoming sync exchanges from remote peers are held rather than merged immediately. The incoming remote manifest bytes are stored in the local content-addressed blob store during the hold. When the developer runs the pin release command, the held remote manifest is retrieved and evaluated in a three-way reconciliation against the baseline and local manifest: non-conflicting modifications land in the working tree, conflicting changes are saved as quarantined conflict files with timestamped extensions, entries are appended to the conflict log, and the pin session ends cleanly without missing-manifest errors.

**Blocked by:** None (can start immediately)

**Status:** done

- [x] Incoming sync exchanges store remote manifest bytes in the store whenever changes are held by an active pin
- [x] Running pin release loads held manifests from the store and reconciles them against baseline and local state
- [x] Non-conflicting held modifications are applied to the working directory on pin release
- [x] Conflicting held modifications produce quarantined conflict files with format `<file>.ferry-conflict.<device>-<timestamp>`
- [x] Releasing a pin appends conflict entries to the conflict log and cleans up internal held peer state
- [x] Automated tests verify that holding remote edits and executing pin release completes with exit code 0 and zero missing-manifest errors
