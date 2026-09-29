# Constitution amendments

Decision log for [`CONSTITUTION.md`](../CONSTITUTION.md). A proposal that changes rules 1-8 needs a pull request labeled `constitution` by a person. Record a rejection here before closing the proposal issue. Reopen a rejection only when a new issue brings a new fact.

## 2026-09-29 product review

Reviewed at `8eae05a` after a read-only pass over the proxy, maps, and docs. Proposal: [#353](https://github.com/wiremuxhq/wiremux/issues/353). This entry adds the Amending section. Rules 1-8 are unchanged.

### Rejected because a rule already covers it

| Proposal | Rule | Why it stays |
| --- | --- | --- |
| A third published crate for the CLI | 3 | `wiremux map` fits in the existing `wiremux` binary. |
| `apiKeyHelper`, a profile function, or a script URL for dynamic keys | 6 | `$VAR` substitution and `TokenProvider` already cover dynamic keys. A command or URL in a profile is the boundary the rule exists to keep. |
| Fill a public client id in `openai-codex-oauth` or `xai-oauth` so `auth login` succeeds | 5 | That is the official-app impersonation path. Login stays exit 2 until a real Wiremux client id exists. Do not invent one. |
| Move the live vendor probe suite into this repo | 7 | Goldens and fixtures stay here. The probe suite stays in canact. |

### Rejected, not a constitution rule

| Proposal | Why it stays out |
| --- | --- |
| Router, failover, or a model table in the proxy | [`docs/CONSUME.md`](CONSUME.md) keeps the agent loop, router, and failover in the host. One `--model` overwrite is a separate feature. It is not a routing table. |
| Non-loopback bind, or proxy auth for LAN use | The listener stays on `127.0.0.1`. Host, Origin, and `sec-fetch-site` checks are the local threat model. |
| A unique response id per stream | Fixed dest ids stay: `chatcmpl-wiremux`, `msg_wiremux`, `resp_wiremux`, `gemini-wiremux`. Frames that omit the id repeat that fixed id. Reopen only if a host stores responses by id and the fixed id collides. |
