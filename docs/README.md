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

## Visual reference

- `../design/` — an exported design prototype (HTML/CSS/React). **Reference only —
  not shipped code,** and a local, `.gitignore`d working directory rather than a
  committed path. The theme tokens in `design/styles.css` are the source for
  `crates/nyx-ui/src/tokens.rs`.
