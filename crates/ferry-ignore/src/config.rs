//! Per-folder ignore configuration (serde-friendly; this is the shape the
//! future CLI persists per folder).

/// Configuration for one folder's selective rules.
///
/// Layer order and the honest `.gitignore` trade-off are documented in the
/// crate docs; here: fields only.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IgnoreConfig {
    /// Honor `.gitignore` files in addition to `ferry.ignore`.
    ///
    /// Default **off**. Both directions, honestly:
    ///
    /// - ON: sync respects VCS intent automatically (build outputs, vendored
    ///   junk stay out). BUT it silently drops exactly the files many users
    ///   most want synced — `.env`-class files are gitignored almost
    ///   everywhere, and "sync everything git doesn't" is Ferry's thesis —
    ///   and couples sync behavior to unrelated git whims (a broad `dist/`
    ///   or `data/` ignore changes what syncs).
    /// - OFF (default): Ferry carries what git refuses, which is the point;
    ///   the risk is that deliberately-git-ignored junk (`.venv/`, scratch
    ///   dirs) also syncs unless mirrored into `ferry.ignore`. The share-time
    ///   secret scan is the safety net for the dangerous subset.
    #[serde(default)]
    pub honor_gitignore: bool,
    /// Built-in agent-state preset ids to apply (`claude`, `opencode`).
    /// Applied between root `ferry.ignore` and [`Self::overrides`]; unknown
    /// ids fail construction loudly.
    #[serde(default)]
    pub presets: Vec<String>,
    /// Highest-precedence user rule lines, verbatim gitignore syntax. These
    /// win over every other ROOT-level layer (defaults, ferry.ignore,
    /// presets).
    #[serde(default)]
    pub overrides: Vec<String>,
}
