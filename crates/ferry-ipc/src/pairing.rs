use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingCode(pub String);

impl PairingCode {
    #[must_use]
    pub fn new(code: String) -> Self {
        Self(code)
    }

    pub fn generate<R: rand::Rng>(rng: &mut R) -> Self {
        const CHARSET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
        let s: String = (0..6)
            .map(|_| {
                let idx = rng.gen_range(0..CHARSET.len());
                CHARSET[idx] as char
            })
            .collect();
        Self(s)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn verify(&self, other: &str) -> bool {
        if self.0.len() != other.len() {
            return false;
        }
        let mut diff = 0u8;
        for (a, b) in self.0.as_bytes().iter().zip(other.as_bytes()) {
            diff |= a ^ b;
        }
        diff == 0
    }
}

impl std::fmt::Display for PairingCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for PairingCode {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatePairingRequest {
    pub folder_id: String,
}

impl CreatePairingRequest {
    #[must_use]
    pub fn new(folder_id: String) -> Self {
        Self { folder_id }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatePairingResponse {
    pub code: String,
    pub expires_at: String,
}

impl CreatePairingResponse {
    #[must_use]
    pub fn new(code: String, expires_at: String) -> Self {
        Self { code, expires_at }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinPairingRequest {
    pub code: String,
    pub target_dir: PathBuf,
}

impl JoinPairingRequest {
    #[must_use]
    pub fn new(code: String, target_dir: PathBuf) -> Self {
        Self { code, target_dir }
    }
}
