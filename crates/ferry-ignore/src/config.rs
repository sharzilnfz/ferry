#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IgnoreConfig {
    #[serde(default)]
    pub honor_gitignore: bool,

    #[serde(default)]
    pub presets: Vec<String>,

    #[serde(default)]
    pub overrides: Vec<String>,
}
