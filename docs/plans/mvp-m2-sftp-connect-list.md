# Plan M2 — SFTP vertical slice: connect + list

> Detailed breakdown of **M2** from [`mvp-master-plan.md`](./mvp-master-plan.md).
> Scope: prove the two biggest unknowns end-to-end against a **real** SFTP
> server — the **Tokio↔GPUI channel bridge** and the **`russh`/`russh-sftp` API**
> — by connecting to one host (with host-key verification) and listing one real
> remote directory in the browser. **No file ops, no transfer queue, no profile
> persistence, no keyring** (those are M3–M5). Password is entered/held in memory
> for this slice only.

## Why this milestone exists

This is the **risk-retirement** milestone. M1 made the app *look* finished on
fixtures; M2 replaces the fixture source for **one** path — connect + list — with
live data, and in doing so nails down the two patterns every later milestone
reuses:

1. **The async bridge.** Establish *once* how the GPUI main thread issues
   `Command`s to the Tokio thread and drains `Event`s back into `AppState` via
   `cx.spawn`, including ordering and view redraws. M3–M5 only add new
   `Command`/`Event` variants — never a new bridge.
2. **The `russh` surface.** `russh` + `russh-sftp` are the largest unknown in the
   whole MVP. Build connect (password auth) + host-key callback + one `list_dir`
   before anything else; everything in M4/M5 (download/upload/rename/…) hangs off
   the same session.
3. **Host-key & credential handling as a first-class feature** — trust-on-first-
   use prompt, app-managed `known_hosts`, and a hard guarantee that no credential
   or secret ever reaches a log or an error string.

Everything here is deliberately narrow: one connection, read-only, one directory
at a time. If this slice is solid, the rest of the MVP is mostly filling in trait
methods behind a bridge that already works.

## Inputs (what already exists)

- **`nyx-service` skeleton**
  ([`crates/nyx-service/src/lib.rs`](../../crates/nyx-service/src/lib.rs)) — the
  thread + Tokio runtime + channel pattern is in place: `Command::Shutdown`,
  `Event::Ready`/`Stopped`, `ServiceHandle { send }`, `spawn() -> (ServiceHandle,
  StdReceiver<Event>)`. The command loop trivially exits today. **The app does
  not yet construct or hold a `ServiceHandle`.**
- **`nyx-protocol` skeleton**
  ([`crates/nyx-protocol/src/lib.rs`](../../crates/nyx-protocol/src/lib.rs)) —
  `RemoteClient` async trait (object-safe via `async_trait`) with
  `connect/list_dir/download/upload/rename/remove/mkdir/disconnect`, and
  `SftpClient` ([`sftp.rs`](../../crates/nyx-protocol/src/sftp.rs)) where every
  method `unimplemented!()`s. `russh`/`russh-sftp` are **not** dependencies yet.
- **`nyx-core` types** — `RemoteEntry { name, size, kind, modified, perms, is_dir }`,
  `EntryKind`, `Protocol`, and `NyxError { Connection, Auth, HostKey, Io,
  NotFound, Unsupported, Cancelled, Other }` with a `Result<T>` alias. These map
  cleanly from `russh` errors; no new variants are expected (confirm during
  error-mapping).
- **`AppState` seams** ([`crates/nyx/src/state/mod.rs`](../../crates/nyx/src/state/mod.rs)) —
  M1 left exactly these to fill:
  - `open_connection(id, cx)` sets `active_id`/`online_id` instantly and loads
    `fake_listing(cwd)`. M2 replaces the fake open with a real `Connect` command
    + `Connected`/`HostKeyPrompt` events.
  - `reload_listing()` / `go_to_path()` call `fixtures::fake_listing(&cwd)`. M2
    replaces the source with `DirListing` events; the derived `visible_entries()`
    sort/filter is **unchanged**.
  - `disconnect()` clears state locally. M2 adds a real `Disconnect` command.
- **Browser view** ([`crates/nyx/src/views/browser.rs`](../../crates/nyx/src/views/browser.rs))
  with working sort/filter/breadcrumb/up/back/forward on whatever `listing`
  contains — no UI change needed for it to show real data.
- **Toast + Modal** in `nyx-ui` (host-key prompt is a `Modal`; errors are
  `Toast`s — both already used by `AppState`).

---

## Decisions to lock (resolve before coding)

### D1 — Direction of the Event channel: make it foreground-awaitable
The skeleton returns `std::sync::mpsc::Receiver<Event>`, justified as "pollable
without an async context." But the bridge wants to `await` events on the GPUI
**foreground** executor inside `cx.spawn` — and a blocking `recv()` there would
freeze the UI. **Decision:** switch the service→UI side to a channel whose
receiver is a `Stream` so the bridge can `while let Some(ev) = rx.next().await`.

- **Recommended:** `futures::channel::mpsc::unbounded::<Event>()`. The Tokio
  thread calls `tx.unbounded_send(ev)` (non-blocking, needs no runtime context);
  the UI drains `rx` as a `Stream` on the foreground executor. `spawn()` returns
  `(ServiceHandle, UnboundedReceiver<Event>)`.
- **Fallback (only if a `futures` dep is unwanted):** keep the std receiver and
  drain it from `cx.background_executor().spawn` (a blocking `recv()` loop), then
  hop each event to the foreground with `cx.update`. More moving parts; prefer
  the stream.

Keep the UI→service side as the existing `tokio::sync::mpsc::UnboundedSender<Command>`
— sends are fire-and-forget from the GPUI thread and don't need a runtime context
to *send*.

### D2 — Host-key verification UX (TOFU)
App-managed `known_hosts` file (per-OS data dir — but for M2 a fixed path under
the config dir is fine; full `directories` wiring is M3). On connect:
- **Known + match** → proceed silently.
- **Unknown host** → emit `Event::HostKeyPrompt { host, fingerprint }`; the UI
  shows a **`Modal`** (trust-on-first-use). User **Trust** → append to
  `known_hosts`, continue; **Reject** → abort with `NyxError::HostKey`.
- **Known + mismatch** → **do not prompt**; abort with `NyxError::HostKey`
  ("remote host identification has changed"). This is the dangerous case; never
  silently trust it in M2.

The mechanism (how the async handler waits for the user's click) is **D3**.

### D3 — How the host-key handler awaits the user
`russh`'s `Handler::check_server_key` is async and must return `Ok(true/false)`
*before* auth proceeds — but the decision lives in the UI. Bridge it with a
**reply command + oneshot**, not by stuffing a sender into the (Clone) `Event`:
- The handler computes the fingerprint, consults `known_hosts`. On unknown, it
  registers a `tokio::sync::oneshot::Sender<bool>` in the connect task and emits
  `Event::HostKeyPrompt { host, fingerprint }`, then `.await`s the oneshot
  receiver.
- The UI's Trust/Reject buttons send `Command::HostKeyDecision { accept: bool }`.
  The service resolves the pending oneshot, the handler returns, auth continues
  or aborts. (M2 has at most one in-flight connect, so a single pending slot is
  enough; key it by host if you want to be defensive.)

### D4 — Where the password comes from in M2
No keyring yet (M3). The app already has connections as fixtures; M2 adds a
minimal **password prompt** (reuse the `Modal` + `TextInput`, masked) shown when
a connection is opened, OR a hardcoded test password read from an env var for the
dev loop. **Recommended:** a small password `Modal` so the connect→auth path is
exercised the way it will be in M3 (M3 just swaps the source from "typed now" to
"keyring lookup, prompt on miss"). Password is held only in memory and passed in
`Command::Connect { profile, password }`; it is **never** logged or stored.

### D5 — Dependency versions
Add to `[workspace.dependencies]` and consult the crates' own examples before
writing against them (the API is the real risk):

| Dep | Where | Notes |
|---|---|---|
| `russh` | `nyx-protocol` | SSH transport + client `Handler`. Pin a current minor (e.g. `0.5x`); check the `client` example for the connect/auth shape. |
| `russh-sftp` | `nyx-protocol` | SFTP subsystem over a russh channel; `SftpSession::read_dir`. |
| `futures` | `nyx-service` (+ app drains the stream) | `channel::mpsc` for the Event stream (D1). `default-features` minimal if possible. |
| `tokio` | `nyx-protocol` | needs `net`/`io-util`/`sync` features for russh; the service already enables `rt-multi-thread,sync,time,macros`. |

`tracing-subscriber` (M1 row in the master plan) is **app-edge log init** — wire
it here if not already, so the bridge and connect path log through `tracing`
(never credentials).

### D6 — Connection identity for the active session
M2 supports **one** live connection (the active one). The service owns a single
`Option<Box<dyn RemoteClient>>` (or `SftpClient`) for now; multi-session is out
of scope. `AppState.online_id` tracks which connection the single session belongs
to. Don't build a session map yet — it's premature until multi-tab connect lands
(post-MVP).

---

## Channel contract (the M2 expansion of `Command`/`Event`)

Add to `nyx-service` (keep `#[non_exhaustive]`; keep `Event: Clone` — so no
non-Clone payloads like senders go inside it):

```rust
pub enum Command {
    Connect { profile: nyx_core::Profile, password: String }, // password: never logged
    HostKeyDecision { accept: bool },                          // reply to HostKeyPrompt (D3)
    ListDir { path: String },
    Disconnect,
    Shutdown,                                                  // existing
}

pub enum Event {
    Ready,                                                     // existing
    Stopped,                                                   // existing
    Connecting { profile_id: String },                        // optional: drives a "connecting" hint
    HostKeyPrompt { host: String, fingerprint: String },
    Connected { profile_id: String },
    DirListing { path: String, entries: Vec<nyx_core::RemoteEntry> },
    Error { message: String },                                // human-safe, credential-free
}
```

Notes:
- `Command` must `Debug` **without** leaking the password — either don't derive
  `Debug` field-wise (custom impl redacting `password`) or wrap the secret in a
  `Redacted(String)` newtype whose `Debug` prints `"***"`. Do this *before* the
  first `tracing` call touches a `Command`.
- `Error.message` is the `Display` of a `NyxError`, which is already
  credential-free by design (`NyxError::Auth` is just `"authentication failed"`).
  Audit that the russh→NyxError mapping never interpolates a secret.
- `DirListing` echoes `path` so a late listing for an old `cwd` can be dropped by
  the UI (the user may have navigated away). Compare against `AppState.cwd`.

---

## The async bridge (establish once, reuse everywhere)

In the app, own the service for the process lifetime and drain its events into
`AppState`:

1. **Hold the handle.** `main.rs` calls `nyx_service::spawn()` and stores the
   `ServiceHandle` somewhere with app lifetime — simplest is a field on
   `AppState` (`service: ServiceHandle`), constructed in `AppState::new`. (The
   handle's `Drop` already requests `Shutdown` + joins.) Add `nyx-service` to
   `crates/nyx/Cargo.toml` (the app currently does **not** depend on it).
2. **Drain events.** In `AppState::new`, take the `UnboundedReceiver<Event>` and
   start a long-lived drain task:
   ```rust
   cx.spawn(async move |this, cx| {
       while let Some(event) = events.next().await {
           if this.update(cx, |state, cx| state.apply_event(event, cx)).is_err() {
               break; // entity gone → app closing
           }
       }
   }).detach();
   ```
   This runs on the **foreground** executor; `next().await` yields, never blocks.
   The UI redraws because `apply_event` calls `cx.notify()` where state changed.
3. **`apply_event`** is the single match that turns events into state mutations:
   - `Connected { profile_id }` → set `online_id`, `view = Browse`, clear any
     "connecting" flag; then issue the first `ListDir` for the root.
   - `DirListing { path, entries }` → if `path` matches the current `cwd`, map
     `entries` into `Vec<EntryRow>` and store in `listing`; drop stale paths.
   - `HostKeyPrompt { host, fingerprint }` → open the host-key `Modal` (store the
     prompt details on `AppState`).
   - `Error { message }` → toast it (`ToastVariant::Danger`); clear connecting
     state; if it aborted a connect, return to `Welcome`.
   - `Connecting` → set a flag for a spinner/overlay (optional in M2; full
     connecting overlay is M6).
4. **Send commands** from `AppState` handlers via `self.service.send(Command::…)`.
   `open_connection` becomes: collect password (D4) → `send(Connect { profile,
   password })` → set a connecting flag (don't flip to `Browse` yet; wait for
   `Connected`). Navigation (`open_dir`, `go_up`, breadcrumb, `refresh`,
   `back`/`forward`) sends `ListDir { path }` instead of calling
   `fake_listing` — but keeps the same `cwd`/`history` bookkeeping.

**Ordering guarantee:** events are processed in send order on a single drain
task, so a `Connected` always precedes its first `DirListing`. The UI guards
stale listings by `path`. No locks, no shared mutable state across threads —
only the two channels.

### What stays on fixtures in M2
Only the **active** connection lists for real. Transfers (`fake_transfers`),
sidebar/welcome connection *data* (still fixtures until M3), and the
density/perms tweaks are untouched. The fixture `fake_listing` can stay in the
tree (used by tests / offline runs) but is no longer called on the live path.

---

## `SftpClient` implementation (`nyx-protocol`)

Fill in `connect` + `list_dir` (the rest stay `unimplemented!()` until M4).
Hold real state on the struct:

```rust
pub struct SftpClient {
    profile: nyx_core::Profile,          // host, port, username, protocol
    handle: Option<russh::client::Handle<ClientHandler>>, // the SSH session
    sftp: Option<russh_sftp::client::SftpSession>,        // the SFTP subsystem
    // host-key plumbing: known_hosts path + the oneshot wiring for D3
}
```

### `connect` (password auth + host-key)
1. TCP connect to `host:port` (`port` from profile or `Protocol::default_port`).
2. `russh::client::connect(config, addr, handler)` with a `ClientHandler` that
   implements `check_server_key` per **D2/D3** (compute fingerprint, check
   `known_hosts`, prompt via oneshot on unknown, reject on mismatch).
3. `session.authenticate_password(username, password)` → on failure map to
   `NyxError::Auth` (**never** echo the password or username into the error).
4. Open a channel, `request_subsystem(true, "sftp")`, wrap with
   `SftpSession::new(channel.into_stream()).await` → store in `self.sftp`.
5. The connect *task in the service* emits `Connected { profile_id }` on success;
   errors propagate as `NyxError` → `Event::Error`.

The `ClientHandler` needs a clone of the Event sender (to emit `HostKeyPrompt`)
and a handle to receive the user's decision — wire these when the service spawns
the connect task, not inside `SftpClient::new`.

### `list_dir`
`self.sftp.read_dir(path)` → iterate entries; for each build a `RemoteEntry`:
- `name` = file name (final component).
- `size` = metadata size (`0` for dirs).
- `kind`/`is_dir` = from the entry's file type (`Directory`/`File`/`Symlink`/
  `Other`); `is_dir` mirrors `kind == Directory`.
- `modified` = mtime → `SystemTime` (`None` if absent).
- `perms` = render the unix mode to the `"rwxr-xr-x"` string the table shows.
Map `russh-sftp` errors (no-such-file → `NyxError::NotFound`, permission/other →
`Io`/`Other`).

### Error mapping (audit for secrets)
Centralize a `fn map_err(e: russh::Error|russh_sftp::Error) -> NyxError`:
- connection/handshake/transport → `Connection`
- auth rejected → `Auth` (drop any server-provided detail that could echo input)
- host-key mismatch/reject → `HostKey`
- sftp no-such-file → `NotFound`; other sftp/io → `Io`
- anything else → `Other`
**Every** arm must produce a credential-free `String`. Add a unit test asserting
the auth/host-key messages contain neither the password nor username sentinel.

---

## Host-key store (`known_hosts`)

Minimal for M2 (no need for a new crate):
- File path: app config dir + `known_hosts` (M2 may hardcode a path under the
  user config dir; M3's `directories` wiring replaces the literal). Create on
  first trust.
- Format: one line per host — `host fingerprint` (the SHA-256 base64 fingerprint
  russh exposes). Simple and human-inspectable; matches the prototype's TOFU
  story. (Full OpenSSH `known_hosts` compatibility is **not** required for V1.)
- Operations: `lookup(host) -> Option<Fingerprint>`, `trust(host, fingerprint)`
  (append), and the compare logic (match / unknown / mismatch) from D2.
Keep this in `nyx-protocol` (it's protocol-adjacent and credential-free) or a
tiny module in the service; do **not** put it in the keyring (it's not a secret).

---

## Browser wiring (replace the fixture source)

No new browser UI — only swap where `listing` comes from:
- **Opening a connection** (sidebar row / welcome card) → password prompt (D4) →
  `Connect`. On `Connected`, the drain task issues the first `ListDir` for the
  profile's `remote_path` (or `/`).
- **Double-click a dir / breadcrumb / up / back / forward / refresh** → keep the
  existing `cwd`/`history` updates, but instead of `reload_listing()` (fixtures)
  send `ListDir { path: cwd.join("/") }`. The incoming `DirListing` repopulates
  `listing`; `visible_entries()` (sort+filter) is unchanged.
- **Stale-listing guard:** drop a `DirListing` whose `path` ≠ current `cwd`.
- **Loading state:** between issuing `ListDir` and receiving `DirListing`, show a
  lightweight "loading…" hint (a row of placeholder text or reuse
  `ProgressBar::indeterminate`). A full spinner/connecting overlay is M6 (G6 from
  M1 was deferred); a minimal hint here is enough.
- **Disconnect** (tab close / status-bar action) → `Command::Disconnect`, clear
  `online_id`, back to `Welcome`.

---

## File changes (by crate)

```
crates/nyx-service/
  Cargo.toml          + futures (Event stream); russh stays out (it's in nyx-protocol)
  src/lib.rs          expand Command/Event (redacted Debug for password); spawn() returns
                      UnboundedReceiver<Event>; command loop: hold one Option<SftpClient>,
                      handle Connect/ListDir/HostKeyDecision/Disconnect; spawn the connect
                      task; wire the host-key oneshot; emit Connecting/Connected/DirListing/
                      HostKeyPrompt/Error.
crates/nyx-protocol/
  Cargo.toml          + russh, russh-sftp, tokio (net,io-util,sync)
  src/sftp.rs         implement connect + list_dir; ClientHandler (check_server_key);
                      russh→NyxError mapping; perms/mtime/kind conversion helpers
  src/known_hosts.rs  (new) TOFU store: lookup/trust/compare
crates/nyx/
  Cargo.toml          + nyx-service
  src/main.rs         init tracing-subscriber (edge); (handle lives on AppState)
  src/state/mod.rs    hold ServiceHandle + drain task; apply_event(); open_connection →
                      Connect; navigation → ListDir; disconnect → Disconnect; host-key
                      modal state + decision senders; connecting/error handling
  src/app.rs          render the host-key Modal + password Modal; loading hint in browser
  src/views/browser.rs   (only if a loading hint needs a hook)
```

---

## Tasks

### 1. Channel contract & dependencies
- [ ] Add `russh`, `russh-sftp` to `[workspace.dependencies]`; add `futures`
      (Event stream). Wire them into the two crate manifests (D5). Add
      `nyx-service` to `crates/nyx/Cargo.toml`.
- [ ] Expand `Command`/`Event` in `nyx-service` per the contract above. Implement
      a **redacting `Debug`** for `Command::Connect` so the password can never be
      logged. Switch `spawn()` to return `futures::channel::mpsc::UnboundedReceiver<Event>`.

### 2. Service command loop
- [ ] Replace the trivial loop with a real dispatcher holding one
      `Option<SftpClient>` (D6). Handle `Connect` (spawn connect task),
      `HostKeyDecision` (resolve the pending oneshot), `ListDir` (call `list_dir`,
      emit `DirListing`), `Disconnect` (drop the session), `Shutdown` (existing).
- [ ] Emit `Connecting`/`Connected`/`DirListing`/`HostKeyPrompt`/`Error` at the
      right points. Ensure send-order = the bridge's processing order.

### 3. `SftpClient` (connect + list)
- [ ] Implement `connect`: TCP → `russh::client::connect` with `ClientHandler` →
      `authenticate_password` → open channel → `request_subsystem("sftp")` →
      `SftpSession::new`. Store handle + session.
- [ ] Implement `check_server_key` (D2/D3): fingerprint, `known_hosts` compare,
      prompt-and-await on unknown, reject on mismatch.
- [ ] Implement `list_dir`: `read_dir` → `Vec<RemoteEntry>` with correct
      kind/size/modified/perms conversion.
- [ ] `known_hosts.rs`: lookup / trust / compare; create file on first trust.
- [ ] `map_err`: russh/russh-sftp → `NyxError`, every arm credential-free. Unit
      test that auth/host-key messages contain no secret.

### 4. The async bridge (app)
- [ ] `AppState::new` constructs `nyx_service::spawn()`, stores `ServiceHandle`,
      and starts the `cx.spawn` drain task over the Event stream.
- [ ] Implement `apply_event` for every variant (connecting / host-key / connected
      / dir-listing / error), calling `cx.notify()` on change and guarding stale
      `DirListing` by `path`.
- [ ] Rewire `open_connection` → password prompt (D4) → `Connect`; flip to
      `Browse` only on `Connected`. Rewire navigation getters to send `ListDir`
      instead of `fake_listing`. `disconnect()` → `Command::Disconnect`.

### 5. Host-key & password modals (UI)
- [ ] Host-key `Modal`: host + fingerprint + Trust/Reject; buttons send
      `Command::HostKeyDecision`. Mismatch case shows a clear danger message and
      no Trust button (auto-reject path).
- [ ] Password `Modal` (masked `TextInput`): collect the secret for `Connect`;
      never echo it to logs/state beyond the in-flight command.
- [ ] Loading hint in the browser between `ListDir` and `DirListing`.

### 6. Errors, logging, cleanliness
- [ ] Init `tracing-subscriber` at the app edge; confirm **no credential** ever
      reaches a log line (grep the connect/auth path; the redacting `Debug`
      backstops it).
- [ ] Error toasts on connect/auth/host-key/list failure; connecting state clears
      and the app returns to `Welcome` on a failed connect.
- [ ] No raw hex in app code; `nyx-ui` still has **zero `nyx-*` deps** (the modals
      use existing components only). `cargo fmt --all`; `cargo clippy --workspace
      --all-targets -- -D warnings`.

---

## Build-verification loop

`cargo clippy --workspace --all-targets -- -D warnings`, then run the real path
against a **test SFTP server**. Two cheap options for the dev loop:
- A local `sftpgo`/OpenSSH `sshd` with a throwaway user, or
- A Docker `atmoz/sftp` container (`docker run -p 2222:22 atmoz/sftp user:pass:::upload`).

Manual acceptance script:
1. First connect → **host-key `Modal` appears**; Trust → browser shows the live
   `/` listing.
2. Double-click into a dir / breadcrumb / up / refresh → each lists for real; the
   UI never freezes (type in the filter box mid-list to confirm responsiveness).
3. Wrong password → auth error toast, back to Welcome, **no secret in logs**.
4. Reconnect → **no** host-key prompt (now in `known_hosts`).
5. Flip the server's host key (or edit `known_hosts`) → **mismatch is rejected**,
   not prompted.

(Build runs need `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer` — see
the build note in repo memory.)

---

## Risks / watch-items

- **`russh` API surface** is the headline risk — front-loaded here on purpose.
  Build connect+list before touching anything else; lean on the `russh` `client`
  example and `russh-sftp` `read_dir` example. Budget time for the `Handler`
  trait and the channel→sftp wrapping; that's where the API is least obvious.
- **Host-key handler awaiting the UI (D3)** — an async callback that must block on
  a human is the subtlest piece. Get the oneshot wiring right (exactly one
  pending decision in M2) and make sure a closed/rejected prompt resolves the
  future so the connect task never hangs.
- **Bridge correctness** — single drain task = in-order processing; the only
  shared state is the two channels. Guard stale `DirListing` by `path`; make the
  drain loop exit cleanly when the entity is gone (closing window).
- **Never log credentials** — redacting `Debug` on `Connect`, audited `map_err`,
  and a grep of the connect path. Treat a credential in a log as a release
  blocker, not a nit.
- **Don't over-build** — one session, read-only, no queue, no persistence. Resist
  adding a session map, retry/reconnect, or transfer plumbing; those are M4/M5
  and dilute the risk-retirement focus.

## Definition of done (M2)

- Against a real test SFTP server, opening a connection prompts for the host key
  on first connect (TOFU `Modal`), then the browser shows the **live** remote
  listing of the root.
- Navigating (open dir / up / back / forward / breadcrumb / refresh) lists real
  directories; sort and filter work on the live data unchanged from M1.
- The UI **never blocks** on the backend — connecting and listing happen off the
  GPUI thread; the bridge applies events via `cx.spawn`/`cx.update`.
- Host-key mismatch is rejected (not prompted); a wrong password surfaces an auth
  toast and returns to Welcome.
- No credential appears in any log line or error string; `nyx-ui` keeps zero
  `nyx-*` deps; `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D
  warnings` are clean.

## Hand-off to M3

M2 leaves these seams for profiles + keyring:
- The **password source** is a prompt (D4). M3 replaces it with a `nyx-keyring`
  lookup (`(service="nyx", account=profile id)`), prompting only on a miss, and
  writing on save from the connection editor.
- Connections are still **fixtures**. M3's `FileProfileStore` (TOML via
  `directories`) replaces `fake_connections()`; sidebar/welcome read the real
  store; the connection editor `Modal` does CRUD and a **Test-connection** button
  reuses M2's `Connect` + a probe.
- The `known_hosts` path is hardcoded in M2 (D2/host-key store). M3's
  `directories` wiring moves it to the proper per-OS data dir.
- The `Connect`/`ListDir`/`Disconnect` commands and the bridge are **frozen**;
  M4 (file ops) and M5 (transfer queue) only add `Command`/`Event` variants and
  fill the remaining `RemoteClient` methods on the session M2 established.
