#!/usr/bin/env bash
#
# Sign a locally-built binary with a STABLE code-signing identity, then exec it.
#
# Why: macOS ties a Keychain item's "Always Allow" ACL to the accessing app's
# code signature. Ad-hoc / unsigned binaries (what `cargo build` produces) have
# a signature derived from the binary's *hash*, so every rebuild looks like a
# different app and the Keychain re-prompts. Signing each build with the SAME
# real identity + the SAME bundle identifier gives a stable "designated
# requirement", so "Always Allow" persists across rebuilds.
#
# This is wired in as the cargo `runner` for the macOS host targets (see
# .cargo/config.toml), so `cargo run -p nyx` / `just run` launch a stably-signed
# binary transparently.
#
# One-time setup — create a free self-signed code-signing cert:
#   Keychain Access -> Certificate Assistant -> Create a Certificate...
#   Name: Nyx Dev   Identity Type: Self Signed Root   Certificate Type: Code Signing
#
# Override the identity (e.g. to use a Developer ID) with NYX_SIGN_IDENTITY.
set -euo pipefail

IDENTITY="${NYX_SIGN_IDENTITY:-Nyx Dev}"
IDENTIFIER="${NYX_BUNDLE_ID:-com.vojir.nyx}"

BIN="$1"
shift

if security find-identity -v -p codesigning 2>/dev/null | grep -qF "$IDENTITY"; then
  codesign --force --sign "$IDENTITY" --identifier "$IDENTIFIER" "$BIN" >/dev/null 2>&1 \
    || echo "nyx: codesign with '$IDENTITY' failed; launching unsigned (Keychain may re-prompt)." >&2
else
  echo "nyx: code-signing identity '$IDENTITY' not found; launching unsigned (Keychain will re-prompt)." >&2
  echo "     Create it once in Keychain Access > Certificate Assistant > Create a Certificate (type: Code Signing, name: Nyx Dev)." >&2
fi

exec "$BIN" "$@"
