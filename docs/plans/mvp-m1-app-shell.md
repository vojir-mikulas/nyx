# Plan M1 — App shell (UI only, in-memory data)

> Detailed breakdown of **M1** from [`mvp-master-plan.md`](./mvp-master-plan.md).
> Scope: assemble the real Nyx layout from existing `nyx-ui` components, driven
> by hardcoded/in-memory state. **No backend, no network, no disk** (except font
> assets). The app should *look* finished and have working local interactions
> (sort, filter, tab-switch, selection, navigation, dock collapse).

## Why this milestone exists

1. Make the app look real and create the surfaces that real data plugs into in
   M2–M5 — the wiring seams are defined here, then filled later.
2. **Surface `nyx-ui` gaps before the library is frozen for Flint.** M1 is the
   last cheap chance to discover missing components. Every gap is captured as a
   `nyx-ui` task (a real component) *or* an explicit app-local composition — never
   an app hack that smuggles styling decisions into the app.
3. Establish the `Entity<AppState>` + view-tree pattern the whole app builds on.

## Inputs (what already exists)

- **App binary** ([`crates/nyx/src/main.rs`](../../crates/nyx/src/main.rs)) — a
  "hello window": installs `Theme::one_dark()` and opens an empty 1000×680 window.
  No `AppState`, root view is `struct Nyx;`.
- **`nyx-ui` kit** — `Button`, `IconButton`, `Badge`, `TextInput` (stateful),
  `Modal`, `ContextMenu`, `Toast`, `Tooltip`, `Tabs`, `ProgressBar`, `Table`
  (+ `Column`), `Theme`/`ActiveTheme`/`cx.theme()`, `StyledExt`
  (`.panel`/`.elevated`/`.row_h`/`.focus_ring`). No icon set — icons are any
  `impl IntoElement`.
- **`nyx-core` types** (the domain model M1 fixtures are built from):
  - `Protocol { Sftp, Ftp, Ftps }` + `default_port()`
  - `EntryKind { File, Directory, Symlink, Other }`
  - `RemoteEntry { name, size: u64, kind, modified: Option<SystemTime>, perms: String, is_dir }`
  - `Profile { id, name, protocol, host, port, username, remote_path: Option<String> }`
  - `Transfer { id: TransferId, direction, remote_path, local_path, total_bytes: Option<u64>, transferred_bytes, status }` + `progress() -> Option<f32>`
  - `TransferStatus { Queued, Running, Completed, Failed, Cancelled }`, `TransferDirection { Upload, Download }`
- **Visual spec** — [`design/`](../../design/): `app.jsx`, `sidebar.jsx`,
  `browser.jsx`, `transfers.jsx`, `statusbar.jsx`, `welcome.jsx`,
  `tweaks-panel.jsx`. Reference only.

---

## Decision: domain types vs. UI view-models

The prototype renders fields that have **no place in `nyx-core`** (they're
presentation state, not transfer/protocol facts):

| Prototype field | Where it belongs in M1 |
|---|---|
| connection `color`, `recent`, `lastUsed`, online/active state | app-local view-model |
| transfer **speed**, **error message**, display name/path split | app-local view-model |
| file **type label** ("HTML", "Folder"), display size ("—" for dirs) | derived in the app from `RemoteEntry` |

**Rule for M1:** `nyx-core` types are *not* extended with UI fields. The `nyx`
crate defines thin **view-models** that wrap a core type plus its UI-only state.
This keeps `nyx-core` a clean domain model and keeps every `nyx-ui` component
domain-free (components still receive `impl IntoElement`/closures, never a
`Profile` or `RemoteEntry`). Example:

```rust
// crates/nyx/src/state/models.rs
struct ConnectionVm {
    profile: nyx_core::Profile,   // the real domain type
    color: AccentKind,            // UI-only
    last_used: Option<SharedString>,
    is_recent: bool,
}
struct TransferVm {
    transfer: nyx_core::Transfer, // the real domain type
    speed_bps: Option<u64>,       // UI-only until M5 wires real progress
    error: Option<SharedString>,  // UI-only display copy
}
```

`transferred_bytes`/`total_bytes` and `status` already live on `Transfer`, so the
dock reads progress straight from the domain type — only `speed`/`error` are
synthetic in M1.

---

## File layout to create in `crates/nyx/src/`

```
crates/nyx/src/
├── main.rs               # window setup: AssetSource, fonts, TextInput::bind_keys, root view
├── app.rs                # root NyxApp view: shell grid, routes on AppState.view
├── state/
│   ├── mod.rs            # AppState (Entity), View enum, all interaction logic
│   ├── models.rs         # ConnectionVm, TransferVm, EntryRow, view-model helpers
│   └── fixtures.rs       # fake_connections(), fake_listing(), fake_transfers()
├── assets.rs             # embedded AssetSource for fonts
└── views/
    ├── welcome.rs        # welcome / connection manager screen
    ├── sidebar.rs        # profile groups + footer
    ├── browser.rs        # tab strip + toolbar + breadcrumb + filter + Table
    ├── transfer_dock.rs  # Tabs + transfer rows + ProgressBar
    └── status_bar.rs     # bottom status row
```

Most views are `RenderOnce` helpers that take `&AppState` data and emit elements;
`AppState` itself is the single `Entity` holding all mutable state and the
`cx.listener` handlers. (Resist a separate `Entity` per panel in M1 — one root
entity keeps the data-flow obvious; split later only if a panel needs local
focus/scroll state, e.g. the filter `TextInput`, which *is* its own `Entity`.)

---

## `AppState` shape (the single source of truth)

```rust
enum View { Welcome, Browse }   // M1 omits Connecting (that's M2/M6)

struct AppState {
    view: View,
    connections: Vec<ConnectionVm>,
    active_id: Option<String>,        // connection currently "open" in the browser
    online_id: Option<String>,        // connection shown as connected (fake: == active)

    // browser
    cwd: Vec<SharedString>,           // path segments, e.g. ["var","www"]
    history: Vec<Vec<SharedString>>,  // back/forward stack
    history_ix: usize,
    listing: Vec<EntryRow>,           // fixtures for the current cwd
    filter: Entity<TextInput>,        // the filter box is stateful
    sort: (SortKey, bool),            // (column, ascending)
    selected: HashSet<SharedString>,  // selected entry names

    // transfer dock
    dock_open: bool,
    dock_tab: DockTab,                // All | Active | Completed | Failed
    transfers: Vec<TransferVm>,

    // tweaks (in-memory only in M1)
    density: Density,                 // Compact | Comfortable | Spacious
    show_perms: bool,
}
```

Derived getters (no stored duplication): `visible_entries()` applies filter+sort,
`dock_rows()` filters `transfers` by `dock_tab`, `selected_count()`, `item_count()`.

---

## Tasks

### 1. Fonts & assets
- [ ] Vendor **JetBrains Mono** and **IBM Plex Sans** (the families named in
      [`overview.md`](../overview.md) and the prototype's `--font-mono`/`--font-ui`)
      into a new top-level `assets/fonts/` dir. Record the license files.
- [ ] `assets.rs`: implement a GPUI `AssetSource` (e.g. `rust-embed` over
      `assets/`) and pass it to `Application::new().with_assets(..)`.
- [ ] At startup, load the fonts via `cx.text_system().add_fonts(..)` and set the
      UI font (IBM Plex Sans) + mono font (JetBrains Mono). Confirm the `Table`,
      breadcrumb, sizes, dates and transfer rows render in the right family
      (mono for paths/sizes/dates, UI for everything else).
- [ ] Call `TextInput::bind_keys(cx)` once at startup (required for the filter box
      and any modal inputs to accept keystrokes).

### 2. App state scaffold
- [ ] `state/models.rs` — `ConnectionVm`, `TransferVm`, `EntryRow` (wraps
      `RemoteEntry` + derived `type_label` and display size), `AccentKind`,
      `SortKey`, `DockTab`, `Density`.
- [ ] `state/fixtures.rs` — believable fake data:
      - `fake_connections()` → ~4–6 `ConnectionVm` across all three protocols,
        some `is_recent`, with `last_used` strings and distinct accent colors.
      - `fake_listing(cwd)` → ~15–25 `RemoteEntry` mixing dirs and files with
        realistic names, sizes, perms, and `modified` times; enough to exercise
        scroll, sort and filter. A couple of nested dirs so navigation works.
      - `fake_transfers()` → a spread across **all** `TransferStatus` values
        (running w/ mid progress + speed, queued, completed, failed w/ error,
        cancelled), both directions.
- [ ] `state/mod.rs` — `AppState` + `View`; `cx.new` it in `main.rs`; root view
      holds `Entity<AppState>` and observes it.

### 3. App shell grid (`app.jsx` → `views`/`app.rs`)
- [ ] Root layout: a vertical flex of **(sidebar | main column)** row + **status
      bar** row. Sidebar fixed width (~244px), main column `flex_1`. Status bar
      ~22px pinned bottom. Use `bg_panel` for sidebar/dock, `bg_app` for main.
- [ ] Route the main column on `AppState.view`: `Welcome` → welcome screen;
      `Browse` → browser + (optional) transfer dock stacked vertically.
- [ ] Sidebar collapse toggle (button in the tab strip) — animate width to 0 or
      just toggle for M1. Track `sidebar_open: bool` if implemented.

### 4. Welcome / connection manager (`welcome.jsx`)
- [ ] Centered column: logo + "Welcome to Nyx" + subtitle.
- [ ] "Saved connections" section: card rows (icon, name + protocol `Badge`,
      `user@host:port` + path in mono, chevron). Click → set `active_id`,
      `online_id`, switch `view` to `Browse`, load `fake_listing(root)`.
- [ ] "Recent connections" section (filter `is_recent`): compact rows with a
      clock icon + `last_used`.
- [ ] "New connection" dashed button with `⌘N` hint. In M1 this opens a **stub**
      (a `Toast` "coming soon" or an empty `Modal`); the real editor is M3.

### 5. Sidebar (`sidebar.jsx`)
- [ ] Header: brand + new-connection `IconButton`.
- [ ] Scrollable groups: **Saved · N** and **Recent · N** section headers, then
      connection rows fed by `connections`.
- [ ] Connection row: online dot, name, faint mono `user@host`, protocol `Badge`,
      hover bg, active state (accent left-bar + `bg_active`). Click → open in
      browser (same as welcome card). Right-click → `ContextMenu` (Edit stub /
      Remove stub for M1).
- [ ] Footer: "New" `Button` (flex) + settings `IconButton`.

### 6. Browser (`browser.jsx`)
- [ ] **Tab strip**: active connection tab (protocol-colored icon + name + close
      `IconButton`), spacer, sidebar-toggle + dock-toggle `IconButton`s (dock
      toggle `.active(dock_open)`).
- [ ] **Toolbar**: Back / Forward / Up / Refresh `IconButton`s with correct
      disabled states (`history_ix`, `cwd` depth); breadcrumb; filter box; New
      folder (`Secondary`) + Upload (`Primary`) buttons (actions are stubs/toasts
      in M1).
- [ ] **Breadcrumb** (mono, clickable crumbs `/` › `var` › `www`) — see Gap G1.
      Clicking a crumb truncates `cwd` and reloads `fake_listing`.
- [ ] **Filter box**: the `Entity<TextInput>` from `AppState`; on change, redraw
      with `visible_entries()`. Focus shows `.focus_ring`.
- [ ] **File table** via `Table` + `Column`s: Name (flex, sortable) · Size (fixed,
      end-align, sortable, mono) · Modified (fixed, mono, sortable) · Type
      (fixed, sortable) · Permissions (fixed, mono — shown only when
      `show_perms`). `render_row` builds each row from an `EntryRow`: dir names in
      `theme.blue`, files in `theme.text`; size "—" for dirs; hover row-action
      `IconButton`s (download/rename/delete) at low opacity.
      - `on_sort` → toggle `sort`; `on_select` → selection (plain click replaces,
        cmd/ctrl-click toggles in `selected`).
      - Double-click a dir → push `cwd`, push history, reload listing.
      - Empty state: folder glyph + "This folder is empty" / "No matches for
        '<filter>'".
- [ ] Row height from `density` (compact 22 / comfortable 26 / spacious 30) via
      `Table::row_height`.

### 7. Transfer dock (`transfers.jsx`)
- [ ] Collapsible: 32px header always; body shown when `dock_open`.
- [ ] Header: collapse chevron `IconButton`, `Tabs` (Transfers N · Active n ·
      Completed n · Failed n with count pills), "clear finished" `IconButton`.
      `Tabs::on_select` sets `dock_tab`.
- [ ] Transfer row from `TransferVm`: direction icon (up/down, colored), file name
      (mono) + faint path, `ProgressBar` (running/queued only — fraction from
      `Transfer::progress()`), bytes done/total (mono), speed string, status
      `Badge` (Running→Info/%, Queued→Neutral, Completed→Success, Failed→Danger,
      Cancelled→Neutral), trailing cancel/retry `IconButton` (stub).
- [ ] Empty state per tab: "No transfers here."

### 8. Status bar (`statusbar.jsx`)
- [ ] Connected: online dot + label, protocol `Badge`, mono `user@host:port`
      button, spacer, (if active transfers) zap + total speed + active count,
      item/selection count (`item_count()` / `selected_count()`), dock-toggle +
      settings `IconButton`s.
- [ ] Disconnected (`view == Welcome`): offline dot + "No connection", spacer,
      mono version label, settings button.

### 9. Interactions (local, no backend)
- [ ] Sort: clicking a sortable header cycles asc/desc; `visible_entries()` sorts
      by `SortKey` (dirs-first within a key is a nice touch — match the prototype).
- [ ] Filter: case-insensitive substring on name; live as you type.
- [ ] Navigation: Up / Back / Forward / breadcrumb / double-click-dir all mutate
      `cwd` + `history` and reload from `fixtures` (each fake dir returns a small
      canned listing; unknown dirs return empty).
- [ ] Selection + counts reflected in the status bar.
- [ ] Dock tab switching and collapse.

### 10. Tweaks (in-memory)
- [ ] Density and `show_perms` are togglable somewhere reachable (a small
      tweaks `Modal` or, minimally, hard-wired defaults with a TODO). Full
      tweaks-panel polish is optional in M1; density + perms column must work
      because they exercise `Table` config. Theme/accent switching can reuse the
      gallery's existing toggle approach but is **optional** here.

### 11. Cleanliness pass
- [ ] **No raw hex anywhere in app code** — only `cx.theme()` tokens, `nyx-ui`
      components, and `StyledExt`. Any color the design needs that has no token →
      add a *semantic* token to `nyx-ui` (not an app constant).
- [ ] `nyx-ui` still has **zero `nyx-*` deps** (unchanged; M1 must not tempt a
      shortcut that passes a domain type into a component).
- [ ] `cargo fmt --all`; `cargo clippy --workspace --all-targets -- -D warnings`.

---

## `nyx-ui` gaps to resolve (the Flint-freeze checkpoint)

The prototype needs a few patterns the kit lacks. **Default: add them to
`nyx-ui`** so they're frozen into Flint; only compose app-locally when the pattern
is genuinely app-specific. Decide each explicitly:

| # | Gap | Recommendation |
|---|---|---|
| G1 | **Breadcrumb** (clickable path segments with separators) | Borderline generic. Prefer a small `nyx-ui` `Breadcrumb` taking `Vec<segment + on_click>`; if it feels too app-shaped, compose from `IconButton`/text in `browser.rs` and note the gap. |
| G2 | **Segmented control** (protocol picker, density picker) | Add to `nyx-ui` — generic and reused by the M3 connection editor and tweaks. |
| G3 | **Toggle switch** (perms column, future settings) | Add to `nyx-ui` — generic form control. |
| G4 | **Divider / section label** (welcome + sidebar group headers) | Trivial; either a `nyx-ui` `Divider` or `StyledExt` helper. |
| G5 | **Status/online dot** (sidebar rows, status bar) | App-local: it's a 6px themed circle, not worth a component — but use a token color, no hex. |
| G6 | **Spinner** (connecting overlay) | Not needed in M1 (no Connecting view). `ProgressBar::indeterminate` covers most cases; defer a dedicated spinner to M2/M6. |
| G7 | **Icon set** | `nyx-ui` is intentionally icon-agnostic. M1 decision needed: ship an app-side icon helper (embedded SVGs via the new `AssetSource`, or a curated set). Keep it in the **app**, not `nyx-ui` (Flint stays icon-provider-free). |

Capture each chosen gap as its own checklist item / tiny PR against `nyx-ui` with
a gallery entry — same bar as plan-02. **Do not** let a gap become an app hack.

---

## Build-verification loop

After each panel: `cargo clippy --workspace --all-targets -- -D warnings`, then
eyeball with `cargo run -p nyx`. New `nyx-ui` components land in the gallery first
(`cargo run -p nyx-ui --example gallery`) before the app uses them, per plan-02.
(Build runs need `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer` —
see the build note in repo memory.)

## Risks / watch-items

- **Icon strategy (G7)** is the one real fork — pick embedded SVG assets early so
  every view uses the same approach; retrofitting icons is tedious.
- **One-entity vs. per-panel entities** — start with a single `AppState` entity.
  Only the filter `TextInput` needs its own entity (focus/IME). Splitting further
  is a later refactor, not an M1 goal.
- **Table sort/filter on fixtures** must be pure/derived (no cached duplicate
  state) so M2 can swap the fixture source for real `DirListing` events with no
  logic change.
- **Don't smuggle styling into the app** to dodge a `nyx-ui` gap — that defeats
  the whole reason M1 runs before the Flint freeze.

## Definition of done (M1)

- `cargo run -p nyx` shows the full Nyx layout — sidebar, welcome screen,
  browser (tab strip + toolbar + breadcrumb + filter + table), transfer dock,
  status bar — with believable fake data and the vendored fonts.
- Sorting, filtering, tab-switching, selection, breadcrumb/up/back navigation,
  density and the perms-column toggle all work on in-memory data.
- Every `nyx-ui` gap is resolved as a real component (in the gallery) or an
  explicitly-noted app-local composition — no app hacks, no domain types in
  `nyx-ui` signatures.
- No raw hex in app code; `nyx-ui` has zero `nyx-*` deps.
- `cargo fmt --all` clean; `cargo clippy --workspace --all-targets -- -D warnings`
  clean.

## Implementation status (done)

M1 is implemented. Build/lint/fmt/tests all clean; `cargo run -p nyx` opens the
full shell driven by fixtures.

**Layout (`crates/nyx/src/`):** `main.rs` (asset source, font load, key bind,
root entity) · `assets.rs` (`rust-embed` over `assets/`, font loader) · `icon.rs`
(embedded-SVG helper) · `state/{mod,models,fixtures}.rs` · `views/{sidebar,
welcome,browser,transfer_dock,status_bar}.rs` + `app.rs` (`impl Render for
AppState`, shell grid, tweaks modal, toast overlay).

**Assets:** JetBrains Mono + IBM Plex Sans (multiple weights, OFL licenses) under
`assets/fonts/`; 31 line icons under `assets/icons/` (G7 — app-side, embedded
SVGs tinted via `text_color`; `nyx-ui` stays icon-provider-free).

**`nyx-ui` additions (gallery-first, zero `nyx-*` deps):**
- `Segmented` (G2) and `Toggle` (G3) — new components with gallery entries.
- `Table`: added `selected_set` (multi-selection), `on_activate` (double-click to
  open a dir), and `on_select` now passes the `ClickEvent` so the owner reads
  modifiers for cmd/ctrl-click. Gallery updated.
- `orange` semantic token (archive file icons), both themes.

**Gap decisions (G1–G7):**
- G1 Breadcrumb → composed app-locally in `browser.rs` (clickable mono crumbs);
  left out of `nyx-ui` for now as it reads app-shaped.
- G2 Segmented, G3 Toggle → added to `nyx-ui`.
- G4 Divider/section label → app-local one-liner with `border_soft` token.
- G5 Status dot → app-local `views::status_dot` (themed circle, no hex).
- G6 Spinner → deferred (no Connecting view in M1).
- G7 Icons → app-side embedded SVG set.

**Explicitly deferred (noted, not hacked):** per-row hover actions and the file-row
right-click `ContextMenu` are deferred to M3/M4 — the `Table` has no trailing-
action/row-context slot yet, and a positioned overlay is better built with the
M3 connection editor than smuggled in now. Sidebar/file right-click currently
shows a "coming in M3" toast. Connect is instant (no Connecting view). New
folder / Upload / Test / Edit / Remove / cancel / retry are stub toasts.

## Hand-off to M2

M1 leaves these seams for the SFTP slice to fill:
- `active_id`/`online_id` set on connect → M2 replaces the fake open with a
  `Connect` command + `Connected`/`HostKeyPrompt` events.
- `listing` populated by `fake_listing(cwd)` → M2 replaces the source with
  `DirListing { path, entries }` events; the derived `visible_entries()` sort/
  filter is unchanged.
- `transfers` (`TransferVm`) populated by `fake_transfers()` → M5 replaces with
  live `TransferProgress` events; `speed_bps`/`error` become real.
