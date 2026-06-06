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

## Plans

Sequenced implementation plans live in [`plans/`](plans/).

- [`plans/post-mvp-hardening.md`](plans/post-mvp-hardening.md) — **active.**
  SFTP V1.1 hardening: path normalization, permissions model, overwrite
  handling, SSH key auth, secret boundary, plus tiered symlink/reconnect/
  collision work. The MVP (SFTP V1) is complete.
- [`plans/windows-build.md`](plans/windows-build.md) — enable the Windows build:
  trim the macOS-specific assumptions (keyring backend, GPUI features, titlebar)
  so the existing packaging script + release CI produce a working `.exe`.

## Visual reference

- [`../design/`](../design/) — an exported design prototype (HTML/CSS/React).
  **Reference only — not shipped code.** The theme tokens in `design/styles.css`
  are the source for `crates/nyx-ui/src/tokens.rs`.
