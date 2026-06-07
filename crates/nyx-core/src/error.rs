//! The crate-wide error type.
//!
//! Libraries return [`NyxError`] (built with `thiserror`); the application edges
//! may wrap these in `anyhow` for context.

use thiserror::Error;

/// Convenience alias used throughout the Nyx crates.
pub type Result<T> = std::result::Result<T, NyxError>;

/// Errors surfaced by the Nyx backend and protocol layers.
///
/// Variants carry human-readable detail as `String` rather than nesting foreign
/// error types, keeping `nyx-core` free of protocol/runtime dependencies. Crates
/// that produce these map their own errors in.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum NyxError {
    /// Could not establish or maintain a connection to the remote host.
    #[error("connection error: {0}")]
    Connection(String),

    /// An established connection's transport died mid-session (network drop, VPN
    /// flap, server restart, sleep/wake). Distinct from [`Connection`](Self::Connection)
    /// (a failed *attempt*) so the service can flip the session to "lost" and the
    /// UI can offer a reconnect rather than treat it as a generic op failure.
    #[error("connection lost: {0}")]
    ConnectionLost(String),

    /// Authentication was rejected by the remote host.
    ///
    /// Never embed the offending credential in this (or any) message.
    #[error("authentication failed")]
    Auth,

    /// Host-key verification failed or the key is untrusted.
    #[error("host key verification failed: {0}")]
    HostKey(String),

    /// A private key file is encrypted and the supplied passphrase was missing
    /// or wrong. Distinct from [`Auth`](Self::Auth) so the UI can re-prompt for
    /// the passphrase rather than report a server rejection. Never carries the
    /// passphrase or any key material.
    #[error("key requires a passphrase")]
    KeyLocked,

    /// A filesystem / transport I/O error occurred.
    #[error("i/o error: {0}")]
    Io(String),

    /// The requested path or resource does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// The operation is not supported by this protocol implementation.
    #[error("operation not supported")]
    Unsupported,

    /// A transfer was cancelled by the user.
    #[error("transfer cancelled")]
    Cancelled,

    /// Anything that does not fit a more specific variant.
    #[error("{0}")]
    Other(String),
}
