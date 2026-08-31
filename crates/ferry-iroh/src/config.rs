

use std::time::Duration;

use ferry_crypto::identity::DeviceIdentity;

use crate::directory::RouteTable;


#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RelaySetting {
    
    
    #[default]
    Disabled,
    
    
    N0,
    
    
    Custom(Vec<String>),
}






#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdnsSetting {
    
    
    
    pub service_name: String,
    
    pub advertise: bool,
}






#[derive(Debug, Clone)]
pub struct IrohConfig {
    
    
    
    pub secret: Option<[u8; 32]>,
    
    
    pub device_identity: Option<DeviceIdentity>,
    pub relays: RelaySetting,
    pub mdns: Option<MdnsSetting>,
    
    
    
    
    
    
    pub force_relay: bool,
    
    
    pub dial_timeout: Duration,
    
    pub routes: Option<RouteTable>,
}

impl Default for IrohConfig {
    fn default() -> Self {
        IrohConfig {
            secret: None,
            device_identity: None,
            relays: RelaySetting::Disabled,
            mdns: Some(MdnsSetting {
                service_name: "ferry-sync".into(),
                advertise: true,
            }),
            force_relay: false,
            dial_timeout: Duration::from_secs(10),
            routes: None,
        }
    }
}

impl IrohConfig {
    pub fn resolve_secret(&self) -> Option<[u8; 32]> {
        if let Some(bytes) = self.secret {
            return Some(bytes);
        }
        self.device_identity
            .as_ref()
            .map(crate::identity::endpoint_seed_from_device_identity)
    }
}
