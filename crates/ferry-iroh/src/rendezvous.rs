pub fn topic_for_code(code: &str) -> String {
    format!("ferry-pair-{}", code.to_ascii_lowercase())
}

pub fn service_name_for_code(code: &str) -> String {
    format!("ferry-pair-{}", code.to_ascii_uppercase())
}
