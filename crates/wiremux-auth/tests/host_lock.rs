//! Keep wiremux-auth versions compatible with a host lock.

#[test]
fn cargo_toml_aligns_keyring_reqwest_toml() {
    let manifest = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
    assert!(
        manifest.contains("toml = \"1.1\""),
        "toml must accept a 1.1.5 host lock, got:\n{manifest}"
    );
    assert!(
        !manifest.contains("toml = \"1.1.6\""),
        "1.1.6 floor rejects a 1.1.5 host lock"
    );
    assert!(
        manifest.contains("version = \"0.13"),
        "reqwest must be 0.13 so a 0.13 host does not pull 0.12, got:\n{manifest}"
    );
    assert!(
        manifest.contains("keyring = { version = \"4.2\""),
        "keyring must be 4.x so a 4.x host does not pull 3.x, got:\n{manifest}"
    );
    assert!(
        manifest.contains("default = [\"net\", \"pkce\", \"device\"]"),
        "net must be a default feature, got:\n{manifest}"
    );
    assert!(
        manifest.contains("\"aws_lc_rs\""),
        "jsonwebtoken must keep aws_lc_rs on net, got:\n{manifest}"
    );
}
