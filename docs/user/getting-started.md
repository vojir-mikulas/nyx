# Getting started

This walks you through creating a connection, signing in, and moving files.

## 1. Create a connection

Press **⌘N** (Ctrl+N) or click **New connection**. Fill in the editor:

| Field | Notes |
|---|---|
| **Name** | A label for your own reference (e.g. "Production web"). Required. |
| **Protocol** | **SFTP**, **FTP**, or **FTPS**. |
| **Encryption** | FTPS only — **Explicit** (default, upgrades port 21 with `AUTH TLS`) or **Implicit** (TLS from the start, usually port 990). |
| **Host** | Hostname or IP of the server. Required. |
| **Port** | Auto-fills with the protocol default (SFTP 22, FTP/FTPS 21). Change it only if your server uses a non-standard port. |
| **Username** | Your login name. Required (except for Anonymous FTP). |
| **Remote path** | Optional — a folder to open automatically on connect, e.g. `/var/www/app`. |
| **Accent color** | A color tag to tell connections apart in the sidebar. |
| **Authentication** | See below. |

> ⚠️ **Plain FTP is unencrypted.** Your username, password, and files cross the
> network in the clear. Prefer **SFTP** or **FTPS** whenever the server supports
> it.

### Authentication methods

- **Password** — works with all protocols. Stored in your OS keychain, never on
  disk in plain text.
- **Key** (SFTP only) — public-key authentication. Point **Private key** at your
  key file (e.g. `~/.ssh/id_ed25519`) using the **Browse** button. If the key is
  encrypted, enter its **Passphrase**; Nyx stores the passphrase in the keychain
  so you only type it once. Ed25519, RSA, and ECDSA keys in OpenSSH format are
  supported.
- **Anonymous** (FTP / FTPS only) — logs in as the `anonymous` user with no
  password. The username and password fields disappear when you pick this.

### Test before saving

Click **Test connection** to verify your settings against the live server before
committing. You'll see a spinner, then a checkmark or an error. When it's good,
click **Save**.

## 2. Trust the server (first connection)

The first time you connect to an SFTP host (or an FTPS server with a
self-signed/private certificate), Nyx shows the server's **SHA-256 fingerprint**
and asks whether to trust it. This is **trust-on-first-use**:

- **Trust** records the fingerprint so future connections are silent.
- If the fingerprint **changes** later, Nyx refuses to connect and warns you —
  this protects against a server being impersonated. (See
  [Troubleshooting](troubleshooting.md) if you hit this legitimately, e.g. after
  a server rebuild.)

Verify the fingerprint against what your server admin or provider published
before trusting it.

## 3. Browse

After connecting, the main area lists the remote folder. Navigate with:

- **Enter** / double-click — open a folder, or download a file.
- **Backspace** — go up one level. The breadcrumb in the toolbar also works.
- **Arrow keys**, **Home/End** — move the selection; **⌘A** selects everything.

### Filtering large folders

Press **⌘F** to focus the filter box. Beyond plain substring search, it accepts:

- Globs: `*.rs`, `log_*`
- Predicates: `type:dir`, `type:file`, `ext:png`, `size:>1M`, `size:<100k`,
  `modified:<7d`
- `"quoted text"` for a case-sensitive exact match
- A leading `/` to search recursively through subfolders

Very large directories (tens of thousands of entries) render smoothly; you'll get
a heads-up in the status bar for exceptionally large listings.

## 4. Transfer files

- **Download** — select one or more files and press **Enter**, or **drag them out
  of the Nyx window into Finder**; they download to wherever you drop them.
- **Upload** — drag files or folders **from Finder onto a folder** in Nyx.
- **Move (remote → remote)** — drag a selection onto another folder in the same
  connection; Nyx moves it server-side.

Folders transfer recursively as a **single** queued item with one aggregate
progress bar (not one entry per file). Symlinks inside a folder are skipped and
counted in the result.

### The transfer queue

Active transfers appear in the dock with filename, progress, speed, and time
remaining. SFTP runs up to **3** transfers at once; FTP and FTPS run **one at a
time**.

- Transfers are **atomic**: Nyx writes to a temporary `.nyxpart` file and renames
  it on success, so a cancelled or interrupted transfer never leaves a corrupt
  file in place of a good one.
- If a file already exists at the destination, Nyx asks whether to **Overwrite**,
  **Skip**, or **Cancel** — with an "apply to all" option for multi-file
  transfers. It never overwrites silently.
- If a transfer hits per-file errors (e.g. permission denied on a few files in a
  big folder), Nyx continues with the rest and shows a "N failed / N skipped"
  summary you can expand for details.

### If the connection drops

Nyx automatically reconnects (with backoff) when a connection is lost mid-session
and shows a "Reconnecting…" banner. SFTP transfers that were in flight **resume
from where they left off** rather than restarting — provided the source file
hasn't changed. If too many reconnect attempts fail, you'll get a manual
**Reconnect** button.

## 5. File operations

| Operation | How |
|---|---|
| Rename | Select, press **F2** |
| Delete | Select, press **Del** (asks for confirmation) |
| Copy remote path | Select, press **⌘C** |
| Refresh listing | **⌘R** |
| New folder | From the toolbar / context menu |

Unix permissions are shown as an `rwxr-xr-x`-style string for each entry.
