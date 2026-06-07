# Installing & first launch

## Requirements

- **macOS** is the primary, fully tested platform. Windows and Linux builds
  exist but are still maturing.
- A GPU-capable machine — Nyx renders its UI on the GPU (no web stack).

## Install on macOS

1. Download `Nyx-<version>.dmg`.
2. Open the `.dmg` and drag **Nyx** into your **Applications** folder.
3. Launch it.

### "Nyx can't be opened" — getting past Gatekeeper

The current builds are **ad-hoc signed**, not signed with an Apple Developer ID.
macOS Gatekeeper will refuse the first launch with a message like *"Nyx can't be
opened because Apple cannot check it for malicious software."* This is expected —
it does **not** mean anything is wrong with the download.

To open it the first time, do **one** of the following:

- **Right-click → Open.** In Finder, right-click (or Control-click) `Nyx.app`,
  choose **Open**, then confirm **Open** in the dialog. macOS remembers the
  choice, so future launches are normal.
- **Or remove the quarantine flag** from a terminal:

  ```sh
  xattr -dr com.apple.quarantine /Applications/Nyx.app
  ```

You only need to do this once per install.

## First launch

Nyx opens to a welcome screen with no connections yet. Press **⌘N** (or the
**New connection** button) to create your first one — see
[Getting started](getting-started.md).

The first time you connect to a server, macOS may show a **Keychain access**
prompt — Nyx uses the system keychain to store your passwords and key
passphrases. Allow it so you don't have to re-enter credentials each time.
