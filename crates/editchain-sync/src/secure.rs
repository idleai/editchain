//! Mutually authenticated TLS 1.3 over an opaque, bounded byte bridge.

use std::io::{self, Cursor, Read as _, Write as _};
use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::ServerName;
use rustls::{
    ClientConfig, ClientConnection, Connection, RootCertStore, ServerConfig, ServerConnection,
};

use crate::identity::{certificate_der, SERVER_NAME};
use crate::{
    encode_message, invalid, DeviceIdentity, FrameDecoder, Membership, Message, Progress,
    PublicDevice, Replica, Session, MAX_FRAME_BYTES,
};

/// Largest opaque input chunk accepted from the local transport bridge.
pub const MAX_BRIDGE_BYTES: usize = 64 * 1024;
/// Maximum opaque output from a single bridge turn.
pub const MAX_BRIDGE_OUTPUT: usize = 256 * 1024;
const ALPN: &[u8] = b"editchain-peer/1";

/// One approved peer's secure replication connection.
#[derive(Debug)]
pub struct SecurePeer {
    connection: Connection,
    membership: Membership,
    session: Session,
    decoder: FrameDecoder,
    authenticated: Option<PublicDevice>,
    expected: Option<String>,
    failed: bool,
}

impl SecurePeer {
    /// Open a TLS endpoint for a previously configured space and approved devices.
    /// `remote` selects client mode and an exact approved server certificate;
    /// `None` accepts a client from this workspace's existing device allowlist.
    ///
    /// # Errors
    /// Rejects absent approvals, invalid credentials, wrong spaces and TLS setup.
    pub fn open(
        root: &Path,
        space: &str,
        identity: &DeviceIdentity,
        remote: Option<&str>,
    ) -> io::Result<Self> {
        let membership = Membership::open(root, space)?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut roots = RootCertStore::empty();
        let (mut connection, expected) = if let Some(certificate) = remote {
            let der = certificate_der(certificate)?;
            let approved = membership.require(&der)?;
            roots.add(der).map_err(tls_error)?;
            let mut config = ClientConfig::builder_with_provider(provider)
                .with_protocol_versions(&[&rustls::version::TLS13])
                .map_err(tls_error)?
                .with_root_certificates(roots)
                .with_client_auth_cert(vec![identity.certificate.clone()], identity.key.clone_key())
                .map_err(tls_error)?;
            config.alpn_protocols = vec![ALPN.to_vec()];
            config.resumption = rustls::client::Resumption::disabled();
            let name = ServerName::try_from(SERVER_NAME)
                .map_err(|_error| invalid("invalid peer server name"))?;
            (
                Connection::Client(
                    ClientConnection::new(Arc::new(config), name).map_err(tls_error)?,
                ),
                Some(approved.fingerprint),
            )
        } else {
            for device in membership.devices()? {
                roots
                    .add(certificate_der(&device.certificate)?)
                    .map_err(tls_error)?;
            }
            let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
                Arc::new(roots),
                provider.clone(),
            )
            .build()
            .map_err(|_error| invalid("no valid approved client certificates"))?;
            let mut config = ServerConfig::builder_with_provider(provider)
                .with_protocol_versions(&[&rustls::version::TLS13])
                .map_err(tls_error)?
                .with_client_cert_verifier(verifier)
                .with_single_cert(vec![identity.certificate.clone()], identity.key.clone_key())
                .map_err(tls_error)?;
            config.alpn_protocols = vec![ALPN.to_vec()];
            config.send_tls13_tickets = 0;
            (
                Connection::Server(ServerConnection::new(Arc::new(config)).map_err(tls_error)?),
                None,
            )
        };
        connection.set_buffer_limit(Some(MAX_FRAME_BYTES.saturating_add(4096)));
        Ok(Self {
            connection,
            membership,
            session: Session::new(Replica::open(root, space, false)?),
            decoder: FrameDecoder::default(),
            authenticated: None,
            expected,
            failed: false,
        })
    }

    /// Authenticated supplying device, distinct from each record's original actor.
    #[must_use]
    pub fn device(&self) -> Option<&PublicDevice> {
        self.authenticated.as_ref()
    }

    /// Durable receipt counters and separately labelled in-flight work for the UI.
    #[must_use]
    pub fn progress(&self) -> &Progress {
        self.session.progress()
    }

    /// Consume opaque TLS input and return bounded opaque output. `tick` starts
    /// a new reconciliation round when idle, including missing-content repair.
    /// A failure permanently invalidates this connection.
    ///
    /// # Errors
    /// Rejects revoked/unapproved devices, TLS or peer-protocol failures, excessive
    /// input/output, and local storage failures. Never dispatches local service RPC.
    pub fn turn(&mut self, bytes: &[u8], tick: bool) -> io::Result<Vec<u8>> {
        if self.failed {
            return Err(invalid("peer connection is closed"));
        }
        let result = self.process(bytes, tick);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    /// Verify orderly TLS closure and complete peer framing at transport EOF.
    ///
    /// # Errors
    /// Rejects a truncated frame or a TLS stream lacking authenticated closure.
    pub fn finish(&mut self) -> io::Result<()> {
        let _: usize = self.connection.read_tls(&mut io::empty())?;
        let _state = self.connection.process_new_packets().map_err(tls_error)?;
        let mut byte = [0];
        if self.connection.reader().read(&mut byte)? != 0 {
            return Err(invalid("unconsumed peer data at EOF"));
        }
        self.decoder.finish()
    }

    /// Send authenticated TLS closure and make this endpoint unusable.
    ///
    /// # Errors
    /// Returns a bounded TLS output failure.
    pub fn close(&mut self) -> io::Result<Vec<u8>> {
        self.failed = true;
        self.connection.send_close_notify();
        let mut bytes = Vec::new();
        self.drain(&mut bytes)?;
        Ok(bytes)
    }

    fn process(&mut self, bytes: &[u8], tick: bool) -> io::Result<Vec<u8>> {
        if bytes.len() > MAX_BRIDGE_BYTES {
            return Err(invalid("opaque bridge chunk exceeds limit"));
        }
        if let Some(device) = &self.authenticated {
            let _approved = self
                .membership
                .require(&certificate_der(&device.certificate)?)?;
        }
        let mut output = Vec::new();
        let mut input = Cursor::new(bytes);
        let mut messages = 0usize;
        while input.position() < u64::try_from(bytes.len()).map_err(io::Error::other)? {
            let count = self.connection.read_tls(&mut input)?;
            if count == 0 {
                return Err(invalid("TLS input made no progress"));
            }
            let state = self.connection.process_new_packets().map_err(tls_error)?;
            if state.peer_has_closed() {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "peer closed TLS",
                ));
            }
            self.authenticate(&mut output)?;
            self.plaintext(&mut output, &mut messages)?;
            self.drain(&mut output)?;
        }
        if tick && self.authenticated.is_some() {
            for message in self.session.tick()? {
                self.send(&message, &mut output)?;
            }
        }
        self.drain(&mut output)?;
        Ok(output)
    }

    fn authenticate(&mut self, output: &mut Vec<u8>) -> io::Result<()> {
        if self.authenticated.is_some() || self.connection.is_handshaking() {
            return Ok(());
        }
        if self.connection.protocol_version() != Some(rustls::ProtocolVersion::TLSv1_3)
            || self.connection.alpn_protocol() != Some(ALPN)
        {
            return Err(invalid(
                "peer did not negotiate the required secure protocol",
            ));
        }
        let certificates = self
            .connection
            .peer_certificates()
            .ok_or_else(|| invalid("peer omitted its device certificate"))?;
        let [certificate] = certificates else {
            return Err(invalid("peer must present one exact device certificate"));
        };
        let device = self.membership.require(certificate)?;
        if self
            .expected
            .as_ref()
            .is_some_and(|expected| expected != &device.fingerprint)
        {
            return Err(invalid("unexpected peer certificate"));
        }
        self.authenticated = Some(device);
        self.send(&self.session.hello(), output)
    }

    fn plaintext(&mut self, output: &mut Vec<u8>, messages: &mut usize) -> io::Result<()> {
        let mut bytes = [0; 16 * 1024];
        loop {
            let count = match self.connection.reader().read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => count,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            };
            if self.authenticated.is_none() {
                return Err(invalid("application bytes before device authentication"));
            }
            for message in self.decoder.push(
                bytes
                    .get(..count)
                    .ok_or_else(|| invalid("invalid TLS read length"))?,
            )? {
                *messages = messages.saturating_add(1);
                if *messages > 512 {
                    return Err(invalid("too many peer messages in one bridge turn"));
                }
                for reply in self.session.receive(message)? {
                    self.send(&reply, output)?;
                }
            }
        }
        Ok(())
    }

    fn send(&mut self, message: &Message, output: &mut Vec<u8>) -> io::Result<()> {
        self.connection
            .writer()
            .write_all(&encode_message(message)?)?;
        self.drain(output)
    }

    fn drain(&mut self, output: &mut Vec<u8>) -> io::Result<()> {
        while self.connection.wants_write() {
            if self.connection.write_tls(output)? == 0 {
                return Err(invalid("TLS output made no progress"));
            }
            if output.len() > MAX_BRIDGE_OUTPUT {
                return Err(invalid("opaque bridge output exceeds limit"));
            }
        }
        Ok(())
    }
}

fn tls_error(_error: rustls::Error) -> io::Error {
    // No certificate, content bytes, or underlying library diagnostics cross IPC.
    crate::authentication_failed("peer TLS authentication or protocol failed")
}
