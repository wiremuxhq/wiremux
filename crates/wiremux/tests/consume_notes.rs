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
        attach.contains("wiremux-auth = \"0.2.1\"")
            && attach.contains("version = \"0.2.1\"")
            && attach.contains("default-features = false"),
        "published-host attach must list crates.io form, got {attach}"
    );
    assert!(
        notes.contains("tag = \"v0.1.0\""),
        "Bline/unpublished attach stays on git tag v0.1.0"
    );
    assert!(
        notes.contains("leftover-only")
            && notes.contains("Production still uses host TokenProviders")
            && notes.contains("production does not wrap it"),
        "notes must not claim the leftover pin already wraps TokenProvider"
    );
    assert!(
        !notes.contains("Wrap `wiremux_auth::TokenProvider` inside `bline_auth::TokenProvider`.\n   Done in Bline"),
        "wrap is still later; leftover-only pin is not consume"
    );
    assert!(
        notes.contains("default-features = false"),
        "maps pin stays maps-only"
    );
    assert!(
        !notes.contains("crates.io is not the attach path"),
        "crates.io is now an attach path for published hosts"
    );
    assert!(
        notes.contains("`--provider xai`")
            && notes.contains("`xai`")
            && notes.contains("`anthropic`")
            && notes.contains("`anthropic-oauth`")
            && notes.contains("`openai`")
            && notes.contains("`openrouter`")
            && notes.contains("`gemini`")
            && notes.contains("`lmstudio`")
            && notes.contains("`vllm`"),
        "consume notes must list catalog ids and the canact mapping"
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
            && notes.contains("thinking.budget_tokens")
            && notes.contains("reasoning.summary=auto")
            && notes.contains("thinkingLevel")
            && notes.contains("Chat Completions and Responses emit `store`"),
        "thinking / LossReport table must exist for adapters"
    );
    assert!(
        notes.contains("WireClient")
            && notes.contains("from_profile")
            && notes.contains("send")
            && notes.contains("stream")
            && notes.contains("list_models"),
        "consume notes must name WireClient and from_profile / send / stream / list_models"
    );
    assert!(
        notes.contains("token_for_profile"),
        "refresh-only hosts still use token_for_profile"
    );
    assert!(
        notes.contains("one text block `\".\"`")
            && notes.contains("messages_whitespace_only_assistant_becomes_dot"),
        "notes must lock Messages empty-text as '.' (issue #90)"
    );
}
