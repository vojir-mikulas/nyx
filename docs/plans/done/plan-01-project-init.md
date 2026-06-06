# Plan 01 — Project Initialization

Goal: a building Cargo workspace with an empty GPUI window opening on macOS, and
the crate skeleton in place. No app features yet — just a green-field that
compiles and runs.

## Prerequisites (verified 2026-06-06)

| Tool | Status | Notes |
|---|---|---|
| Rust | ✅ 1.96.0 stable | Zed pins 1.95; newer stable is fine |
| Xcode CLT | ✅ `/Library/Developer/CommandLineTools` | needed for Metal |
| git | ✅ 2.50 | |
| Homebrew | ✅ 5.1 | |
| cmake | ✅ installed via brew | required by some GPUI native deps |

## Step 1 — Repo & licensing

- [ ] `git init` in project root
- [ ] `LICENSE` — Apache-2.0 (owner: vojir-mikulas, 2026)
- [ ] `.gitignore` — `/target`, `Cargo.lock` kept (binary/app → commit lock)
- [ ] `README.md` — short pitch + build instructions
- [ ] `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md` (light)
- [ ] Keep `design/` and `docs/` in the tree

## Step 2 — Workspace skeleton

- [ ] Root `Cargo.toml` as a `[workspace]` with `resolver = "2"`
- [ ] `rust-toolchain.toml` — channel `1.96.0` (or track Zed's), components
      `rustfmt, clippy, rust-analyzer, rust-src`
- [ ] `[workspace.dependencies]` centralizing shared deps (gpui, tokio, serde,
      anyhow/thiserror, tracing)
- [ ] Create empty crates (so the graph resolves):
  - `crates/nyx` (bin)
  - `crates/nyx-ui` (lib) — built per [plan-02](./plan-02-nyx-ui-flint.md)
  - `crates/nyx-core` (lib)
  - `crates/nyx-service` (lib)
  - `crates/nyx-protocol` (lib)
  - `crates/nyx-transfer` (lib)
  - `crates/nyx-profile` (lib)
  - `crates/nyx-keyring` (lib)

## Step 3 — Pin GPUI

GPUI is a git dependency (not a stable crates.io release for app use). It is
split into `gpui` + `gpui_platform` + `gpui_macros`.

- [ ] Pin a **specific Zed commit** for reproducible builds (candidate from
      2026-06-06: `137e677a0561eb7284cfad1bedccea70155e2473`)
- [ ] Declare in `[workspace.dependencies]`:
  ```toml
  gpui = { git = "https://github.com/zed-industries/zed", rev = "137e677a…" }
  ```
- [ ] On macOS the Metal backend + `font-kit` come via defaults; **do not**
      enable `x11`/`wayland` features (Linux-only)
- [ ] First build will be **long** (compiles the whole GPUI dep tree) — expect
      several minutes, watch for missing native deps
- [ ] If the dependency shape fights us, consult GPUI's own
      `crates/gpui/examples/*` and `crates/gpui/Cargo.toml` for the correct
      declaration and macOS feature set (we depend on `gpui` directly — **no**
      `gpui-component` or any external widget crate)

## Step 4 — "Hello window"

- [ ] In `crates/nyx`, minimal GPUI app: `application().run()`, `open_window`,
      a root view implementing `Render` that draws a themed `div`
- [ ] `cargo run -p nyx` opens a 1000×680 window with the One Dark background
- [ ] Pull in `nyx-ui` theme as soon as plan-02 step 1 lands (so the window uses
      `cx.theme().bg_app`, not a hardcoded hex)

## Step 5 — Crate contracts (stubs, no logic)

Define the shared types and traits so crates compile against each other:

- [ ] `nyx-core`: `NyxError` (thiserror), `RemoteEntry { name, size, kind,
      modified, perms, is_dir }`, `Protocol` enum, `TransferId`, `Transfer`,
      `TransferStatus`
- [ ] `nyx-protocol`: the `RemoteClient` async trait (from the spec) + an empty
      `SftpClient` struct that returns `unimplemented!()`
- [ ] `nyx-service`: the backend-thread skeleton — a `spawn()` that starts a
      Tokio runtime on its own thread, plus `Command`/`Event` enums and the
      channel pair (no real work yet)
- [ ] `nyx-profile`, `nyx-keyring`: trait + struct stubs

## Step 6 — Dev ergonomics & CI

- [ ] `cargo fmt` + `clippy` clean
- [ ] GitHub Actions: `fmt --check`, `clippy -D warnings`, `cargo test`,
      `cargo build` (macOS runner first; add Linux later)
- [ ] `justfile` or `cargo` aliases: `run`, `gallery`, `lint`

## Step 7 — AI-assisted development setup

Make the repo legible and safe for AI coding agents (Claude Code etc.) from day
one. The goal: an agent can read one file and know the architecture, the rules,
and the exact commands — without rediscovering them each session.

### Root `CLAUDE.md`

The primary context file, kept short and high-signal (link out to `docs/` for
depth rather than duplicating). Contents:

- [ ] **One-paragraph what/why** + link to [`../overview.md`](../overview.md)
- [ ] **Architecture in 5 lines**: GPUI main thread ↔ Tokio backend thread via
      channels; crate map (`nyx`, `nyx-ui`, `nyx-core`, `nyx-service`,
      `nyx-protocol`, `nyx-transfer`, `nyx-profile`, `nyx-keyring`)
- [ ] **Commands**: `cargo run -p nyx`, `cargo run -p nyx-ui --example gallery`,
      `cargo test`, `cargo clippy -D warnings`, `cargo fmt`
- [ ] **Hard rules (call out loudly):**
  - In-house UI only — **no `gpui-component` / no external widget crate**
  - `nyx-ui` must **never** depend on any `nyx-*` crate (keeps Flint extraction
    trivial — see [`plan-02-nyx-ui-flint.md`](./plan-02-nyx-ui-flint.md))
  - **Never log credentials**; passwords live in the OS keychain only
  - GPUI is pinned to a rev — don't bump it casually
- [ ] **Conventions**: error handling (`thiserror` in libs, `anyhow` at edges),
      `tracing` for logs, styling via `StyledExt`/theme tokens (no raw hex in
      app code), `RenderOnce` for stateless components
- [ ] **Gotchas**: first GPUI build is long; macOS feature flags; Tokio↔GPUI
      bridge pattern
- [ ] **Pointers**: `docs/` plans, `design/` visual reference (prototype, not
      shipped code)

### Scoped `CLAUDE.md` for `nyx-ui`

- [ ] `crates/nyx-ui/CLAUDE.md` restating the extraction rules locally (no
      `nyx-*` deps, no domain types in component signatures, gallery-first
      workflow) — so an agent editing that crate sees the constraints in context

### Agent ergonomics & guardrails

- [ ] `.claude/settings.json` — allowlist common safe commands (`cargo build`,
      `cargo test`, `cargo clippy`, `cargo fmt`, `cargo run -p …`) to cut
      permission prompts
- [ ] Keep `docs/` as the canonical plan source; `CLAUDE.md` links to it (single
      source of truth, no drift)
- [ ] `AGENTS.md` as a thin alias/symlink to `CLAUDE.md` (covers other agent
      tools that look for that name)
- [ ] Note the build-verification loop for agents: change → `cargo clippy` →
      `cargo run -p nyx-ui --example gallery` to eyeball UI work

### Human-facing docs

- [ ] `docs/README.md` index linking the planning docs (and fix relative links
      now that plans live under `docs/plans/`)
- [ ] `README.md` build/run section kept in sync with `CLAUDE.md` commands

## Definition of done

- Root `CLAUDE.md` + `crates/nyx-ui/CLAUDE.md` exist and are accurate
- `.claude/settings.json` allowlists the common cargo commands
- `cargo run -p nyx` opens an empty themed GPUI window on macOS
- `cargo build` compiles the whole workspace (stubs included)
- `cargo run -p nyx-ui --example gallery` runs (even if it shows one component)
- fmt + clippy clean, CI green

## Risks

- **First GPUI build**: dependency split + native deps; the main unknown. Pin a
  SHA, keep cmake installed, adjust features for macOS.
- **GPUI API drift**: examples in these docs reflect current `main`; method
  names may shift. Pinning a SHA freezes this.
- **Tokio ↔ GPUI bridge**: deferred to feature work, but the `nyx-service`
  skeleton should establish the channel pattern early.
