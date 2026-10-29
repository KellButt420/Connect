use std::collections::HashSet;

use connect_net::iroh::EndpointId;
use connect_proto::Permissions;
use serde::{Deserialize, Serialize};

/// Hosts remembered on the Connect page.
const MAX_RECENT: usize = 8;

/// Everything the app remembers between runs.
#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Shown to the other side.
    pub device_name: String,
    pub accepting: bool,
    pub permissions: Permissions,
    /// Devices let in without asking.
    pub trusted: Vec<KnownDevice>,
    /// Hosts connected to, most recent first.
    pub recent: Vec<KnownDevice>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct KnownDevice {
    /// User-ID, stored as text so the settings file stays readable.
    pub id: String,
    pub name: String,
}

impl KnownDevice {
    fn new(id: EndpointId, name: &str) -> Self {
        Self { id: id.to_string(), name: name.to_owned() }
    }

    pub fn user_id(&self) -> Option<EndpointId> {
        self.id.parse().ok()
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            device_name: crate::device_name(),
            accepting: true,
            permissions: Permissions::default(),
            trusted: Vec::new(),
            recent: Vec::new(),
        }
    }
}

impl Settings {
    pub fn trusted_ids(&self) -> HashSet<EndpointId> {
        self.trusted.iter().filter_map(KnownDevice::user_id).collect()
    }

    pub fn trust(&mut self, id: EndpointId, name: &str) {
        self.trusted.retain(|d| d.user_id() != Some(id));
        self.trusted.push(KnownDevice::new(id, name));
    }

    pub fn add_recent(&mut self, id: EndpointId, name: &str) {
        self.recent.retain(|d| d.user_id() != Some(id));
        self.recent.insert(0, KnownDevice::new(id, name));
        self.recent.truncate(MAX_RECENT);
    }
}
