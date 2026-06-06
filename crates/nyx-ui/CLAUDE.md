# CLAUDE.md — `nyx-ui`

This crate is the in-house GPUI component library + theme. It will be extracted
verbatim into a standalone crate named **Flint**, so it is built to be *zero-
coupling*. Treat these as hard constraints when editing here.

## The one rule that matters most

> **`nyx-ui` must never depend on anything Nyx-specific.** No `nyx-*` crate, no
> SFTP types, no app state. It depends on `gpui` only (the gallery example may use
> `gpui_platform` as a dev-dependency — that's fine; it's not a `nyx-*` dep).

If you reach for a `nyx-core` type here, stop — the design is wrong. Map domain
types to generic component props *in the app*, not in the component.

## Rules

- **No domain types in signatures.** A row renderer takes `impl IntoElement` or a
  closure — **never** a `RemoteEntry`.
- **Tokens are semantic and generic** (`bg_panel`, `accent`) — never
  `sftp_badge_color`. App-specific styling lives in the app.
- **Public API via the `prelude`**: `use nyx_ui::prelude::*;`.
- **Gallery-first.** Every public component gets a gallery entry
  (`examples/gallery.rs`) and doc comments. Iterate there before wiring into the
  app: `cargo run -p nyx-ui --example gallery`.
- **Apache-2.0 license header** on every file (matches the future Flint repo).
- **No external widget crate.** Build on GPUI primitives (`div`, the styling
  API). That includes `TextInput` — built in-house.

## Layout

- `theme.rs` — `Theme` token struct, `Global` impl, `ActiveTheme` accessor.
- `tokens.rs` — concrete One Dark / GitHub Dark tables (from `design/styles.css`).
- `styled_ext.rs` — `StyledExt`: theme-aware style recipes (the "@apply" layer).
- `components/` — one file per component, variant API (`Button` is the reference).
- `examples/gallery.rs` — the storybook.

## Build order & full spec

See [`../../docs/plans/plan-02-nyx-ui-flint.md`](../../docs/plans/plan-02-nyx-ui-flint.md).
`TextInput` is the hard one — build it early to surface its risk.
