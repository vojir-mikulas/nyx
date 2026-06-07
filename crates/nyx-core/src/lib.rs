//! Shared domain types for Nyx.
//!
//! This crate has **no UI and no runtime knowledge** — it is the common
//! vocabulary (errors, remote entries, the transfer model) that the protocol,
//! service and UI layers all speak. Keep it dependency-light and side-effect
//! free.

mod error;
mod path;
mod remote;
mod secret;
mod transfer;

pub use error::{NyxError, Result};
pub use path::{is_safe_local_segment, RemotePath, RemotePathError};
pub use remote::{EntryKind, FtpsMode, Permissions, Protocol, RemoteEntry, Rwx, ServerTrustKind};
pub use secret::Secret;
pub use transfer::{
    CollisionChoice, EntryIssue, EntryOutcomeKind, SourceMeta, Transfer, TransferDirection,
    TransferId, TransferKind, TransferProgress, TransferReport, TransferStatus,
};
