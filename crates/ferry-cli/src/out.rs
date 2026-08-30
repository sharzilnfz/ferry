




#[derive(Debug)]
pub struct Output {
    
    
    pub json: serde_json::Value,
    
    pub human: String,
    
    
    pub exit_code: u8,
}

impl Output {
    pub fn new(json: serde_json::Value, human: impl Into<String>) -> Self {
        Output {
            json,
            human: human.into(),
            exit_code: 0,
        }
    }
}


pub fn error_text(code: &str, message: &str, hint: &str) -> String {
    format!("error: {message} (code={code})\nhint: {hint}")
}


pub fn error_json(code: &str, message: &str, hint: &str) -> String {
    serde_json::json!({
        "error": message,
        "code": code,
        "hint": hint,
    })
    .to_string()
}
