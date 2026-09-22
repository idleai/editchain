//! Real TLS admission, pinning, revocation and byte-isolation regressions.

use std::collections::VecDeque;

use super::*;
use crate::{DeviceIdentity, Membership, SecurePeer};

fn configure(root: &Path, space: &str, remote: &DeviceIdentity) -> io::Result<()> {
    let _replica = Replica::open(root, space, true)?;
    let _device = Membership::open(root, space)?.approve(&remote.public().certificate)?;
    Ok(())
}

fn connect(a: &mut SecurePeer, b: &mut SecurePeer) -> io::Result<()> {
    let mut queue: VecDeque<_> = [(false, a.turn(&[], false)?), (true, b.turn(&[], false)?)].into();
    for _ in 0..20_000 {
        let Some((to_a, bytes)) = queue.pop_front() else {
            return Ok(());
        };
        let peer = if to_a { &mut *a } else { &mut *b };
        for bytes in bytes.chunks(4093) {
            let output = peer.turn(bytes, false)?;
            if !output.is_empty() {
                queue.push_back((!to_a, output));
            }
        }
    }
    Err(io::Error::other("secure peers did not settle"))
}

#[test]
fn tls_peers_preserve_identity_across_restart_and_recheck_live_revocation() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let ar = dir.path().join("a");
    let br = dir.path().join("b");
    let ai = DeviceIdentity::load_or_create(&dir.path().join("ad"))?;
    let bi = DeviceIdentity::load_or_create(&dir.path().join("bd"))?;
    check_eq!(
        DeviceIdentity::load_or_create(&dir.path().join("ad"))?.public(),
        ai.public(),
        "device key persists across worker lifetimes"
    );
    configure(&ar, "space-1", &bi)?;
    configure(&br, "space-1", &ai)?;
    seed(&ar, &[record(1, b"never plaintext on bridge")?])?;
    let mut a = SecurePeer::open(&ar, "space-1", &ai, None)?;
    let mut b = SecurePeer::open(&br, "space-1", &bi, Some(&ai.public().certificate))?;
    check!(
        a.device().is_none() && b.device().is_none(),
        "device unverified before TLS"
    );
    connect(&mut a, &mut b)?;
    check_eq!(b.progress().records, 1, "actual TLS-carried replication");
    check_eq!(
        a.device(),
        Some(&bi.public()),
        "server pins approved client certificate"
    );
    check_eq!(
        b.device(),
        Some(&ai.public()),
        "client pins invited server certificate"
    );
    Membership::open(&ar, "space-1")?.revoke(&bi.public().fingerprint)?;
    check!(
        a.turn(&[], true).is_err(),
        "revocation closes already-authenticated session"
    );
    check!(a.turn(&[], false).is_err(), "closed session cannot resume");
    check!(
        SecurePeer::open(&ar, "space-1", &ai, None).is_err(),
        "no remaining devices can connect"
    );
    check_eq!(
        CanonicalChain::read(&br)?.stats().accepted,
        1,
        "revocation cannot recall durable copies"
    );
    Ok(())
}

#[test]
fn wrong_client_server_and_space_are_rejected_before_inventory() -> io::Result<()> {
    for mode in 0..3 {
        let dir = tempfile::tempdir()?;
        let ar = dir.path().join("a");
        let br = dir.path().join("b");
        let ai = DeviceIdentity::load_or_create(&dir.path().join("ad"))?;
        let bi = DeviceIdentity::load_or_create(&dir.path().join("bd"))?;
        let stranger = DeviceIdentity::load_or_create(&dir.path().join("stranger"))?;
        configure(&ar, "space-1", &bi)?;
        let remote = if mode == 1 { &stranger } else { &ai };
        let space = if mode == 2 { "space-2" } else { "space-1" };
        configure(&br, space, remote)?;
        seed(&ar, &[record(1, b"private until admitted")?])?;
        let mut a = SecurePeer::open(&ar, "space-1", &ai, None)?;
        let mut b = SecurePeer::open(
            &br,
            space,
            if mode == 0 { &stranger } else { &bi },
            Some(&remote.public().certificate),
        )?;
        check!(
            connect(&mut a, &mut b).is_err(),
            "certificate or space mismatch rejects connection"
        );
        check!(
            !a.progress().accepted && !b.progress().accepted,
            "no inventory before full admission"
        );
        check_eq!(
            CanonicalChain::read(&br)?.stats().records,
            0,
            "no evidence disclosed by rejected peer"
        );
    }
    Ok(())
}

#[test]
fn raw_service_requests_and_tampered_tls_are_not_peer_protocol() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let ar = dir.path().join("a");
    let br = dir.path().join("b");
    let ai = DeviceIdentity::load_or_create(&dir.path().join("ad"))?;
    let bi = DeviceIdentity::load_or_create(&dir.path().join("bd"))?;
    configure(&ar, "space-1", &bi)?;
    configure(&br, "space-1", &ai)?;
    let mut a = SecurePeer::open(&ar, "space-1", &ai, None)?;
    check!(
        a.turn(
            br#"{"id":1,"body":{"Open":{"workspace_path":"/","chain_dir":".editchain"}}}"#,
            false
        )
        .is_err(),
        "raw general RPC fails as TLS input"
    );
    let mut a = SecurePeer::open(&ar, "space-1", &ai, None)?;
    let mut b = SecurePeer::open(&br, "space-1", &bi, Some(&ai.public().certificate))?;
    connect(&mut a, &mut b)?;
    let mut bytes = b.turn(&[], true)?;
    let last = bytes
        .last_mut()
        .ok_or_else(|| io::Error::other("missing encrypted round"))?;
    *last ^= 1;
    check!(
        a.turn(&bytes, false).is_err(),
        "modified authenticated ciphertext is rejected"
    );
    Ok(())
}

#[test]
fn corrupt_device_identity_is_not_replaced_and_private_file_permissions_hold() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let device = dir.path().join("device");
    let _identity = DeviceIdentity::load_or_create(&device)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        check_eq!(
            std::fs::metadata(device.join("device.json"))?
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "credential file owner-only"
        );
        check_eq!(
            std::fs::metadata(&device)?.permissions().mode() & 0o777,
            0o700,
            "device directory owner-only"
        );
    }
    std::fs::write(device.join("device.json"), b"corrupt")?;
    check!(
        DeviceIdentity::load_or_create(&device).is_err(),
        "corruption cannot silently rotate identity"
    );
    check_eq!(
        std::fs::read(device.join("device.json"))?,
        b"corrupt",
        "failed load preserves original bytes"
    );
    Ok(())
}

#[test]
fn control_framing_rejects_oversized_and_truncated_input_and_unknown_commands() -> io::Result<()> {
    let oversized = u32::try_from(crate::MAX_CONTROL_BYTES.saturating_add(1))
        .map_err(io::Error::other)?
        .to_le_bytes();
    check!(
        crate::run_worker(&mut oversized.as_slice(), &mut Vec::new()).is_err(),
        "size checked before allocating input"
    );
    check!(
        crate::run_worker(&mut [1, 0].as_slice(), &mut Vec::new()).is_err(),
        "partial local control is an error"
    );
    let bytes = br#"{"type":"Open","workspace_path":"/"}"#;
    let mut framed = u32::try_from(bytes.len())
        .map_err(io::Error::other)?
        .to_le_bytes()
        .to_vec();
    framed.extend_from_slice(bytes);
    let mut output = Vec::new();
    crate::run_worker(&mut framed.as_slice(), &mut output)?;
    let result: serde_json::Value = serde_json::from_slice(
        output
            .get(4..)
            .ok_or_else(|| io::Error::other("missing worker frame"))?,
    )
    .map_err(io::Error::other)?;
    check_eq!(
        result.get("ok"),
        Some(&serde_json::Value::Bool(false)),
        "local history RPC not a peer command"
    );
    Ok(())
}
