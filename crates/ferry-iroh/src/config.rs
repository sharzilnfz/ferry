

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
            mdns: None,
            force_relay: false,
            dial_timeout: Duration::from_secs(10),
            routes: None,
        }
    }
}

impl IrohConfig {
    pub fn builder() -> IrohConfigBuilder {
        IrohConfigBuilder(IrohConfig::default())
    }

    
    pub fn resolve_secret(&self) -> Option<[u8; 32]> {
        if let Some(bytes) = self.secret {
            return Some(bytes);
        }
        self.device_identity
            .as_ref()
            .map(crate::identity::endpoint_seed_from_device_identity)
    }
}


#[derive(Debug)]
pub struct IrohConfigBuilder(IrohConfig);

impl IrohConfigBuilder {
    
    pub fn device_identity(mut self, id: &DeviceIdentity) -> Self {
        self.0.device_identity = Some(id.clone());
        self
    }

    
    pub fn secret(mut self, seed: [u8; 32]) -> Self {
        self.0.secret = Some(seed);
        self
    }

    pub fn relays(mut self, relays: RelaySetting) -> Self {
        self.0.relays = relays;
        self
    }

    pub fn mdns(mut self, mdns: MdnsSetting) -> Self {
        self.0.mdns = Some(mdns);
        self
    }

    pub fn force_relay(mut self, yes: bool) -> Self {
        self.0.force_relay = yes;
        self
    }

    pub fn dial_timeout(mut self, d: Duration) -> Self {
        self.0.dial_timeout = d;
        self
    }

    pub fn routes(mut self, routes: RouteTable) -> Self {
        self.0.routes = Some(routes);
        self
    }

    pub fn build(self) -> IrohConfig {
        self.0
    }
}
