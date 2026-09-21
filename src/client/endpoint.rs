use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

mod catalog;
mod ssh_metadata;

pub(crate) use catalog::{EndpointCatalog, SavedSshEndpoint};
pub(crate) use ssh_metadata::{SshMachineMetadata, SshMetadataCache};

const PROFILE_ID_BYTES: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct ProfileId(String);

impl ProfileId {
    pub(crate) fn parse(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        if value.len() != PROFILE_ID_BYTES * 2
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("endpoint profile id must be 32 lowercase hexadecimal characters".into());
        }
        Ok(Self(value))
    }

    pub(crate) fn generate() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);

        let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let digest = Sha256::digest(format!("{}:{now}:{sequence}", std::process::id()).as_bytes());
        Self(
            digest[..PROFILE_ID_BYTES]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        )
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProfileId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ClientEndpointId {
    Local,
    Ssh(ProfileId),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct EndpointNegotiation {
    methods: HashSet<String>,
    capabilities: HashSet<String>,
}

impl EndpointNegotiation {
    pub(crate) fn new(methods: Vec<String>, capabilities: Vec<String>) -> Self {
        Self {
            methods: methods.into_iter().collect(),
            capabilities: capabilities.into_iter().collect(),
        }
    }

    pub(crate) fn supports_surface_interest(&self) -> bool {
        self.capabilities.contains("surface_interest")
            && self.capabilities.contains("presentation_effects_fence")
            && self.methods.contains("client_shell.surface.set")
    }

    pub(crate) fn supports_health_check(&self) -> bool {
        self.capabilities.contains("health_check")
    }
}
