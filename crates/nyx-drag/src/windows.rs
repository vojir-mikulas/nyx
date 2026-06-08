//! Windows drag-out via a virtual-file `IDataObject` with delayed rendering.
//!
//! We vend a single COM `IDataObject` that advertises `CFSTR_FILEDESCRIPTOR`
//! (one `FILEDESCRIPTORW` per [`DragFile`]) up front, and resolves
//! `CFSTR_FILECONTENTS` **lazily**: only when the drop target pulls a file's
//! bytes does the shell call `GetData(FILECONTENTS, lindex=i)`, at which point we
//! run [`DragFetch::fetch`] into a temp file and hand back an `IStream` over it.
//! The shell then copies that stream to the real drop location. This mirrors the
//! macOS `NSFilePromiseProvider` flow; the difference is the **staging hop**: on
//! macOS the OS gives us the final URL to write into, while on Windows we write a
//! temp file and the shell places it.
//!
//! Threading: `DoDragDrop` is a **blocking modal loop**. We run it on the calling
//! (UI) thread - the standard Win32 pattern, and the only way the cursor capture
//! and the UI-thread feedback hooks ([`DragHandlers`]) behave correctly. The
//! consequence is that the at-drop download happens inside `DoDragDrop` and
//! briefly blocks the UI thread; a future refinement could move `DoDragDrop` to a
//! dedicated STA thread and marshal the hooks back (see `windows-drag-out.md`).
//!
//! Folder contents are **not yet** delivered: a `FILEDESCRIPTORW` with
//! `FILE_ATTRIBUTE_DIRECTORY` is advertised so directories appear in the drop,
//! but Windows wants folder children flattened into the descriptor list (or an
//! `IStorage`), which needs a recursive remote listing the drag seam does not
//! provide. That is Phase 2 - see the plan. Files (single and multiple) are the
//! shipped path; a directory's `FILECONTENTS` pull returns `E_NOTIMPL`.

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::core::{implement, Result as WinResult, HRESULT, PCWSTR};
use windows::Win32::Foundation::{
    GlobalFree, BOOL, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS,
    DV_E_FORMATETC, E_FAIL, E_NOTIMPL, HGLOBAL, HWND, OLE_E_ADVISENOTSUPPORTED, POINT, S_FALSE,
    S_OK,
};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
use windows::Win32::System::Com::{
    IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA, IStream, DATADIR,
    DATADIR_GET, DVASPECT_CONTENT, FORMATETC, STGMEDIUM, STGMEDIUM_0, TYMED, TYMED_HGLOBAL,
    TYMED_ISTREAM,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{
    DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize, OleUninitialize, ReleaseStgMedium,
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{
    SHCreateStdEnumFmtEtc, SHCreateStreamOnFileEx, CFSTR_FILECONTENTS, CFSTR_FILEDESCRIPTORW,
    CFSTR_PERFORMEDDROPEFFECT, FD_ATTRIBUTES, FD_FILESIZE, FD_PROGRESSUI, FILEDESCRIPTORW,
};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use crate::{
    DragEnd, DragError, DragFetch, DragFile, DragHandlers, DragIcon, DragMoveCallback, DragSession,
};

/// Registered clipboard format ids for the formats we vend. Cached per call so we
/// register once and compare numerically in `GetData`/`QueryGetData`.
#[derive(Clone, Copy)]
struct Formats {
    descriptor: u16,
    contents: u16,
}

impl Formats {
    fn register() -> Self {
        // SAFETY: each arg is a valid wide, null-terminated framework string.
        unsafe {
            // Registering PERFORMEDDROPEFFECT makes the shell report the final
            // effect back via SetData; we don't read it, but register it so the
            // shell treats us as a well-behaved source.
            let _ = RegisterClipboardFormatW(CFSTR_PERFORMEDDROPEFFECT);
            Self {
                descriptor: RegisterClipboardFormatW(CFSTR_FILEDESCRIPTORW) as u16,
                contents: RegisterClipboardFormatW(CFSTR_FILECONTENTS) as u16,
            }
        }
    }
}

/// Process-unique counter so concurrent drags can't collide on a temp name.
static STAGE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Pick a fresh temp path to stage one file's download into.
fn stage_path(name: &str) -> PathBuf {
    let seq = STAGE_SEQ.fetch_add(1, Ordering::Relaxed);
    let safe: String = name
        .chars()
        .map(|c| if std::path::is_separator(c) { '_' } else { c })
        .collect();
    std::env::temp_dir().join(format!("nyx-drag-{}-{seq}-{safe}", std::process::id()))
}

/// The virtual-file data object handed to the shell. Holds the promised files and
/// the fetch callback; `staged` accumulates the temp files we created so the drag
/// can delete them once `DoDragDrop` returns.
#[implement(IDataObject)]
struct DataObject {
    files: Vec<DragFile>,
    fetch: Arc<dyn DragFetch>,
    formats: Formats,
    staged: Arc<Mutex<Vec<PathBuf>>>,
}

impl DataObject {
    /// Build the `FILEGROUPDESCRIPTORW` block as a moveable `HGLOBAL`.
    fn descriptor_global(&self) -> WinResult<HGLOBAL> {
        let n = self.files.len();
        // FILEGROUPDESCRIPTORW = { UINT cItems; FILEDESCRIPTORW fgd[1]; }, so the
        // total is a u32 count followed by a packed run of n descriptors.
        let size = std::mem::size_of::<u32>() + n * std::mem::size_of::<FILEDESCRIPTORW>();
        // SAFETY: allocate moveable memory, then lock to get a writable pointer.
        let hglobal = unsafe { GlobalAlloc(GMEM_MOVEABLE, size)? };
        // SAFETY: `hglobal` was just allocated with `size` bytes.
        let base = unsafe { GlobalLock(hglobal) };
        if base.is_null() {
            // SAFETY: `hglobal` is a valid handle we just allocated.
            unsafe {
                let _ = GlobalFree(hglobal);
            }
            return Err(E_FAIL.into());
        }
        // SAFETY: `base` points at `size` writable bytes; the layout matches
        // FILEGROUPDESCRIPTORW (count then a packed run of descriptors).
        unsafe {
            *(base as *mut u32) = n as u32;
            let fgd = (base as *mut u8).add(std::mem::size_of::<u32>()) as *mut FILEDESCRIPTORW;
            for (i, file) in self.files.iter().enumerate() {
                std::ptr::write(fgd.add(i), file_descriptor(file));
            }
            let _ = GlobalUnlock(hglobal);
        }
        Ok(hglobal)
    }

    /// Resolve the i-th file's bytes (download to a temp file) and wrap them in an
    /// `IStream` for `FILECONTENTS`.
    fn contents_stream(&self, index: usize) -> WinResult<IStream> {
        let file = self.files.get(index).ok_or(DV_E_FORMATETC)?;
        if file.is_dir {
            // Folder contents are Phase 2 (see module docs).
            return Err(E_NOTIMPL.into());
        }
        let dest = stage_path(&file.name);
        self.fetch.fetch(file, &dest).map_err(|_| E_FAIL)?;
        self.staged.lock().unwrap().push(dest.clone());

        let wide: Vec<u16> = dest
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // STGM_READ | STGM_SHARE_DENY_WRITE: read-only, stable while the shell
        // copies. Temp cleanup happens after DoDragDrop returns, not on release.
        const STGM_READ_SHARE: u32 = 0x0000_0020;
        // SAFETY: `wide` is a valid null-terminated path; no template stream.
        unsafe {
            SHCreateStreamOnFileEx(
                PCWSTR(wide.as_ptr()),
                STGM_READ_SHARE,
                FILE_ATTRIBUTE_NORMAL.0,
                false,
                None,
            )
        }
    }
}

/// One `FILEDESCRIPTORW` for a promised file: name, size hint, and directory flag.
fn file_descriptor(file: &DragFile) -> FILEDESCRIPTORW {
    let mut fd = FILEDESCRIPTORW::default();
    let mut flags = FD_PROGRESSUI.0 | FD_ATTRIBUTES.0;
    fd.dwFileAttributes = if file.is_dir {
        FILE_ATTRIBUTE_DIRECTORY.0
    } else {
        FILE_ATTRIBUTE_NORMAL.0
    };
    if let Some(size) = file.size {
        flags |= FD_FILESIZE.0;
        fd.nFileSizeHigh = (size >> 32) as u32;
        fd.nFileSizeLow = size as u32;
    }
    fd.dwFlags = flags as u32;
    // FILEDESCRIPTORW is packed, so build the name in an aligned local and assign
    // the whole array (a reference to the packed field would be UB).
    let name: Vec<u16> = file.name.encode_utf16().take(259).collect();
    let mut cfile = [0u16; 260];
    cfile[..name.len()].copy_from_slice(&name);
    fd.cFileName = cfile;
    fd
}

#[allow(non_snake_case)]
impl IDataObject_Impl for DataObject_Impl {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> WinResult<STGMEDIUM> {
        // SAFETY: the shell passes a valid, readable FORMATETC.
        let fe = unsafe { &*pformatetcin };
        if fe.cfFormat == self.formats.descriptor && (fe.tymed & TYMED_HGLOBAL.0 as u32) != 0 {
            let hglobal = self.descriptor_global()?;
            return Ok(STGMEDIUM {
                tymed: TYMED_HGLOBAL.0 as u32,
                u: STGMEDIUM_0 { hGlobal: hglobal },
                pUnkForRelease: ManuallyDrop::new(None),
            });
        }
        if fe.cfFormat == self.formats.contents && (fe.tymed & TYMED_ISTREAM.0 as u32) != 0 {
            let index = if fe.lindex < 0 { 0 } else { fe.lindex as usize };
            let stream = self.contents_stream(index)?;
            return Ok(STGMEDIUM {
                tymed: TYMED_ISTREAM.0 as u32,
                u: STGMEDIUM_0 {
                    pstm: ManuallyDrop::new(Some(stream)),
                },
                pUnkForRelease: ManuallyDrop::new(None),
            });
        }
        Err(DV_E_FORMATETC.into())
    }

    fn GetDataHere(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *mut STGMEDIUM,
    ) -> WinResult<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
        // SAFETY: the shell passes a valid, readable FORMATETC.
        let fe = unsafe { &*pformatetc };
        let descriptor =
            fe.cfFormat == self.formats.descriptor && (fe.tymed & TYMED_HGLOBAL.0 as u32) != 0;
        let contents =
            fe.cfFormat == self.formats.contents && (fe.tymed & TYMED_ISTREAM.0 as u32) != 0;
        if descriptor || contents {
            S_OK
        } else {
            S_FALSE
        }
    }

    fn GetCanonicalFormatEtc(
        &self,
        _pformatectin: *const FORMATETC,
        _pformatetcout: *mut FORMATETC,
    ) -> HRESULT {
        E_NOTIMPL
    }

    fn SetData(
        &self,
        _pformatetc: *const FORMATETC,
        pmedium: *const STGMEDIUM,
        frelease: BOOL,
    ) -> WinResult<()> {
        // We don't consume the shell's set formats (e.g. PERFORMEDDROPEFFECT), but
        // we must honor `fRelease` ownership.
        if frelease.as_bool() && !pmedium.is_null() {
            // SAFETY: the shell transfers ownership of a valid medium to us.
            unsafe { ReleaseStgMedium(pmedium as *mut STGMEDIUM) };
        }
        Ok(())
    }

    fn EnumFormatEtc(&self, dwdirection: u32) -> WinResult<IEnumFORMATETC> {
        if DATADIR(dwdirection as i32) != DATADIR_GET {
            return Err(E_NOTIMPL.into());
        }
        let formats = [
            format_etc(self.formats.descriptor, TYMED_HGLOBAL),
            format_etc(self.formats.contents, TYMED_ISTREAM),
        ];
        // SAFETY: `formats` is a valid slice the enumerator copies.
        unsafe { SHCreateStdEnumFmtEtc(&formats) }
    }

    fn DAdvise(
        &self,
        _pformatetc: *const FORMATETC,
        _advf: u32,
        _padvsink: Option<&IAdviseSink>,
    ) -> WinResult<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _dwconnection: u32) -> WinResult<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> WinResult<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

/// A `FORMATETC` for one advertised format (content aspect, all indices).
fn format_etc(cf: u16, tymed: TYMED) -> FORMATETC {
    FORMATETC {
        cfFormat: cf,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: tymed.0 as u32,
    }
}

/// The drop source: drives the standard `QueryContinueDrag`/`GiveFeedback` loop
/// and reports cursor movement to the UI via `on_move`.
#[implement(IDropSource)]
struct DropSource {
    hwnd: HWND,
    on_move: Option<DragMoveCallback>,
    scale: f32,
}

#[allow(non_snake_case)]
impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, fescapepressed: BOOL, grfkeystate: MODIFIERKEYS_FLAGS) -> HRESULT {
        if fescapepressed.as_bool() {
            DRAGDROP_S_CANCEL
        } else if grfkeystate.0 & MK_LBUTTON.0 == 0 {
            // Primary button released -> commit the drop.
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> HRESULT {
        if let Some(on_move) = self.on_move.as_ref() {
            on_move(cursor_in_window(self.hwnd, self.scale));
        }
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// The cursor position mapped into the window's GPUI coordinate space (logical
/// pixels, top-left origin), or `None` if it can't be mapped.
fn cursor_in_window(hwnd: HWND, scale: f32) -> Option<(f32, f32)> {
    let mut pt = POINT::default();
    // SAFETY: `pt` is a valid out-param.
    unsafe { GetCursorPos(&mut pt).ok()? };
    // SAFETY: `hwnd` is the live GPUI window; `pt` is converted in place.
    if !unsafe { ScreenToClient(hwnd, &mut pt) }.as_bool() {
        return None;
    }
    Some((pt.x as f32 / scale, pt.y as f32 / scale))
}

/// The window's DPI scale (logical px = physical px / scale).
fn window_scale(hwnd: HWND) -> f32 {
    // SAFETY: `hwnd` is the live GPUI window.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 {
        1.0
    } else {
        dpi as f32 / 96.0
    }
}

pub fn start_file_drag(
    window: &impl HasWindowHandle,
    files: Vec<DragFile>,
    fetch: Arc<dyn DragFetch>,
    icon: Option<DragIcon>,
    handlers: DragHandlers,
) -> Result<DragSession, DragError> {
    let _ = icon; // reserved for a future custom drag image

    let handle = window
        .window_handle()
        .map_err(|err| DragError::Window(err.to_string()))?;
    let hwnd = match handle.as_raw() {
        RawWindowHandle::Win32(h) => HWND(h.hwnd.get() as *mut c_void),
        other => return Err(DragError::Window(format!("unexpected handle: {other:?}"))),
    };

    // Drag-drop requires an OLE-initialized STA thread. Every successful
    // OleInitialize (S_OK *and* S_FALSE when already initialized) must be balanced
    // by OleUninitialize, so we pair it whenever the call succeeds.
    // SAFETY: called on the UI thread; balanced below.
    let ole_ok = unsafe { OleInitialize(None) }.is_ok();

    let staged: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
    let scale = window_scale(hwnd);

    let data: IDataObject = DataObject {
        files,
        fetch,
        formats: Formats::register(),
        staged: staged.clone(),
    }
    .into();
    let source: IDropSource = DropSource {
        hwnd,
        on_move: handlers.on_move,
        scale,
    }
    .into();

    let mut effect = DROPEFFECT_NONE;
    // SAFETY: both COM objects are valid; `DoDragDrop` runs its modal loop and
    // returns once the drop completes or is cancelled.
    let result = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect) };

    // The shell has finished copying staged files by the time DoDragDrop returns.
    for path in staged.lock().unwrap().drain(..) {
        let _ = std::fs::remove_file(path);
    }

    if let Some(on_end) = handlers.on_end {
        let accepted = result == DRAGDROP_S_DROP && effect != DROPEFFECT_NONE;
        on_end(DragEnd {
            local: cursor_in_window(hwnd, scale),
            accepted,
        });
    }

    if ole_ok {
        // SAFETY: balances our successful OleInitialize on this thread.
        unsafe { OleUninitialize() };
    }

    // `data`/`source` (and thus the COM objects) are released here at scope end.
    Ok(DragSession {})
}
