# Constitution

Rules 1-8 stay in force until a pull request amends them.

A change to those rules, or to the Amending section, lands only through a pull request that a person labels `constitution`. Automation does not apply that label and does not merge the pull request. Auto-merge already skips a diff that contains this file. Adding the label is the human yes.

## Amending

The pull request must:

1. Name the rule number it changes.
2. Quote the current text and the replacement.
3. Say which real improvement the current rule blocks, with a file or issue link.
4. Say which invariants stay: license, two crates, profiles are data only, no public Claude-Pro-in-Codex / Cline / OpenCode preset, the probe suite stays in canact, and the project stays public.
5. Link a proposal issue. Rejected proposals are recorded in [`docs/constitution-amendments.md`](docs/constitution-amendments.md) before the issue is closed.

This section records the procedure. Rules 1-8 below stay as written until a labeled pull request replaces one of them. The decision log is the place a later review checks before proposing the same change again.

## Rules

1. Independent org `wiremuxhq/wiremux`. Not `blineai/`.
2. License is MIT OR Apache-2.0.
3. Two crates: `wiremux` and `wiremux-auth`. The CLI is a binary in `wiremux`, not a third crate.
4. Anthropic OAuth ships as a data profile. The same schema is the pasteable backdoor.
5. Do not ship a public Claude-Pro-in-Codex, Cline, or OpenCode preset.
6. Profiles are data only. Refuse functions, `!command`, and URL-as-script.
7. Do not fold this crate into canact. The probe suite stays in canact.
8. Public project. README, GitHub About, topics, and crate descriptions describe the product. Do not restore stealth-public stubs (`Not ready.`, `Reserved.`).
