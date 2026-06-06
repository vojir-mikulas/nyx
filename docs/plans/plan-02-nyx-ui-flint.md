# Plan 02 — `nyx-ui` → Flint (build it extractable)

Goal: build the UI component library **`nyx-ui`** now, in-repo, so that
extracting it later into a standalone repo named **Flint** is a near-trivial
rename + move. The single most important rule:

> **`nyx-ui` must never depend on anything Nyx-specific.** No `nyx-core`,
> no SFTP types, no app state. It is a generic GPUI component library that
> happens to live in our repo today.

## Why shadcn-style

We treat `nyx-ui` like shadcn/ui, not like MUI:

- **We own the source** (it's our crate) — full control, no third-party widget dep.
- Built on **primitives we already have**: GPUI's `div`/element layer + its
  Tailwind-like styling API are the "Radix + Tailwind" equivalent.
- **Themeable** via tokens (the "CSS variables" analog).
- **Variant API** (the `cva` analog) — styling as a function of typed props.
- **Scoped**: ~12 components our design actually uses, not 40+.

## What GPUI already gives us (don't rebuild)

- **Tailwind-like styling**: `.flex().flex_col().gap_3().p_4().bg().rounded_md()
  .border_1().text_sm().hover(|s| …)` — built in.
- **Components**: `RenderOnce` (stateless, ≈ function component) and `Render`
  (stateful, held in `Entity<T>` ≈ store/class component).
- **Composition**: `.child()/.children()` ≈ JSX children.

What GPUI does **not** give us, and what `nyx-ui` adds: a **semantic theme
layer** + **house components** with a consistent variant API.

## Architecture of `nyx-ui`

```
crates/nyx-ui/
├── Cargo.toml            # depends ONLY on gpui (+ its own deps). No nyx-* crates.
├── src/
│   ├── lib.rs            # pub use prelude
│   ├── theme.rs          # Theme struct, Global impl, ActiveTheme trait, token sets
│   ├── styled_ext.rs     # StyledExt: .panel(cx), .row_h(cx), .focus_ring(cx) — the "@apply"
│   ├── tokens.rs         # One Dark + GitHub Dark token tables (ported from design/styles.css)
│   └── components/
│       ├── button.rs        # variant + size + disabled + on_click
│       ├── icon_button.rs
│       ├── text_input.rs    # single-line, stateful (the hard one — build FIRST)
│       ├── badge.rs
│       ├── modal.rs
│       ├── context_menu.rs
│       ├── toast.rs
│       ├── tabs.rs
│       ├── progress_bar.rs
│       ├── tooltip.rs
│       └── table.rs         # uniform_list-based (rows are fixed-height in our design)
└── examples/
    └── gallery.rs        # the "storybook": every component, both themes, all states
```

### Theme layer (the extraction-safe core)

```rust
// theme.rs
pub struct Theme {
    pub bg_app: Hsla, pub bg_panel: Hsla, pub bg_elevated: Hsla,
    pub text: Hsla, pub text_muted: Hsla,
    pub border: Hsla, pub accent: Hsla,
    pub row_height: Pixels,
    // … full token set from design/styles.css
}
impl Global for Theme {}

pub trait ActiveTheme { fn theme(&self) -> &Theme; }
impl ActiveTheme for App { fn theme(&self) -> &Theme { self.global::<Theme>() } }

impl Theme {
    pub fn one_dark() -> Self { /* ported tokens */ }
    pub fn github_dark() -> Self { /* ported tokens */ }
}
```

### StyledExt (the "@apply" / component classes)

```rust
// styled_ext.rs
pub trait StyledExt: Styled + Sized {
    fn panel(self, cx: &App) -> Self {
        self.bg(cx.theme().bg_panel).border_1().border_color(cx.theme().border)
    }
    fn row_h(self, cx: &App) -> Self { self.h(cx.theme().row_height) }
    fn focus_ring(self, cx: &App) -> Self { self.border_color(cx.theme().accent) }
}
impl<T: Styled> StyledExt for T {}
```

### Component with variant API (the `cva` analog)

```rust
// components/button.rs
#[derive(IntoElement)]
pub struct Button {
    label: SharedString,
    variant: ButtonVariant,  // Primary | Ghost | Danger
    size: ButtonSize,        // Sm | Md
    disabled: bool,
    on_click: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
}
// usage reads declaratively, like JSX:
//   Button::new("Connect").variant(Primary).size(Sm).on_click(|_,_,_| …)
```

## Build order (de-risk first)

1. **Theme + StyledExt + tokens** — port `design/styles.css` variables (both
   themes). Nothing renders yet, but the foundation is set.
2. **Gallery harness** — an example binary that installs a `Theme` global and
   renders a scratch view. A theme toggle switches One Dark ↔ GitHub Dark.
3. **`TextInput` (single-line)** — the hardest widget (cursor, selection,
   clipboard). Build it first so its risk is known early. Built **in-house** on
   GPUI's text/element primitives — no external widget crate. Our design only
   needs single-line fields, so this is far smaller than a full editor.
4. **`Button`, `IconButton`, `Badge`** — fast wins, prove the variant pattern.
5. **`Modal`, `ContextMenu`, `Toast`, `Tooltip`** — overlay components.
6. **`Tabs`, `ProgressBar`** — for the transfer dock.
7. **`Table`** — `uniform_list`-based; sortable header; selection. The file
   browser's backbone.

Each component lands in the **gallery first**, then gets used by the app. You
iterate on a button without launching the SFTP stack.

## Rules that keep extraction trivial

- [ ] `nyx-ui/Cargo.toml` lists **no `nyx-*` dependency**. Enforce by review.
- [ ] No domain types in signatures. A row renderer takes a generic
      `impl IntoElement` / closure, **not** a `RemoteEntry`. (The app maps its
      types to the component's props.)
- [ ] Theme tokens are **semantic and generic** (`bg_panel`, `accent`) — never
      `sftp_badge_color`. App-specific styling lives in the app.
- [ ] Public API via a `prelude` module: `use nyx_ui::prelude::*;`
- [ ] Component docs + a gallery entry for every public component.
- [ ] License header Apache-2.0 in each file (matches future Flint repo).

## The extraction (later, when dbviewer starts)

Because of the rules above, "make it Flint" is mechanical:

1. `git subtree split --prefix=crates/nyx-ui` (preserves history) → push to new
   `vojir-mikulas/flint` repo.
2. Rename crate `nyx-ui` → `flint`, module path `nyx_ui` → `flint` (find/replace).
3. In Nyx, replace the path dependency with
   `flint = { git = "https://github.com/vojir-mikulas/flint", rev = "…" }`.
4. dbviewer depends on the same `flint`.

If the no-coupling rules held, steps 2–4 touch only `Cargo.toml`s and import
paths — no logic changes.

## Definition of done (for the `nyx-ui` milestone)

- `cargo run -p nyx-ui --example gallery` shows all ~12 components, in both
  themes, in their key states.
- `nyx-ui/Cargo.toml` has zero `nyx-*` dependencies.
- The app (`nyx`) renders its real screens using only `nyx-ui` components +
  `StyledExt`, with no raw hex colors in app code.
