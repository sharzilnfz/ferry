//! Tuned built-in defaults: the baseline every folder gets before any user
//! artifact speaks. Each entry carries its rationale inline; changing this
//! list is a product decision, not a code cleanup (defaults decide whether
//! people trust the tool — CONTEXT.md, "Selective rules").
//!
//! Layers (see crate docs): defaults are the LOWEST layer. A `ferry.ignore`
//! line beats them; presets and config overrides beat that.

/// Default exclude lines, compiled into every rule set, in order.
///
/// Rationale, entry by entry:
///
/// - `.DS_Store` — macOS Finder metadata. Written constantly while browsing,
///   meaningless on any other machine.
/// - `Thumbs.db`, `desktop.ini` — Windows Explorer thumbnail cache and
///   folder-view metadata (localized names, view settings). Machine-local by
///   definition.
/// - `*.swp`, `*~` — vim swap files and emacs/kate-style backup droppings.
///   Transient editor scratch state; never meaningful to a peer.
/// - `node_modules/` — **opt-in** (excluded by default). Enormous file count,
///   reinstallable from the lockfile, and historically THE directory that
///   destroys naive sync tools (research/use-cases.md, cross-cutting finding
///   1). Power users on offline/air-gapped machines can opt back in with one
///   line (`!node_modules/` in `ferry.ignore`); everyone else gets sane
///   behavior out of the box.
/// - `.env`, `.env.*` — **opt-in with a loud warning** (excluded by default).
///   Roughly 54% of `.env` files contain detectable secrets and 65% of leaked
///   secrets live in env-class files (research archetype 9 / `GitGuardian`).
///   E2E encryption protects the bytes, but sharing credentials to another
///   device must be a deliberate act: opting in is one `!.env` line, and the
///   share-time secret scan (`crate::secrets`) flags likely credentials.
pub const DEFAULT_RULES: &[&str] = &[
    ".DS_Store",
    "Thumbs.db",
    "desktop.ini",
    "*.swp",
    "*~",
    "node_modules/",
    ".env",
    ".env.*",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_rules_cover_the_documented_set() {
        // The ticket's verbatim default set; keep both in lockstep.
        assert_eq!(
            DEFAULT_RULES,
            &[
                ".DS_Store",
                "Thumbs.db",
                "desktop.ini",
                "*.swp",
                "*~",
                "node_modules/",
                ".env",
                ".env.*"
            ]
        );
    }
}
