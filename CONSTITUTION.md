# Constitution

Immutable until a human-labeled PR amends this file.

1. Independent org `wiremuxhq/wiremux`. Not `blineai/`.
2. License is MIT OR Apache-2.0.
3. Two crates: `wiremux` and `wiremux-auth`. The CLI is a binary in `wiremux`, not a third crate.
4. Anthropic OAuth ships as a data profile. The same schema is the pasteable backdoor.
5. Do not ship a public Claude-Pro-in-Codex, Cline, or OpenCode preset.
6. Profiles are data only. Refuse functions, `!command`, and URL-as-script.
7. Do not fold this crate into canact. The probe suite stays in canact.
8. Public project. README, GitHub About, topics, and crate descriptions describe the product. Do not restore stealth-public stubs (`Not ready.`, `Reserved.`).
