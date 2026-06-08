# Custom themes

Nyx ships with three built-in themes — **One Dark**, **GitHub Dark** and **Ayu
Dark** — selectable under **Settings → Tweaks → Color scheme** (⌘,). You can also
write your own: drop a `.toml` file into Nyx's `themes/` folder and it shows up in
that same picker.

## Where to put a theme

Create a `themes/` folder inside Nyx's config folder and put one `.toml` file per
theme in it:

| OS | Folder |
|---|---|
| macOS | `~/Library/Application Support/dev.nyx.Nyx/themes/` |
| Linux | `~/.config/dev.nyx.Nyx/themes/` (or `$XDG_CONFIG_HOME/...`) |
| Windows | `%APPDATA%\dev.nyx.Nyx\themes\` |

Themes are loaded **at startup**, so restart Nyx after adding or editing a file.
Your theme appears in the picker under the `name` you give it.

> **In a hurry?** Grab [`example-theme.toml`](example-theme.toml) — a complete,
> fully-commented theme. Copy it into the `themes/` folder above, rename it, tweak
> the colors, and restart.

## A minimal theme

You only list the tokens you want to change — everything else is inherited from a
**base** theme (One Dark unless you set `base`). So a usable theme can be five
lines:

```toml
name = "My Theme"
base = "One Dark"

[colors]
accent = "#e06c75"
```

That gives you One Dark with a red accent. Save it as
`themes/my-theme.toml`, restart, and pick "My Theme".

## Colors

Colors are hex strings in `[colors]`, in any of these forms:

- `"#rgb"` — short form, e.g. `"#fff"`
- `"#rrggbb"` — e.g. `"#282c33"`
- `"#rrggbbaa"` — with alpha, e.g. `"#98c37929"` (≈16% opacity). Use this for the
  translucent tokens like `accent_ghost`.

Every token, grouped by role:

| Token | Used for |
|---|---|
| `bg_app` | Main file-browser surface |
| `bg_panel` | Sidebar + transfer dock |
| `bg_panel_2` | Deepest surface (status bar, empty states) |
| `bg_elevated` | Modals, popovers, active tab |
| `bg_bar` | Toolbars / tab strip |
| `bg_hover` | Hovered row/control background |
| `bg_active` | Selected/active neutral background |
| `bg_selected` | Selected-row background (tinted) |
| `bg_input` | Text-field background |
| `border` | Default borders |
| `border_soft` | Subtle dividers |
| `border_strong` | Emphasized borders |
| `text` | Primary text |
| `text_muted` | Secondary text |
| `text_faint` | Tertiary text |
| `text_dim` | Dimmest text (labels, disabled) |
| `accent` | Primary action / highlight |
| `accent_hover` | Accent, hovered |
| `accent_ghost` | Translucent accent (focus-ring glow) — use an alpha hex |
| `on_accent` | Text/icons on top of `accent` |
| `green` | Success, FTPS |
| `red` | Error, danger |
| `blue` | Info, running transfers, FTP, folders |
| `purple` | SFTP |
| `yellow` | Warning |
| `orange` | Archives, secondary warning |

## Layout

Optional. Sizes are in logical pixels.

```toml
[layout]
row_height = 26.0   # file-row height
radius     = 5.0    # default corner radius
radius_sm  = 3.0    # small radius (chips, icon buttons, menu items)
```

## Tips & gotchas

- **`name` is required.** A file without it is skipped.
- **`base` must be a built-in** (`"One Dark"`, `"GitHub Dark"`, `"Ayu Dark"`). You
  can't base a theme on another custom theme.
- **A bad file is skipped, not fatal.** If a theme doesn't appear, check the log
  (see [Troubleshooting](troubleshooting.md)) — a misspelled token name, a bad
  hex value, or a missing `name` is reported there with the file path.
- **Don't reuse a built-in name.** A custom theme named "One Dark" is ignored so
  the built-ins stay stable; pick a unique `name`.
- **Some tints are derived.** A few accents (e.g. status badges) are computed by
  fading a base color rather than reading their own token, so they can't be
  retargeted independently yet. Set the base color (`green`, `red`, …) and the
  tint follows.

## Full example

A complete theme, overriding everything (a good starting point — copy and edit):

```toml
name = "Midnight"
base = "One Dark"

[colors]
bg_app       = "#1b1d23"
bg_panel     = "#16181d"
bg_panel_2   = "#121317"
bg_elevated  = "#22252d"
bg_bar       = "#191b21"
bg_hover     = "#21242c"
bg_active    = "#2a2e38"
bg_selected  = "#26344d"
bg_input     = "#101216"
border       = "#2c303a"
border_soft  = "#22252d"
border_strong = "#3a3f4b"
text         = "#d7dae0"
text_muted   = "#969ca8"
text_faint   = "#666c78"
text_dim     = "#525863"
accent       = "#7aa2f7"
accent_hover = "#94b4ff"
accent_ghost = "#7aa2f729"
on_accent    = "#16181d"
green        = "#9ece6a"
red          = "#f7768e"
blue         = "#7dcfff"
purple       = "#bb9af7"
yellow       = "#e0af68"
orange       = "#ff9e64"

[layout]
row_height = 26.0
radius     = 5.0
radius_sm  = 3.0
```
