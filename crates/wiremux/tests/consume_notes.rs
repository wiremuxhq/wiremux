//! Consume notes stay current after the first Bline pin.

#[test]
fn consume_notes_do_not_claim_consume_is_unstarted() {
    let version = env!("CARGO_PKG_VERSION");
    let notes = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/BLINE-CONSUME.md"
    ));
    let release_please = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../release-please-config.json"
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
        attach.contains(&format!("wiremux-auth = \"{version}\""))
            && attach.contains(&format!("version = \"{version}\""))
            && attach.contains("default-features = false")
            && attach.contains("x-release-please-version"),
        "published-host attach must list crates.io {version} with a release-please marker, got {attach}"
    );
    assert!(
        !notes.contains("tag = \"v0.1.0\""),
        "Bline attach is crates.io {version}, not git tag v0.1.0"
    );
    assert!(
        !notes.contains("leftover-only")
            && !notes.contains("Production still uses host TokenProviders")
            && !notes.contains("production does not wrap it"),
        "notes must not still describe the leftover-only pin as current"
    );
    assert!(
        notes.contains("Wrap `wiremux_auth::TokenProvider` inside `bline_auth::TokenProvider`.\n   Done in Bline #3992")
            && notes.contains("Bline production now wraps")
            && notes.contains("21b56488"),
        "notes must record the live Bline wrap, not leftover-only"
    );
    assert!(
        notes.contains(&format!("crates.io is `{version}`"))
            && notes.contains(&format!("v{version}"))
            && notes.contains("v0.6.0")
            && notes.contains("v0.5.0")
            && notes.contains("3996")
            && notes.contains("4010")
            && notes.contains("v0.4.0")
            && notes.contains("not in crates.io `0.4.0`")
            && notes.contains("not in crates.io `0.5.0`")
            && notes.contains("#181"),
        "notes must name crates.io {version} and keep the Bline 3996/4010/0.4.0/0.5.0/0.6.0 history"
    );
    for needle in [
        format!("crates.io is `{version}`"),
        format!("Current tag is `v{version}`"),
        format!("published tag `v{version}`"),
        format!("pin crates.io `{version}`"),
        format!("Matching tag is `v{version}`"),
        format!("stay on `{version}`"),
    ] {
        let line = notes
            .lines()
            .find(|line| line.contains(&needle))
            .unwrap_or_else(|| panic!("missing current-pin line: {needle}"));
        assert!(
            line.contains("x-release-please-version"),
            "current-pin line must carry a release-please marker: {line}"
        );
        assert_eq!(
            line.matches(version).count(),
            1,
            "generic extra-files rewrites only the first semver on a marked line: {line}"
        );
    }
    let history_pin = notes
        .lines()
        .find(|line| line.contains("The 0.6.0 pin was tag"))
        .expect("0.6.0 leftover-prove history");
    assert!(
        !history_pin.contains("x-release-please-version"),
        "history pins must not be extra-files targets: {history_pin}"
    );
    assert!(
        release_please.contains("docs/BLINE-CONSUME.md")
            && release_please.contains("\"type\": \"generic\""),
        "release-please extra-files must bump the consume pin as generic"
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
            && notes.contains("`--provider grok-build`")
            && notes.contains("`--provider grok-build-messages`")
            && notes.contains("`xai`")
            && notes.contains("`anthropic`")
            && notes.contains("`anthropic-oauth`")
            && notes.contains("`openai`")
            && notes.contains("`openrouter`")
            && notes.contains("`gemini`")
            && notes.contains("`lmstudio`")
            && notes.contains("`vllm`")
            && notes.contains("`xai-oauth`")
            && notes.contains("`xai-grok-build`")
            && notes.contains("`xai-grok-build-messages`")
            && notes.contains("cli-chat-proxy.grok.com")
            && notes.contains("x-grok-client-version")
            && notes.contains("x-grok-model-override")
            && notes.contains("shipped_profile_ids"),
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
        notes.contains("token_for_profile_cached")
            && notes.contains("token_for_profile")
            && !notes.contains("should call `token_for_profile` on a catalog id")
            && !notes.contains("Refresh-only hosts still call `token_for_profile`"),
        "refresh-only hosts must be told to call token_for_profile_cached"
    );
    assert!(
        notes.contains("URL-first host")
            && notes.contains("cli-chat-proxy.grok.com")
            && notes.contains("handmade"),
        "URL-first hosts must still get the Grok Build header pack"
    );
    assert!(
        notes.contains("Continue.")
            && notes.contains("messages_encode_appends_continue_on_assistant_last"),
        "notes must lock Messages assistant-last Continue."
    );
    assert!(
        notes.contains("one text block `\".\"`")
            && notes.contains("messages_whitespace_only_assistant_becomes_dot"),
        "notes must lock Messages empty-text as '.' (issue #90)"
    );
}
