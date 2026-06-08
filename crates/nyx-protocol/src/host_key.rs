//! The server-identity trust prompt callback.
//!
//! When a connection meets a server it has never seen, the protocol layer must
//! ask *someone* whether to trust the presented identity (trust-on-first-use) -
//! an SSH host key for SFTP, a TLS certificate for FTPS. That decision lives in
//! the UI, but the protocol layer must not know about the UI - so it depends only
//! on this small async trait. The service implements it by surfacing a modal and
//! awaiting the user's choice; tests can implement it as a constant.

use async_trait::async_trait;
use nyx_core::ServerTrustKind;

/// Asks whether to trust an unknown server identity (host key or certificate).
///
/// Implementations may block on user input, so the method is async. It is called
/// **only** for unknown identities; a recorded match is trusted automatically and
/// a mismatch is rejected without prompting. `kind` lets the UI word the prompt
/// correctly per protocol.
#[async_trait]
pub trait ServerTrustPrompt: Send + Sync {
    /// Return `true` to trust (and persist) `fingerprint` for `host`, `false` to
    /// abort the connection. `fingerprint` is the SHA-256 form (`SHA256:…` for a
    /// host key, a hex digest for a certificate).
    async fn confirm_unknown(&self, host: &str, fingerprint: &str, kind: ServerTrustKind) -> bool;
}
