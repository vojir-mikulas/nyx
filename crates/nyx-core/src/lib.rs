// Copyright 2026 vojir-mikulas
// SPDX-License-Identifier: Apache-2.0

//! Shared domain types for Nyx.
//!
//! This crate has **no UI and no runtime knowledge** — it is the common
//! vocabulary (errors, remote entries, the transfer model) that the protocol,
//! service and UI layers all speak. Keep it dependency-light and side-effect
//! free.

mod error;
mod remote;
mod transfer;

pub use error::{NyxError, Result};
pub use remote::{EntryKind, Protocol, RemoteEntry};
pub use transfer::{Transfer, TransferDirection, TransferId, TransferStatus};
