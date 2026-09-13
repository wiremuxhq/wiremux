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
    let attach = notes
        .split("```toml")
        .nth(1)
        .and_then(|rest| rest.split("```").next())
        .expect("suggested attach toml fence");
    assert!(
        attach.contains("tag = \"v0.1.0\""),
        "suggested attach toml must pin tag v0.1.0, got {attach}"
    );
    assert!(
        notes.contains("default-features = false"),
        "maps pin stays maps-only"
    );
    assert!(
        notes.contains("LLM layer is wiremux"),
        "heading must say the LLM layer is wiremux"
    );
    assert!(
        !notes.contains("DESIGN non-goal for v1")
            && !notes.contains("Same crate later")
            && !notes.contains("IrCache` is only `enabled` + `retention`"),
        "notes must not leave Gcp/Azure/AwsSts or the cache floor in Bline"
    );
    assert!(
        notes.contains("`GcpTokenProvider`, `AzureTokenProvider`, `AwsStsTokenProvider`")
            && notes.contains("`wiremux-auth`"),
        "cloud TokenProviders belong in wiremux-auth"
    );
    assert!(
        notes.contains("load_profile_for_wire"),
        "resolve-by-wire belongs here"
    );
    assert!(
        notes.contains("min_cacheable_tokens"),
        "cache floor belongs on IrCache"
    );
    assert!(
        notes.contains("Signed `IrPart::Thinking`")
            && notes.contains("part.thinking")
            && notes.contains("sampling.max_reasoning_tokens")
            && notes.contains("thinking.budget_tokens"),
        "thinking / LossReport table must exist for adapters"
    );
}
