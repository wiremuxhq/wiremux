# Agents

> **Human contributors:** This file is for AI coding assistants.
> You can safely ignore it. See README.md and CONTRIBUTING.md instead.

Rust workspace. MSRV 1.95. Edition 2024.

```bash
make check
```

`make check` needs rustc 1.95 (see `rust-toolchain.toml`), rustfmt,
clippy, and `cargo-deny`. Sign commits with `git commit -s` (DCO).

Two crates: `wiremux-auth` (profile AST + TokenProvider) and `wiremux`
(dialect maps + optional CLI binary). Do not add a third published crate.

Do not dest-parent-copy from Bline. Land failing corpus tests before product modules.

Read `CONSTITUTION.md` before changing license, org, crate graph, or OAuth presets.

The PR plan is not in this tree. Read `~/.grok/skills/wiremux-contrib/SKILL.md`
and pass the design path it prints to `/execute-plan`. Do not search `/tmp`
or pick a "latest" design. PR 1 is this scaffold. Start at PR 2.
