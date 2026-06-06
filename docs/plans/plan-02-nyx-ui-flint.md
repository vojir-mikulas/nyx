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

---

# Theming v2 — editable & pluggable themes

This extends the plan above. The v1 theme is two hardcoded Rust constructors
(`Theme::one_dark()` / `Theme::github_dark()`) returning a flat struct of
`Hsla`/`Pixels`. That is correct for components but a dead end for the two goals
we now want:

1. **Easy editing** — author/tweak a theme without recompiling, ideally with live
   reload, and let a new theme be ~10 lines (override a base, not restate every
   token).
2. **Plugin themes (later)** — third parties drop a theme into a folder and it
   appears in the picker. No code, no recompile, no security surface.

## The one principle that keeps this safe

> **Components never change.** They keep reading the resolved [`Theme`] global via
> `cx.theme()`. Everything below is an *authoring + resolution layer that sits
> above `Theme`* and produces one. The hot render path and the Flint-extraction
> contract are untouched.

So we split the world in two:

- **`Theme`** — *resolved* tokens. Fully concrete (`Hsla`, `Pixels`). What
  `render` reads. Lives in `theme.rs`. **No serde, no `Option`, no indirection.**
- **`ThemeSpec`** — *authored* tokens. Serde-(de)serializable, every token
  `Option`, supports `extends` + a named palette. What files/plugins contain.
  Lives in a new `theme_spec.rs`. Resolves **into** a `Theme`.

```
  one-dark.toml ──parse──▶ ThemeSpec ──resolve(base)──▶ Theme ──set_global──▶ cx.theme()
   (authoring)            (Option<…>)                  (concrete)            (components)
```

This is the shadcn/CSS-variables split: authoring is loose and DRY; the runtime
value components read is strict and flat.

## Required prerequisite refactor (small, do first)

`Theme` currently can't represent a runtime-loaded theme:

- `name: &'static str` → **`SharedString`** (plus add `id: SharedString`, a
  stable slug like `"one-dark"` used as the registry key and in settings).
- Keep `one_dark()` / `github_dark()` as today — they become the *built-in*
  `ThemeSpec`s' resolved output (or just stay as direct `Theme` builders that the
  registry seeds with). Either way the gallery toggle keeps working.

That's the only change to existing component-facing code.

## Color & metric authoring format

`Hsla` has no useful serde repr and hex is what `design/styles.css` already uses,
so introduce a tiny `Color` newtype in `nyx-ui` that (de)serializes from strings
and converts to `Hsla`:

- `"#5d80e6"` — opaque hex.
- `"#5d80e6/0.14"` — hex + alpha (replaces the v1 `.opacity(0.14)` calls).
- `"$blue"` — **palette reference** (resolves against the theme's `[palette]`).

Metrics (`row_height`, `radius`) author as plain numbers (px). Keep the set
small; resist adding per-component knobs — semantic tokens only (the existing
rule still holds: `bg_panel`/`accent`, never `sftp_badge_color`).

## `ThemeSpec` shape

```rust
// theme_spec.rs  (serde-gated, see features below)
#[derive(Clone, Deserialize, Serialize)]
pub struct ThemeSpec {
    pub id: String,
    pub name: String,
    pub appearance: Appearance,        // Dark | Light — drives default pick & os-sync
    pub extends: Option<String>,       // base theme id; inherit then override
    #[serde(default)]
    pub palette: HashMap<String, Color>,   // raw named colors: blue = "#61afef"
    #[serde(default)]
    pub tokens: TokenOverrides,        // every field Option<Color>/Option<f32>
}
```

Resolution order (in `ThemeSpec::resolve(&self, registry) -> Result<Theme>`):

1. Start from `extends` base's resolved `Theme` (or a hardcoded fallback if
   `extends` is `None`).
2. Resolve `$palette` refs and `#hex/alpha` strings to `Hsla`.
3. Apply each `Some(_)` token over the base; `None` keeps the inherited value.

**This is what makes editing easy:** `extends = "one-dark"` + 5 overridden tokens
is a complete theme. Change one palette entry, every token built on it updates.
Resolution errors (unknown ref, bad hex, `extends` cycle) are reported, and the
theme is skipped — a broken theme never panics the app.

## `ThemeRegistry` — many themes, switch at runtime

A second global holding all known themes and the active id:

```rust
pub struct ThemeRegistry {
    themes: IndexMap<SharedString, Theme>,   // id -> resolved
    active: SharedString,
}
impl Global for ThemeRegistry {}
// register(spec) resolves & inserts; activate(id) re-installs the `Theme` global
// and bumps a refresh so open windows redraw.
```

- Built-ins (One Dark, GitHub Dark) are registered at startup from in-crate
  `ThemeSpec`s (or direct builders).
- `activate(id)` swaps the `Theme` global and triggers `cx.refresh()`.
- The app exposes this as a theme picker; selection persists in `nyx-profile`
  (the *app* owns persistence — `nyx-ui` just exposes the registry API).

## Feature gating (keeps Flint core lean & extraction clean)

`nyx-ui` stays gpui-only at its core. New capability is opt-in:

- `serde` — `Color`/`ThemeSpec` derive + parse. Pulls `serde` + `toml`.
- `theme-loader` — read a directory of `*.toml` into the registry (needs `serde`
  + `std::fs`). Used by the app and the gallery.
- `hot-reload` — watch that directory (`notify`) and re-resolve on change.

None of these are `nyx-*` deps, so the extraction contract is intact. The pure
component/render path compiles with no features on.

## Build order (theming v2)

1. **Refactor** `name`→`SharedString` + add `id`. Gallery still green.
2. **`Color` newtype** + tests (hex, hex/alpha, `$ref`, round-trip). No rendering.
3. **`ThemeSpec` + `resolve`** behind `serde`; port both built-ins to `.toml`
   fixtures and assert `resolve == Theme::one_dark()` (parity test).
4. **`ThemeRegistry`** + runtime `activate`; gallery toggle now drives the
   registry instead of constructing a `Theme` inline. Add a 3rd theme that just
   `extends = "one-dark"` to prove inheritance.
5. **`theme-loader`** — app loads `~/<config>/nyx/themes/*.toml` at startup and
   merges into the registry; picker lists them.
6. **`hot-reload`** (dev ergonomics) — edit a `.toml`, see it live in the
   gallery/app. This is the "easily editable" payoff.

## Theme plugins (the "later")

Because a theme is **pure data**, "plugin themes" need almost no new machinery —
they are step 5 pointed at a plugins directory:

- Discover `themes/*.toml` inside each installed plugin folder; register with a
  namespaced id (`plugin-id/theme-id`) so two plugins can't collide.
- **Zero code execution → zero sandboxing needed.** This is the safe, shippable
  slice of "plugin support" and should land well before any code plugin.
- A theme plugin is just a folder + a `theme.toml` (+ optional manifest:
  `name`, `author`, `version`). Packaging/distribution is a later doc.

**Out of scope here:** plugins that ship *code* (custom components, behavior).
That needs a real extension boundary (stable ABI or WASM), capability/security
model, and lifecycle — its own plan (`plan-03-plugins.md`) when we get there.
Theming v2 deliberately delivers the 80% (custom themes) without opening that
door.

## Definition of done (theming v2)

- A theme is loadable from a `*.toml` file at runtime; the picker switches the
  live `Theme` global with no recompile.
- A new theme can be authored as `extends` + a handful of token overrides.
- Parity test: built-in `.toml` specs resolve to the same `Theme` as the v1
  hardcoded constructors.
- `nyx-ui` core still compiles with **no features** and **zero `nyx-*` deps**;
  serde/loader/hot-reload are additive features only.
- Malformed theme files are skipped with a logged error, never a panic.
