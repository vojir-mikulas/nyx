//! `nyx-drag` - drag files **out** of the app and into the OS file manager.
//!
//! GPUI handles drag-*in* (`ExternalPaths` drops) but has no drag-*source* API,
//! so this crate is the platform adapter for the export direction. It is
//! deliberately **domain- and UI-agnostic**: it speaks window handles and file
//! promises, never SFTP or app state, so the "no `nyx-*` in the UI layer" spirit
//! holds and a future native GPUI drag-source can slot in *behind*
//! [`start_file_drag`] without touching the app.
//!
//! The hard part is that our files aren't local: they live on a remote server
//! and don't exist on disk when the drag begins. We use **promised files** - the
//! drag starts instantly, and the OS calls back at drop time with the
//! destination, at which point [`DragFetch::fetch`] streams the bytes there. The
//! caller wires `fetch` to its normal download pipeline; the OS-supplied
//! destination is just the download's local path.
//!
//! Today the only real backend is macOS (`NSFilePromiseProvider`); other
//! platforms return [`DragError::Unsupported`]. See
//! `docs/plans/drag-out-to-desktop.md`.

use std::path::Path;
use std::sync::Arc;

use raw_window_handle::HasWindowHandle;

#[cfg(target_os = "macos")]
mod macos;

/// One file offered to the OS in a drag-out. `size`, when known, lets the
/// platform hint the file size to the drop target (purely advisory).
#[derive(Clone, Debug)]
pub struct DragFile {
    /// The destination file name (with extension) the OS will create.
    pub name: String,
    /// The file's size in bytes, if known.
    pub size: Option<u64>,
    /// Whether this promise is a directory: the platform advertises a folder type
    /// so the OS creates a directory at the drop URL for [`DragFetch::fetch`] to
    /// fill recursively.
    pub is_dir: bool,
}

impl DragFile {
    /// A file with an unknown size.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            size: None,
            is_dir: false,
        }
    }
}

/// A drag image (RGBA, premultiplied) shown under the cursor during the drag.
///
/// Reserved for the polish phase: the current macOS backend uses a generic
/// system document icon and ignores any supplied pixels. Kept in the signature
/// so adding a real preview later is not a breaking change.
#[derive(Clone, Debug)]
pub struct DragIcon {
    /// Row-major RGBA8 pixels, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
}

/// Resolves a promised file's bytes at drop time.
///
/// The OS calls [`fetch`](DragFetch::fetch) when (and only when) the user drops
/// onto a target. The implementation must write `file`'s bytes to `dest` and
/// **block until done**, returning `Err` to cancel that one item. It runs on an
/// arbitrary OS thread (never the UI thread), so it must be `Send + Sync` and
/// must not touch UI state directly.
pub trait DragFetch: Send + Sync + 'static {
    /// Write `file` to `dest`, blocking until the bytes are on disk.
    fn fetch(&self, file: &DragFile, dest: &Path) -> Result<(), DragError>;
}

/// A live (or just-started) drag session.
///
/// On macOS the OS owns the drag once it begins, so this is a marker that the
/// session was launched; dropping it does not cancel the drag.
#[derive(Debug)]
#[non_exhaustive]
pub struct DragSession {}

/// Where and how an OS drag-out finished, reported back to the caller so it can
/// decide what a drop landing **back inside** the originating window means (e.g.
/// an in-app move instead of a drop-to-local).
#[derive(Clone, Copy, Debug)]
pub struct DragEnd {
    /// The drop point in the originating window's GPUI coordinate space (logical
    /// pixels, top-left origin), or `None` if it couldn't be mapped.
    pub local: Option<(f32, f32)>,
    /// Whether an external target accepted the drop - a real drop-to-local. When
    /// `false`, the gesture ended without any target taking the files (commonly a
    /// release back inside the app window).
    pub accepted: bool,
}

/// Invoked once, on the UI thread, when the drag session ends. Used to detect a
/// drop back inside the originating window. Not `Send`: it fires on the main
/// thread within AppKit's drag machinery.
pub type DragEndCallback = Box<dyn FnOnce(DragEnd) + 'static>;

/// Invoked on the UI thread as the drag moves, with the cursor in the
/// originating window's GPUI coordinates (logical pixels, top-left origin), or
/// `None` when off-window/unmappable. Lets the caller show feedback (e.g.
/// highlight the folder under the cursor) while the native drag is back inside.
pub type DragMoveCallback = Box<dyn Fn(Option<(f32, f32)>) + 'static>;

/// Optional feedback hooks for a drag session. Both fire on the UI thread.
#[derive(Default)]
pub struct DragHandlers {
    /// Called once when the gesture ends; see [`DragEnd`].
    pub on_end: Option<DragEndCallback>,
    /// Called as the cursor moves; see [`DragMoveCallback`].
    pub on_move: Option<DragMoveCallback>,
}

/// Why a drag-out could not start, or a promised fetch failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DragError {
    /// This platform has no drag-out backend.
    #[error("drag-out is not supported on this platform")]
    Unsupported,
    /// `files` was empty - nothing to drag.
    #[error("no files to drag")]
    NoFiles,
    /// Not called on the platform's main/UI thread.
    #[error("drag-out must be started on the main thread")]
    NotMainThread,
    /// No active input event to anchor the drag session to (the OS needs the
    /// originating mouse event).
    #[error("no active mouse event to anchor the drag")]
    NoEvent,
    /// The window handle was missing or of an unexpected kind.
    #[error("invalid window handle: {0}")]
    Window(String),
    /// A promised [`DragFetch::fetch`] failed (credential-free message).
    #[error("{0}")]
    Fetch(String),
}

impl DragError {
    /// Build a [`DragError::Fetch`] from any message.
    pub fn fetch(message: impl Into<String>) -> Self {
        Self::Fetch(message.into())
    }
}

/// Start an OS drag session for `files`, anchored to `window`.
///
/// Returns immediately (the OS drives the drag and calls `fetch` lazily at drop
/// time). `icon` is an optional drag preview (currently advisory on macOS).
/// `handlers` carries optional UI-thread feedback hooks (drag end / drag move) -
/// letting the caller turn a drop back inside the window into an in-app action
/// and show feedback while the drag is inside.
///
/// Must be called on the main/UI thread from within an active mouse handler, so
/// the platform can anchor the drag to the originating event.
pub fn start_file_drag(
    window: &impl HasWindowHandle,
    files: Vec<DragFile>,
    fetch: Arc<dyn DragFetch>,
    icon: Option<DragIcon>,
    handlers: DragHandlers,
) -> Result<DragSession, DragError> {
    if files.is_empty() {
        return Err(DragError::NoFiles);
    }
    #[cfg(target_os = "macos")]
    {
        macos::start_file_drag(window, files, fetch, icon, handlers)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (window, files, fetch, icon, handlers);
        Err(DragError::Unsupported)
    }
}
