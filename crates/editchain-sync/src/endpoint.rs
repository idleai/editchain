//! Explicit peer routing over a caller-owned authenticated byte transport.

use std::io;

use crate::{
    encode_message, invalid, FrameDecoder, Message, Progress, ReplicationStorage, Session,
};

/// Caller-owned delivery of ordered bytes to an already authorized peer.
///
/// Successful sends mean transport acceptance, never durable remote storage.
/// Transports must preserve byte order and report interrupted streams as errors;
/// retries use a fresh connection and resume from durable records. Async hosts
/// can enqueue frames here or drive [`Session`] directly from their event loop.
pub trait Transport {
    /// Send one complete encoded frame to the supplied opaque peer identifier.
    ///
    /// # Errors
    /// Returns transport errors, including uncertain or partial delivery.
    fn send(&mut self, peer: &str, frame: &[u8]) -> io::Result<()>;
}

/// One caller-selected peer, storage policy and transport connection.
///
/// The peer label routes messages; it is not authentication. Construct this
/// only after the host has authenticated and authorized that peer for the
/// selected chain. The host controls scheduling, reconnects and discovery.
#[derive(Debug)]
pub struct PeerConnection<R, T> {
    peer: String,
    session: Session<R>,
    transport: T,
    decoder: FrameDecoder,
    started: bool,
    failed: bool,
}

impl<R: ReplicationStorage, T: Transport> PeerConnection<R, T> {
    /// Bind an authorized peer without reading storage or sending any bytes.
    pub fn new(peer: impl Into<String>, storage: R, transport: T) -> Self {
        Self {
            peer: peer.into(),
            session: Session::new(storage),
            transport,
            decoder: FrameDecoder::default(),
            started: false,
            failed: false,
        }
    }

    /// Send protocol negotiation once, after host authentication is complete.
    ///
    /// # Errors
    /// Returns a transport error or rejects an already started/failed connection.
    pub fn start(&mut self) -> io::Result<()> {
        self.ensure_open()?;
        if self.started {
            return Err(invalid("replication connection already started"));
        }
        self.started = true;
        let result = self.send(vec![self.session.hello()]);
        self.failed = result.is_err();
        result
    }

    /// Process a bounded transport fragment from this connection's peer.
    ///
    /// # Errors
    /// Wrong-peer input, protocol, storage and transport errors invalidate the
    /// connection. Recover adapters and start a fresh session to retry.
    pub fn receive(&mut self, peer: &str, bytes: &[u8]) -> io::Result<()> {
        self.ensure_started()?;
        let result = self.process(peer, bytes);
        self.failed = result.is_err();
        result
    }

    fn process(&mut self, peer: &str, bytes: &[u8]) -> io::Result<()> {
        if peer != self.peer {
            return Err(invalid("input belongs to a different replication peer"));
        }
        for message in self.decoder.push(bytes)? {
            let replies = self.session.receive(message)?;
            self.send(replies)?;
        }
        Ok(())
    }

    /// Schedule another catch-up round, including retries of missing blobs.
    ///
    /// # Errors
    /// Returns scope, storage or transport errors and invalidates the connection.
    pub fn tick(&mut self) -> io::Result<()> {
        self.ensure_started()?;
        let result = self.session.tick().and_then(|messages| self.send(messages));
        self.failed = result.is_err();
        result
    }

    /// Report transport EOF and reject a truncated frame. A reconnect always
    /// uses a new connection, even when EOF was between complete frames.
    ///
    /// # Errors
    /// Returns an error for a failed connection or incomplete frame.
    pub fn finish(&mut self) -> io::Result<()> {
        self.ensure_started()?;
        self.failed = true;
        self.decoder.finish()
    }

    /// Separate durable receipts and remote acknowledgements from downloads.
    #[must_use]
    pub fn progress(&self) -> &Progress {
        self.session.progress()
    }

    /// Recover storage and transport. Discard the old stream before reconnecting.
    #[must_use]
    pub fn into_parts(self) -> (R, T) {
        (self.session.into_storage(), self.transport)
    }

    fn send(&mut self, messages: Vec<Message>) -> io::Result<()> {
        for message in messages {
            self.transport
                .send(&self.peer, &encode_message(&message)?)?;
        }
        Ok(())
    }

    fn ensure_open(&self) -> io::Result<()> {
        if self.failed {
            return Err(invalid("failed replication connection; reconnect"));
        }
        Ok(())
    }

    fn ensure_started(&self) -> io::Result<()> {
        self.ensure_open()?;
        if !self.started {
            return Err(invalid("replication connection has not started"));
        }
        Ok(())
    }
}
