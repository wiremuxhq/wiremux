# Agents

Rust workspace. MSRV 1.95. Edition 2024.

```bash
make check
```

Two crates: `wiremux-auth` (profile AST + TokenProvider) and `wiremux`
(dialect maps + optional CLI binary). Do not add a third published crate.

Do not dest-parent-copy from Bline. Land failing corpus tests before product modules.

Read `CONSTITUTION.md` before changing license, org, crate graph, or OAuth presets.
