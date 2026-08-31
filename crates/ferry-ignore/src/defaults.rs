






























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
