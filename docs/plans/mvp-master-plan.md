# Plan 03 — MVP (SFTP V1)

Goal: turn the building skeleton + component library into a **usable SFTP
client** — create a profile, connect (with host-key verification), browse a
remote tree, and upload/download/rename/delete files with a live transfer queue.
Scope is exactly V1 from [`../overview.md`](../overview.md#v1-sftp-only); FTP/FTPS,
Linux packaging and the Flint/dbviewer extraction stay out.

## Where this picks up

- **Plan-01** (skeleton, hello window, crate stubs) — ✅ in [`done/`](./done/plan-01-project-init.md).
- **Plan-02** (`nyx-ui` component library + theme) — ✅ in [`done/`](./done/plan-02-nyx-ui-flint.md).
  The gallery has the full kit (button, icon_button, badge, text_input, modal,
  context_menu, toast, tooltip, tabs, progress_bar, table).
- **Gap to close:** there is a polished UI kit and a set of empty backend stubs
  (`nyx-service`, `nyx-protocol`, `nyx-transfer`, `nyx-profile`, `nyx-keyring`)
  that don't connect to anything. This plan wires them together, screen by
  screen and command by command.

The visual spec is the prototype in [`../../design/`](../../design/) — `app.jsx`
(shell grid), `sidebar.jsx`, `browser.jsx`, `transfers.jsx`, `statusbar.jsx`,
`welcome.jsx`, `tweaks-panel.jsx`. **Reference only — not shipped code.**

## Decisions to lock (before M2)

New `[workspace.dependencies]` to add as each milestone needs them:

| Dep | For | Milestone |
|---|---|---|
| `russh`, `russh-sftp` | SFTP transport + protocol | M2 |
| `keyring` | OS keychain credentials | M3 |
| `toml` (+ existing `serde`) | profile persistence format | M3 |
| `directories` | per-OS config/data paths | M3 |
| `tracing-subscriber` | app-edge log init | M1 |

Parked decisions to resolve when reached:
- **Host-key verification UX:** app-managed `known_hosts` file + trust-on-first-
  use prompt (a `Modal`), *not* silently trusting. (M2)
- **Profile encryption-at-rest:** beyond keychain for the secret, are non-secret
  profile fields plaintext TOML? Default yes for MVP. (M3)
- **Concurrency policy:** max parallel transfers (default 3?), per-file vs global.
  (M5)

---

## M1 — App shell (UI only, in-memory data)

> **Detailed breakdown:** [`mvp-m1-app-shell.md`](./mvp-m1-app-shell.md).

Assemble the real Nyx layout from existing `nyx-ui` components, driven by
hardcoded/in-memory state. **No backend.** This makes the app look real, creates
the surfaces real data plugs into later, and surfaces any `nyx-ui` gaps before
the library is frozen for Flint.

- [ ] `assets/` — vendor **JetBrains Mono** + **IBM Plex Sans**; load them at
      startup (GPUI `AssetSource` / `cx.text_system`). Set the UI/mono fonts.
- [ ] App state scaffold in `crates/nyx/src/`: a root `Entity<AppState>` holding
      the current view, selected profile, in-memory dir listing, fake transfers.
- [ ] **Welcome / connection manager** screen (`welcome.jsx`) — empty state +
      recent profiles list.
- [ ] **App shell grid** (`app.jsx`): sidebar | main column + status bar row.
- [ ] **Sidebar** (`sidebar.jsx`): profile groups, connection rows, footer
      actions — fed by a `Vec<Profile>` fixture.
- [ ] **Browser** (`browser.jsx`): tab bar + breadcrumb toolbar + filter box +
      the `Table` of `RemoteEntry` fixtures; sort/filter working on in-memory data.
- [ ] **Transfer dock** (`transfers.jsx`): `Tabs` (active/completed/failed) +
      rows with `ProgressBar`, fed by fake transfers.
- [ ] **Status bar** (`statusbar.jsx`).
- [ ] No raw hex in app code — only `nyx-ui` components + `StyledExt` + tokens.

**Done when:** `cargo run -p nyx` shows the full Nyx layout with believable fake
data; sorting/filtering/tab-switching work locally; clippy + fmt clean.

---

## M2 — SFTP vertical slice: connect + list

> **Detailed breakdown:** [`mvp-m2-sftp-connect-list.md`](./mvp-m2-sftp-connect-list.md).

The key risk-retirement milestone: prove the **Tokio↔GPUI channel bridge** and
the **`russh` API** end-to-end by listing one real remote directory.

- [ ] Expand `nyx-service` `Command` / `Event`: `Connect { profile, password }`,
      `ListDir { path }`, `Disconnect`; events `Connected`, `DirListing { path,
      entries }`, `Error { message }`, `HostKeyPrompt { fingerprint }`.
- [ ] Bridge: in the app, `cx.spawn` a task that drains the `Event` receiver and
      applies events to `AppState` (`cx.update`), redrawing the views.
- [ ] Implement `SftpClient` on `russh`/`russh-sftp`: `connect` (password auth
      first), host-key callback → `HostKeyPrompt`, `list_dir` → `Vec<RemoteEntry>`.
- [ ] Host-key trust: app-managed `known_hosts`; unknown key raises a `Modal`
      (trust-on-first-use); known/mismatch handled.
- [ ] Map `russh` errors → `NyxError` (no credentials in any message/log).
- [ ] Wire the browser to real data: clicking a connection connects + lists `/`;
      double-click a dir lists into it; up/refresh work.

**Done when:** against a real test SFTP server, the browser shows the live remote
listing; host-key prompt appears on first connect; the UI never blocks on the
backend.

---

## M3 — Profiles + keyring + connection editor

> **Detailed breakdown:** [`mvp-m3-profiles-keyring-editor.md`](./mvp-m3-profiles-keyring-editor.md).

Make connections persistent and credentials secure.

- [ ] `nyx-profile`: implement `FileProfileStore` over TOML in the per-OS config
      dir (`directories`); CRUD + load-on-startup. No secrets in the file.
- [ ] `nyx-keyring`: implement `OsKeyring` via the `keyring` crate
      (get/set/delete password). Address by `(service="nyx", account=profile id)`.
- [ ] **Connection editor** `Modal` (from `tweaks-panel.jsx` / modal styles):
      create / edit / delete a profile; password field writes to the keychain.
- [ ] **Test-connection** button: runs `Connect` + a probe, reports ok/err inline.
- [ ] Sidebar + welcome screen now read from the real store.

**Done when:** you can create a profile, store its password in the keychain,
restart the app, and reconnect without re-entering anything; deleting a profile
removes its keychain entry.

---

## M4 — File operations

Fill out the rest of the `RemoteClient` trait and the browser actions.

- [ ] `SftpClient`: `download`, `upload`, `rename`, `remove`, `mkdir`.
- [ ] Service commands + events for each; optimistic UI + refresh on completion.
- [ ] Browser wiring: context menu (download/rename/delete/copy-path), new-folder,
      drag-or-button upload, download-to chooser.
- [ ] Confirm-destructive `Modal` for delete; error `Toast`s on failure.

**Done when:** all six file ops work against the test server from the UI, with
confirmations and error toasts.

---

## M5 — Transfer queue (live)

- [ ] `nyx-transfer`: real queue with a concurrency cap; each transfer streams
      progress (bytes, speed) and supports cancellation.
- [ ] Service drives the queue on the Tokio thread; emits `TransferProgress`
      events throttled for the UI.
- [ ] Dock shows live `ProgressBar` + speed + status; cancel button works;
      completed/failed tabs populate; multiple concurrent transfers.

**Done when:** uploading/downloading several large files shows smooth live
progress and speed, transfers run concurrently up to the cap, and cancel stops
one mid-flight cleanly.

---

## M6 — Polish & MVP cut

- [ ] Empty/error/connecting states (`connecting` overlay from `welcome.jsx`).
- [ ] Keyboard: Enter/Backspace/F2/Delete in the browser; `⌘,` etc.
- [ ] Sort persistence per session; filter clears sensibly.
- [ ] `tracing` wired to a log file (never credentials); `anyhow` context at edges.
- [ ] Pass: no raw hex in app code; `nyx-ui` still has zero `nyx-*` deps.
- [ ] README/CLAUDE screenshots + a short "first connection" note.

**Done when:** the V1 scope in the overview is fully clickable end-to-end.

---

## Risks

- **`russh` API surface** — the largest unknown; M2 front-loads it. Build
  connect+list before anything else; consult `russh-sftp` examples.
- **Tokio↔GPUI bridge correctness** — event ordering, view updates off the
  channel, cancellation. Establish the pattern once in M2 and reuse it.
- **Host-key & credential handling** — must be correct and never logged; treat
  as a first-class feature, not an afterthought (M2/M3).
- **`nyx-ui` freeze for Flint** — M1 is the last cheap chance to find missing
  components; capture gaps as `nyx-ui` tasks, not app hacks.

## Definition of done (MVP)

- Create/edit/delete profiles; passwords in the OS keychain only.
- Connect over SFTP with host-key verification.
- Browse (name/size/type/modified/perms; open/up/refresh/sort/filter).
- Upload/download/rename/delete/create-folder.
- Live transfer queue: progress %, speed, status, concurrency, cancel.
- fmt + clippy clean, CI green, app code free of raw hex and of `nyx-*` leakage
  into `nyx-ui`.
