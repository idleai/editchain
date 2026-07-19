use editchain_import::ids::{derive_node_id, derive_session_id, hash_raw};

#[test]
fn deterministic_node_id() {
    let a = derive_node_id("/workspace/editchain");
    let b = derive_node_id("/workspace/editchain");
    assert_eq!(a, b);
}

#[test]
fn different_inputs_different_ids() {
    let a = derive_node_id("/workspace/a");
    let b = derive_node_id("/workspace/b");
    assert_ne!(a, b);
}

#[test]
fn deterministic_session_id() {
    let uuid = "3f7db8b8-73a7-4cea-be8d-3d2d54fedd2c";
    let a = derive_session_id(uuid);
    let b = derive_session_id(uuid);
    assert_eq!(a, b);
}

#[test]
fn hash_raw_is_blake3() {
    let data = b"hello world";
    let h = hash_raw(data);
    let expected = blake3::hash(data);
    assert_eq!(h, *expected.as_bytes());
}