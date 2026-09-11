//! Keep wiremux-auth versions compatible with a Bline host lock.

#[test]
fn cargo_toml_aligns_keyring_reqwest_toml() {
    let manifest = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
    assert!(
        manifest.contains("toml = \"1.1\""),
        "toml must accept a 1.1.5 host lock, got:\n{manifest}"
    );
    assert!(
        !manifest.contains("toml = \"1.1.6\""),
        "1.1.6 floor rejects Bline's 1.1.5 lock"
    );
    assert!(
        manifest.contains("version = \"0.13"),
        "reqwest must be 0.13 so a 0.13 host does not pull 0.12, got:\n{manifest}"
    );
    assert!(
        manifest.contains("keyring = \"4."),
        "keyring must be 4.x so a 4.x host does not pull 3.x, got:\n{manifest}"
    );
}
