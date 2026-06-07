# Nyx — Overview

> A fast, reliable, cross-platform file-transfer client written in Rust, built the way Zed is built.

## What it is

Nyx is a desktop SFTP/FTP/FTPS client focused on **reliability, simplicity and
performance** rather than feature-completeness. You create connection profiles,
browse remote directories, and transfer files — with a polished native UI.

The design language is **Zed's "One Dark"** (plus a GitHub Dark theme), fully
tokenized. The visual reference lives in [`design/`](../design/) (an exported
Claude Design prototype in HTML/CSS/React — reference only, not shipped code).

## Guiding principles (the "Zed ideology")

- **Pure Rust, top to bottom.** No web stack. UI rendered with **GPUI** (Zed's
  own GPU-accelerated framework), not Tauri/React.
- **Cargo workspace of small, focused crates.** Clear boundaries; UI isolated
  from domain logic.
- **Own our UI layer.** A house-built component library (`nyx-ui`, later
  extracted as **Flint**) on top of raw GPUI — no third-party widget dependency.
- **Open-source**, GPL-3.0-or-later (GPUI's tree links GPL-3.0 crates from the
  Zed repo — see [`../NOTICE`](../NOTICE)), owner `vojir-mikulas`.
- **Performance & simplicity** as first principles.

## Tech stack

| Layer | Choice | Notes |
|---|---|---|
| Language | Rust (stable, 1.96 local; Zed pins 1.95) | |
| UI framework | **GPUI** (`git = zed-industries/zed`) | Tailwind-like styling, `Render`/`RenderOnce` components |
| UI components | **`nyx-ui`** (own crate → future **Flint**) | shadcn-style: owned source, themed, variant API, gallery |
| Async | **Tokio** on a dedicated backend thread | bridged to GPUI's own executor via channels |
| SFTP | `russh` / `russh-sftp` (Tokio-based) | |
| FTP / FTPS | `suppaftp` (Tokio) + `tokio-rustls` (ring) | FTPS cert trust via TOFU (`webpki-roots`, `sha2`) |
| Credentials | OS keychain (`keyring` crate) | never logged, never in profile files |
| Profiles | local persisted store | shareable with future apps |
| License | GPL-3.0-or-later | forced by GPL-3.0 crates in GPUI's tree; see `NOTICE` |

## Architecture at a glance

```
┌──────────────────────────── GPUI (main thread, own executor) ────────────────────────────┐
│  nyx (app binary)                                                                          │
│   ├─ root view + app state (Entity<T>)                                                     │
│   └─ views built from  ► nyx-ui  (Theme global, StyledExt, components)                     │
│                                                                                            │
│        ▲ progress/results (channel)              commands (channel) ▼                      │
└────────┼─────────────────────────────────────────────────────────┼───────────────────────┘
         │                                                           │
┌────────┴───────────────────────── Tokio backend thread ──────────┴───────────────────────┐
│  nyx-service   (owns connections + transfer queue, runs the Tokio runtime)                 │
│   ├─ nyx-protocol   RemoteClient trait + SftpClient / FtpClient / FtpsClient               │
│   ├─ nyx-transfer   queue, concurrency, progress, cancellation                             │
│   ├─ nyx-profile    profile CRUD + persistence                                             │
│   └─ nyx-keyring    OS-keychain credentials                                                │
│                                                                                            │
│  nyx-core   shared types (errors, RemoteEntry, transfer model) — no UI, no runtime knowledge│
└────────────────────────────────────────────────────────────────────────────────────────────┘
```

**Why a backend thread?** GPUI runs its own executor on the main thread; `russh`
is Tokio-based. A dedicated Tokio thread owns all connections and the transfer
queue, communicating with the UI over channels. The UI delivers results back
into views via `cx.spawn`. This keeps `nyx-protocol`/`nyx-transfer` as pure
Tokio crates with zero UI knowledge — and it models live transfer progress
cleanly (one entity observes the channel).

## Workspace layout (target)

```
nyx/
├── Cargo.toml                 # workspace; pins gpui to a zed git rev
├── rust-toolchain.toml        # match/track Zed's toolchain
├── crates/
│   ├── nyx/                   # GPUI app binary: window, root view, app state
│   ├── nyx-ui/               # ► component library (extract → Flint). Standalone, zero app coupling
│   ├── nyx-core/             # shared types: errors, RemoteEntry, transfer model
│   ├── nyx-service/          # Tokio backend thread + command/event channels
│   ├── nyx-protocol/         # RemoteClient trait + SftpClient / FtpClient / FtpsClient
│   ├── nyx-transfer/         # queue, concurrency, progress, cancellation
│   ├── nyx-profile/          # profile store
│   └── nyx-keyring/          # OS-keychain credentials
├── assets/                    # icons, fonts (JetBrains Mono, IBM Plex Sans)
├── design/                    # visual reference (prototype)
└── docs/                      # these documents
```

## Scope

### Shipped
- Protocols: **SFTP**, **FTP**, **FTPS** (explicit/implicit TLS), selected per
  profile and dispatched behind the `RemoteClient` trait
- Connection profiles: create / edit / delete, stored locally, test-connection
- Browse remote dirs: name, size, type, modified, permissions; open / up /
  refresh / sort / filter
- File ops: upload, download, rename, delete, create folder
- Transfer queue: progress %, speed, status (queued/running/completed/failed/
  cancelled), multiple concurrent transfers, cancel
- Trust-on-first-use: SSH host keys (SFTP) and TLS certificates (FTPS);
  credentials in OS keychain

### Later
- Linux packaging (macOS first)
- Extract `nyx-ui` → **Flint** standalone repo
- Sibling app **dbviewer** reusing Flint + the shared backend crates

### Non-goals (MVP)
Sync, diff tools, file editing, cloud providers, network shares, plugins,
terminal access, multi-pane manager.

## Reuse strategy (toward dbviewer)

Built clean-but-in-repo now, promoted to separate repos when dbviewer begins:
- **`nyx-ui` → Flint** — the component library + theme (see
  [`plan-02-nyx-ui-flint.md`](./plans/plan-02-nyx-ui-flint.md))
- **`nyx-keyring`, `nyx-profile`** — every connection-based app needs these
- **`nyx-service` pattern** — the Tokio-thread + channels scaffolding

App-specific (not shared): `nyx-protocol`, `nyx-transfer`.

## Open questions / parked decisions

- Exact GPUI rev to pin and the macOS feature-flag combination (GPUI is split
  into `gpui` + `gpui_platform` + `gpui_macros`; default features include
  Linux-only `x11`/`wayland`). We'll pin a SHA, e.g. `137e677…`, and resolve the
  feature set by building once. Reference: GPUI's own `crates/gpui/examples`.
- Profile encryption-at-rest details beyond keychain.

> **No third-party UI dependency.** `nyx-ui` is built entirely in-house on raw
> GPUI — we do not use `gpui-component` or any external widget library. That is
> the whole point of owning the UI layer (→ Flint).
