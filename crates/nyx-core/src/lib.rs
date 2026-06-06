//! Shared domain types for Nyx.
//!
//! This crate has **no UI and no runtime knowledge** — it is the common
//! vocabulary (errors, remote entries, the transfer model) that the protocol,
//! service and UI layers all speak. Keep it dependency-light and side-effect
//! free.

mod error;
mod path;
mod remote;
mod transfer;

pub use error::{NyxError, Result};
pub use path::{RemotePath, RemotePathError};
pub use remote::{EntryKind, Permissions, Protocol, RemoteEntry, Rwx};
pub use transfer::{Transfer, TransferDirection, TransferId, TransferProgress, TransferStatus};
