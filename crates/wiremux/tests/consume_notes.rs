//! Consume notes stay current after the first Bline pin.

#[test]
fn consume_notes_do_not_claim_consume_is_unstarted() {
    let notes = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/BLINE-CONSUME.md"
    ));
    assert!(
        !notes.contains("Bline consume is not started"),
        "first leftover prove already landed in Bline"
    );
    assert!(
        !notes.contains("has no\n`wiremux` dep") && !notes.contains("has no `wiremux` dep"),
        "Bline Cargo.toml now pins wiremux"
    );
    assert!(
        notes.contains("4d939fe87cdf0a3ce9eba599810520eeaf55b6b9"),
        "attach examples must name the first leftover-prove SHA"
    );
    assert!(
        notes.contains("default-features = false"),
        "maps pin stays maps-only"
    );
}
