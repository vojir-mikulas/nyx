# Contributing to Nyx

Thanks for your interest! Nyx is in early development; contributions and feedback
are welcome.

## Ground rules

- **Pure Rust, in-house UI.** The UI is built on raw [GPUI](https://github.com/zed-industries/zed).
  We do **not** use `gpui-component` or any external widget crate — owning the UI
  layer (`nyx-ui` → future **Flint**) is a core goal.
- **`nyx-ui` must never depend on any `nyx-*` crate.** It is a generic component
  library that happens to live here today. See
  [`docs/plans/plan-02-nyx-ui-flint.md`](docs/plans/plan-02-nyx-ui-flint.md).
- **Never log credentials.** Passwords live in the OS keychain only — never in
  logs, never in profile files.
- **GPUI is pinned to a git revision.** Don't bump it casually.

## Conventions

- Error handling: `thiserror` in libraries, `anyhow` at edges (the app/binary).
- Logging via `tracing`.
- Styling via `nyx-ui`'s `StyledExt` + theme tokens — **no raw hex** in app code.
- Stateless components use `RenderOnce`; stateful views use `Render` + `Entity`.

## Before you push

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

CI runs the same checks. Keep them green.

## Project docs

The canonical plans live in [`docs/`](docs/). [`CLAUDE.md`](CLAUDE.md) is the
high-signal entry point (for humans and AI agents alike) and links out to the
plans rather than duplicating them.
