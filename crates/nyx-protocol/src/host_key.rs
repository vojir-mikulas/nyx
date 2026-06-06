// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! The host-key prompt callback.
//!
//! When an SFTP connection meets a host it has never seen, the protocol layer
//! must ask *someone* whether to trust the presented key (trust-on-first-use).
//! That decision lives in the UI, but the protocol layer must not know about the
//! UI — so it depends only on this small async trait. The service implements it
//! by surfacing a modal and awaiting the user's choice; tests can implement it as
//! a constant.

use async_trait::async_trait;

/// Asks whether to trust an unknown host key.
///
/// Implementations are consulted from inside the SSH handshake (the russh
/// host-key callback), so the method is async — it may block on user input. It is
/// called **only** for unknown hosts; a recorded match is trusted automatically
/// and a mismatch is rejected without prompting.
#[async_trait]
pub trait HostKeyPrompt: Send + Sync {
    /// Return `true` to trust (and persist) `fingerprint` for `host`, `false` to
    /// abort the connection. `fingerprint` is the SHA-256 form, e.g. `SHA256:…`.
    async fn confirm_unknown(&self, host: &str, fingerprint: &str) -> bool;
}
