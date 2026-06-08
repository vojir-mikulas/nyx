# Nyx docs

The canonical source of truth for what Nyx is and how it's being built. Code-
level guidance for agents lives in [`../CLAUDE.md`](../CLAUDE.md), which links
back here.

## Start here

- [`overview.md`](overview.md) — what Nyx is, the guiding principles ("the Zed
  ideology"), tech stack, architecture, scope, and the reuse strategy toward the
  sibling app **dbviewer**.
- [`git-workflow.md`](git-workflow.md) — how we structure source control:
  trunk-based branching, conventional commits, versioning/releases, CI gates,
  and the repo hygiene files.
- [`user/`](user/README.md) — the **end-user guide** (install, getting started,
  troubleshooting). For people using the app, not building it.

## Plans

Sequenced implementation plans live in `plans/`. **Note:** `plans/` (and
`design/` below) are local working directories, kept out of version control via
`.gitignore` — the links in this file resolve only in a working checkout, not on
the GitHub mirror.

- [`plans/beta-release.md`](plans/beta-release.md) — **active.** Pre-release
  checklist for the first public open-source beta (`v0.1.0-beta.1`): version
  bump, `CHANGELOG.md`, README screenshot, Gatekeeper/notarization story, issue
  templates, and the tag-day sequence. Packaging, not code.
- [`plans/code-review-hardening.md`](plans/code-review-hardening.md) —
  **active.** Remediation of an external security/robustness review: FTPS
  changed-cert rejection and SFTP→local path-traversal (the two blockers),
  collision-gate stat-error handling, restrictive file perms, FTP serialization,
  channel backpressure, plus cargo-deny/license/doc-link cleanup. Two review
  findings (atomic overwrite, reconnect race) are already closed.
- [`plans/post-mvp-hardening.md`](plans/post-mvp-hardening.md) — **active.**
  SFTP V1.1 hardening: path normalization, permissions model, overwrite
  handling, SSH key auth, secret boundary, plus tiered symlink/reconnect/
  collision work. The MVP (SFTP V1) is complete.
- [`plans/windows-build.md`](plans/windows-build.md) — enable the Windows build:
  trim the macOS-specific assumptions (keyring backend, GPUI features, titlebar)
  so the existing packaging script + release CI produce a working `.exe`.
- [`plans/keyboard-controls.md`](plans/keyboard-controls.md) — app-wide keyboard
  controls: consolidate bindings into one keymap with a clean `key_context`
  hierarchy, add global shortcuts, modal Esc/Enter, richer browser navigation,
  and a shortcuts cheat-sheet.
- [`plans/folder-transfers.md`](plans/folder-transfers.md) — recursive folder
  download/upload, drag folders in and out, and multi-file selection: make a
  directory a first-class aggregate transfer that reuses the queue, progress
  dock, collision gate and path-locking. Extends `drag-out-to-desktop.md`.
- [`plans/ftp-ftps.md`](plans/ftp-ftps.md) — **done.** Added **FTP** and **FTPS**
  behind the existing `RemoteClient` trait: a protocol factory keyed on
  `profile.protocol`, a plain-FTP client and an FTPS client (explicit/implicit
  TLS) with TLS-cert TOFU trust, plus editor wiring. Phase 5 polish
  (anonymous login, server-quirk hardening) is the remaining tail.
- [`plans/anonymous-ftp-login.md`](plans/anonymous-ftp-login.md) — **proposed.**
  Add an **Anonymous** auth mode for FTP/FTPS (RFC 959 `anonymous` login) — no
  stored or prompted credential, modeled as a third `AuthMethod` variant. The
  remaining useful tail of `ftp-ftps.md` Phase 5.
- [`plans/auto-reconnect-resume.md`](plans/auto-reconnect-resume.md) — the
  deferred half of T2.2: **auto-reconnect** with backoff, and **transfer resume**
  from an offset after a drop. Detection + manual reconnect already ship.
- [`plans/per-file-error-surfacing.md`](plans/per-file-error-surfacing.md) —
  surface **which** entries failed/were skipped in a folder transfer, and **why**,
  not just an aggregate count. Extends folder-transfers Phase 6.
- [`plans/cancel-partial-tree-safety.md`](plans/cancel-partial-tree-safety.md) —
  make a cancelled/failed transfer leave a predictable destination: atomic
  temp-then-rename for files, honest "partial" state for folders, never destroy a
  merge target. Formalizes the deferred folder-transfers cancel risk.
- [`plans/windows-drag-out.md`](plans/windows-drag-out.md) — **backlog.** Windows
  drag-out parity (COM `IDataObject` delayed rendering) — the unbuilt Phase 3 of
  `drag-out-to-desktop.md`. Pure additive `nyx-drag` module; no app-code changes.
- [`plans/in-app-drag.md`](plans/in-app-drag.md) — **proposed.** A drag that
  starts as an in-app **move into folders** and auto-hands-off to the native
  drag-out only when the pointer leaves the window. Extends
  `drag-out-to-desktop.md`.
- [`plans/advanced-filtering.md`](plans/advanced-filtering.md) — **done.**
  Grew the browser filter into a small query language (glob + `type:`/`size:`/
  `ext:`/`modified:` predicates) with two scopes: current dir (default) and a
  **recursive search of the remote tree** via a leading `/` sigil, streamed and
  cancellable, with a Path-column results view. The shared matcher lives in
  `nyx-core`; ships independently of `large-listings.md` via a result cap.
- [`plans/user-themes.md`](plans/user-themes.md) — **proposed.** Let users drop
  a `*.toml` file into a `themes/` config dir and have it show up in the theme
  picker like a built-in. A serde DTO + base-overlay merge + directory scan in
  the app layer; the only `nyx-ui` change is `Theme.name: &'static str → String`,
  so Flint stays serde-free.
- [`plans/connection-accent-color.md`](plans/connection-accent-color.md) —
  **proposed.** The per-connection accent color already exists end to end but
  only tints two small icons; make it actually identify a connection (welcome
  card chip + titlebar segment), and optionally widen the Blue/Purple/Green
  palette to the theme's other named colors.

## Visual reference

- `../design/` — an exported design prototype (HTML/CSS/React). **Reference only —
  not shipped code,** and (like `plans/`) a local, `.gitignore`d working directory
  rather than a committed path. The theme tokens in `design/styles.css` are the
  source for `crates/nyx-ui/src/tokens.rs`.
