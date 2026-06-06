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

Sequenced implementation plans live in [`plans/`](plans/):

- [`plans/plan-01-project-init.md`](plans/plan-01-project-init.md) — workspace
  skeleton, GPUI pin, "hello window", crate contracts, CI, AI-dev setup.
- [`plans/plan-02-nyx-ui-flint.md`](plans/plan-02-nyx-ui-flint.md) — the
  `nyx-ui` component library, built to be extracted as **Flint**.

## Visual reference

- [`../design/`](../design/) — an exported design prototype (HTML/CSS/React).
  **Reference only — not shipped code.** The theme tokens in `design/styles.css`
  are the source for `crates/nyx-ui/src/tokens.rs`.
