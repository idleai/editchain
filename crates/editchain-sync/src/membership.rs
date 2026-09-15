//! A replica owner's explicit device approvals, independent of discovery.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use editchain_store::durable::atomic_write;
use serde::{Deserialize, Serialize};

use crate::{invalid, PublicDevice, Replica};

/// Maximum approved remote devices for the initial small-mesh implementation.
pub const MAX_DEVICES: usize = 32;

/// Workspace-scoped local admission policy, read again on every peer turn.
#[derive(Debug)]
pub struct Membership {
    root: PathBuf,
    space: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Members {
    version: u16,
    space: String,
    devices: BTreeMap<String, PublicDevice>,
}

impl Membership {
    /// Open policy for an already configured space. This cannot rebind a chain.
    ///
    /// # Errors
    /// Rejects an unconfigured or mismatched space, or unavailable local storage.
    pub fn open(root: &Path, space: &str) -> io::Result<Self> {
        if !root.join("multiplayer/scope.json").exists() {
            return Err(invalid("configure sharing before admitting devices"));
        }
        let _replica = Replica::open(root, space, false)?;
        Ok(Self {
            root: root.to_owned(),
            space: space.to_owned(),
        })
    }

    /// List explicitly approved public device identities.
    ///
    /// # Errors
    /// Rejects unreadable, corrupt or oversized policy metadata.
    pub fn devices(&self) -> io::Result<Vec<PublicDevice>> {
        Ok(self.load()?.devices.into_values().collect())
    }

    /// Approve one device after the local user accepts its invitation identity.
    ///
    /// # Errors
    /// Rejects malformed certificates, the device bound, contention or failed fsync.
    pub fn approve(&self, certificate: &str) -> io::Result<PublicDevice> {
        let device = PublicDevice::parse(certificate)?;
        let _writer = crate::writer(&self.root)?;
        let mut members = self.load()?;
        drop(
            members
                .devices
                .insert(device.fingerprint.clone(), device.clone()),
        );
        if members.devices.len() > MAX_DEVICES {
            return Err(invalid("approved device limit exceeded"));
        }
        self.save(&members)?;
        Ok(device)
    }

    /// Remove a local approval. Live peer turns recheck this durable policy.
    ///
    /// # Errors
    /// Returns metadata, writer contention or failed fsync errors.
    pub fn revoke(&self, fingerprint: &str) -> io::Result<()> {
        let _writer = crate::writer(&self.root)?;
        let mut members = self.load()?;
        drop(members.devices.remove(fingerprint));
        self.save(&members)
    }

    /// Require a byte-exact, currently approved certificate.
    ///
    /// # Errors
    /// Returns policy read errors or a permission error for absent/revoked devices.
    pub fn require(&self, certificate: &[u8]) -> io::Result<PublicDevice> {
        let key = crate::identity::fingerprint(certificate);
        let members = self.load()?;
        let device = members.devices.get(&key).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "device is not approved for this space",
            )
        })?;
        if crate::identity::certificate_der(&device.certificate)?.as_ref() != certificate {
            return Err(invalid("device certificate pin mismatch"));
        }
        Ok(device.clone())
    }

    fn path(&self) -> PathBuf {
        self.root.join("multiplayer/members.json")
    }

    fn load(&self) -> io::Result<Members> {
        let path = self.path();
        let members: Members = match fs::read(&path) {
            Ok(bytes) => {
                if bytes.len() > 256 * 1024 {
                    return Err(invalid("membership metadata exceeds limit"));
                }
                serde_json::from_slice(&bytes)
                    .map_err(|_error| invalid("invalid membership metadata"))?
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Members {
                version: 1,
                space: self.space.clone(),
                devices: BTreeMap::new(),
            },
            Err(error) => return Err(error),
        };
        if members.version != 1
            || members.space != self.space
            || members.devices.len() > MAX_DEVICES
        {
            return Err(invalid("membership space or version mismatch"));
        }
        Ok(members)
    }

    fn save(&self, members: &Members) -> io::Result<()> {
        atomic_write(
            &self.path(),
            &serde_json::to_vec(members).map_err(io::Error::other)?,
        )
    }
}
