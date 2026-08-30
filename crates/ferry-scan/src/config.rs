



use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanConfig {
    
    pub quiet_window: Duration,
    
    
    
    pub audit_interval: Duration,
    
    
    pub poll_interval: Duration,
    
    pub parent_manifest_id: Option<ferry_store::BlobId>,
}

impl Default for ScanConfig {
    fn default() -> Self {
        ScanConfig {
            quiet_window: Duration::from_millis(500),
            audit_interval: Duration::from_hours(24),
            poll_interval: Duration::from_secs(10),
            parent_manifest_id: None,
        }
    }
}
