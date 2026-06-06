# CLAUDE.md — Nyx

> Read this first. It is the high-signal map of the repo. For depth, follow the
> links into `docs/` — don't duplicate that content here.

## What & why

Nyx is a fast, reliable, cross-platform **SFTP/FTP/FTPS** client written in pure
Rust, built the way Zed is built: GPU-rendered UI via **GPUI**, no web stack,
small focused crates, an in-house component library. Priorities are reliability,
simplicity and performance over feature-completeness. Full picture:
[`docs/overview.md`](docs/overview.md).

## Architecture in 5 lines

- **GPUI main thread** (own executor) renders all UI; **Tokio backend thread**
  owns connections + the transfer queue. They talk over **channels** (`Command`
  UI→service, `Event` service→UI).
- Crate map: `nyx` (app binary) · `nyx-ui` (in-house components + theme) ·
  `nyx-core` (shared types) · `nyx-service` (backend thread) · `nyx-protocol`
  (`RemoteClient` + SFTP) · `nyx-transfer` (queue) · `nyx-profile` (profiles) ·
  `nyx-keyring` (OS keychain).
- `nyx-core` holds shared types with no UI/runtime knowledge; the protocol/
  transfer/profile/keyring crates run on the Tokio thread; the UI observes
  events via `cx.spawn`.

## Commands

```sh
cargo run -p nyx                              # open the app window
cargo run -p nyx-ui --example gallery         # the component gallery ("storybook")
cargo test                                    # run tests
cargo clippy --workspace --all-targets -- -D warnings   # lint (warnings = errors)
cargo fmt --all                               # format
```

`just` shortcuts exist too: `just run`, `just gallery`, `just lint`, `just check`.

## Hard rules (do not break)

- **In-house UI only.** No `gpui-component`, no external widget crate. We build
  on raw GPUI. Owning the UI layer (→ **Flint**) is the whole point.
- **`nyx-ui` must never depend on any `nyx-*` crate**, and no domain types in its
  component signatures. This keeps the Flint extraction trivial. See
  [`docs/plans/plan-02-nyx-ui-flint.md`](docs/plans/plan-02-nyx-ui-flint.md) and
  [`crates/nyx-ui/CLAUDE.md`](crates/nyx-ui/CLAUDE.md).
- **Never log credentials.** Passwords live in the OS keychain (`nyx-keyring`)
  only — never in logs, never in profile files.
- **GPUI is pinned to a git rev** in the root `Cargo.toml`. Don't bump it
  casually — it freezes the API and the dependency tree.

## Conventions

- **Errors:** `thiserror` in libraries (`nyx_core::NyxError`), `anyhow` at the
  edges (the app binary).
- **Logging:** `tracing`.
- **Styling:** `nyx-ui`'s `StyledExt` + theme tokens (`cx.theme().bg_app`). **No
  raw hex colors in app code** — add a semantic token instead.
- **Components:** stateless → `RenderOnce`; stateful views → `Render` + `Entity`.
- **Crate metadata** is inherited from `[workspace.package]`; shared deps from
  `[workspace.dependencies]`.
- **Comments — minimal.** Comment only what the code can't say itself: a
  non-obvious *why*, an invariant (concurrency/safety), a real gotcha. Write them
  terse — a clause, not a paragraph. Do **not** add: echo doc comments that
  restate a name (`/// Set the selected index.` over `fn selected`), what-comments
  that narrate the next line, section dividers (`// --- foo ---`), commented-out
  code, or plan-doc provenance (`M3`, `plan M6 D4`). Self-documenting code over a
  comment, every time.

## Gotchas

- **First GPUI build is long** (compiles the whole GPUI dep tree — several
  minutes). Subsequent builds are fast.
- **macOS feature flags:** GPUI is split into `gpui` + `gpui_platform` (+
  `gpui_macros`). We enable `font-kit` and leave Linux-only `x11`/`wayland`
  **off**. `application()` comes from `gpui_platform`.
- **Metal needs FULL Xcode, not just the Command Line Tools.** `gpui_macos`
  compiles `.metal` shaders at build time with the `metal` tool, which ships only
  in `Xcode.app`. One-time setup (the CLT alone is *not* enough):
  ```sh
  sudo xcodebuild -license accept
  sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
  ```
  Also keep `cmake` installed (`brew install cmake`).
- **Tokio ↔ GPUI bridge:** the backend runs on its own thread with its own Tokio
  runtime; the UI never blocks on it — see `nyx-service::spawn`.

## Build-verification loop (for agents)

After a change: `cargo clippy --workspace --all-targets -- -D warnings`, then for
UI work eyeball it with `cargo run -p nyx-ui --example gallery` (or
`cargo run -p nyx`).

## Pointers

- [`docs/`](docs/) — canonical plans (single source of truth). Start at
  [`docs/README.md`](docs/README.md).
- [`design/`](design/) — visual reference (an exported prototype in HTML/CSS/
  React). **Reference only, not shipped code.**
