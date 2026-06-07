# Troubleshooting & FAQ

## "Nyx can't be opened" on macOS

The build is ad-hoc signed, so Gatekeeper blocks the first launch. Right-click the
app and choose **Open**, or run
`xattr -dr com.apple.quarantine /Applications/Nyx.app`. Full steps in
[Installing & first launch](install.md#nyx-cant-be-opened--getting-past-gatekeeper).

## I can't connect

Work through these in order:

1. **Test connection** in the profile editor — it isolates a config problem from
   a transfer problem and shows the specific error.
2. **Host / port** — confirm the address and that the port matches the protocol
   (SFTP 22, FTP/FTPS 21, implicit FTPS often 990). A non-standard SSH port is a
   common cause.
3. **Protocol** — SFTP (over SSH) and FTP/FTPS are different services. A server
   offering SSH on port 22 does not necessarily offer FTP, and vice versa.
4. **Firewall / VPN / network** — make sure the host is reachable from your
   network at all.

## "Authentication failed"

The server rejected your credentials. For security, Nyx doesn't show server-side
detail. Check:

- Username and password are correct for **that** server.
- For key auth, the **public** half of your key is installed in the server's
  `~/.ssh/authorized_keys`.
- The account is allowed to use the protocol you picked.

### Password vs. key

To **update a stored password or passphrase**, open the profile and type the new
value. Leaving the field **blank keeps** the previously stored secret — it does
not clear it.

## "Passphrase required" / encrypted key

If your private key file is encrypted, Nyx prompts for its passphrase (this is
separate from your account password). Enter it once and Nyx saves it to the
keychain. A wrong passphrase re-prompts rather than reporting an auth failure.

Supported key formats: OpenSSH **Ed25519**, **RSA**, and **ECDSA**.

## "Host key changed" / connection refused after it used to work

Nyx pins each server's fingerprint on first use. If the fingerprint later changes,
it refuses to connect — by design, since that can indicate a server being
impersonated.

If the change is **legitimate** (the server was rebuilt, migrated, or its key was
rotated), remove the stored fingerprint so Nyx re-prompts on the next connect.
The trust records live in Nyx's application-data folder:

- `known_hosts` — SFTP host-key fingerprints
- `known_certs` — FTPS certificate fingerprints

On macOS these are under
`~/Library/Application Support/dev.nyx.Nyx/`. Delete the line for the affected
host (or the whole file to reset all trust), then reconnect and verify the new
fingerprint before trusting it.

## A few files in a folder transfer failed

Folder transfers continue past individual errors and show a "N failed / N
skipped" summary — expand it to see which paths and why (often permissions).
Symlinks are skipped on purpose. Cancelled folder transfers are left **partial**
and clearly marked; Nyx never deletes a partially-transferred tree on your behalf.

## FTPS won't negotiate TLS

- **Explicit** mode connects in plaintext on port 21 and upgrades with `AUTH
  TLS`. **Implicit** mode is TLS from the first byte, usually on port 990. If one
  fails, the server may expect the other — switch the **Encryption** setting.
- Nyx forces both the control and data channels to be encrypted. A server
  misconfigured to allow only an unencrypted data channel will fail.

## Where is my data stored?

| What | Where |
|---|---|
| Passwords & key passphrases | **OS keychain** (service `nyx`) — never written to disk in plain text |
| Connection profiles | `profiles.toml` in Nyx's config folder (owner-only). Contains hosts, usernames, key file paths — **no secrets** |
| Trusted host keys / certs | `known_hosts`, `known_certs` in Nyx's data folder |

On macOS the config and data folders are both under
`~/Library/Application Support/dev.nyx.Nyx/`.

## How do I see all keyboard shortcuts?

Press **⌘/** (Ctrl+/) inside the app for a cheat-sheet that always matches the
current build.
