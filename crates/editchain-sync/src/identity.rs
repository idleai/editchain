//! Persistent device credentials. Only the public certificate crosses local IPC.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use editchain_store::durable::sync_parent_dir;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};

use crate::invalid;

pub(crate) const SERVER_NAME: &str = "editchain-peer.invalid";

/// Public device identity exchanged through a trusted invitation channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicDevice {
    /// Base64 DER certificate, without any private key material.
    pub certificate: String,
    /// BLAKE3 fingerprint of the exact certificate bytes.
    pub fingerprint: String,
}

impl PublicDevice {
    /// Parse and fingerprint a bounded, structurally valid certificate.
    ///
    /// # Errors
    /// Rejects oversized, invalid base64 or malformed certificates.
    pub fn parse(certificate: &str) -> io::Result<Self> {
        let der = certificate_der(certificate)?;
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(der.clone())
            .map_err(|_error| invalid("invalid device certificate"))?;
        Ok(Self {
            certificate: STANDARD.encode(&der),
            fingerprint: fingerprint(&der),
        })
    }
}

/// Long-lived local device key; debugging deliberately shows only its fingerprint.
pub struct DeviceIdentity {
    pub(crate) certificate: CertificateDer<'static>,
    pub(crate) key: PrivateKeyDer<'static>,
}

impl fmt::Debug for DeviceIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceIdentity")
            .field("fingerprint", &fingerprint(&self.certificate))
            .finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredIdentity {
    version: u16,
    certificate: Vec<u8>,
    secret: Vec<u8>,
}

impl DeviceIdentity {
    /// Load or atomically create credentials in application-private device storage.
    /// Serialize competing creators with a short filesystem lock. On Unix both
    /// the directory and credential file restrict access to their owner.
    ///
    /// # Errors
    /// Returns filesystem, key-generation, or malformed-credential errors. Existing
    /// corrupt identities fail visibly instead of silently changing device identity.
    pub fn load_or_create(directory: &Path) -> io::Result<Self> {
        fs::create_dir_all(directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        }
        sync_parent_dir(directory)?;
        let lock = private_options()
            .create(true)
            .truncate(false)
            .open(directory.join("identity.lock"))?;
        lock.lock()?;
        let path = directory.join("device.json");
        let stored = if path.exists() {
            let file = fs::File::open(&path)?;
            if file.metadata()?.len() > 16_384 {
                return Err(invalid("device identity exceeds limit"));
            }
            let mut bytes = Vec::new();
            let _: usize = file.take(16_385).read_to_end(&mut bytes)?;
            serde_json::from_slice::<StoredIdentity>(&bytes)
                .map_err(|_error| invalid("invalid stored device identity"))?
        } else {
            let generated = rcgen::generate_simple_self_signed(vec![SERVER_NAME.to_owned()])
                .map_err(|_error| invalid("device key generation failed"))?;
            let stored = StoredIdentity {
                version: 1,
                certificate: generated.cert.der().to_vec(),
                secret: generated.signing_key.serialize_der(),
            };
            let pending = directory.join("device.pending");
            let mut file = private_options()
                .create(true)
                .truncate(true)
                .open(&pending)?;
            file.write_all(&serde_json::to_vec(&stored).map_err(io::Error::other)?)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&pending, &path)?;
            sync_parent_dir(&path)?;
            stored
        };
        if stored.version != 1 || stored.certificate.len() > 4096 || stored.secret.len() > 4096 {
            return Err(invalid("invalid stored device identity"));
        }
        let identity = Self {
            certificate: CertificateDer::from(stored.certificate),
            key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(stored.secret)),
        };
        let _public = PublicDevice::parse(&STANDARD.encode(&identity.certificate))?;
        lock.unlock()?;
        Ok(identity)
    }

    /// Certificate and fingerprint suitable for an invitation or status display.
    #[must_use]
    pub fn public(&self) -> PublicDevice {
        PublicDevice {
            certificate: STANDARD.encode(&self.certificate),
            fingerprint: fingerprint(&self.certificate),
        }
    }
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    let _options = options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let _options = options.mode(0o600);
    }
    options
}

pub(crate) fn certificate_der(certificate: &str) -> io::Result<CertificateDer<'static>> {
    if certificate.len() > 8192 {
        return Err(invalid("device certificate exceeds limit"));
    }
    let bytes = STANDARD
        .decode(certificate)
        .map_err(|_error| invalid("invalid certificate encoding"))?;
    if bytes.is_empty() || bytes.len() > 4096 {
        return Err(invalid("device certificate exceeds limit"));
    }
    Ok(CertificateDer::from(bytes))
}

pub(crate) fn fingerprint(certificate: &[u8]) -> String {
    blake3::hash(certificate).to_hex().to_string()
}
