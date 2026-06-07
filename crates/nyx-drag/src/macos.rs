//! macOS drag-out via promised files.
//!
//! We vend one [`NSFilePromiseProvider`] per file through the GPUI `NSView`
//! (reached via the window handle) and begin a dragging session anchored to the
//! current `NSEvent`. The provider holds a **weak** reference to its delegate, so
//! the delegates are kept alive for the session's lifetime by the dragging
//! *source* (which AppKit retains until the drag ends).
//!
//! The promise resolver ([`PromiseDelegate`]) returns a background
//! `NSOperationQueue`, so the OS calls `writePromiseToURL:` off the main thread —
//! there [`DragFetch::fetch`] can block on the download without freezing the UI.

use std::path::PathBuf;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession,
    NSDraggingSource, NSFilePromiseProvider, NSFilePromiseProviderDelegate, NSImage,
    NSImageNameMultipleDocuments, NSView,
};
use objc2_foundation::{
    NSArray, NSError, NSOperationQueue, NSPoint, NSRect, NSSize, NSString, NSURL,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::{DragError, DragFetch, DragFile, DragIcon, DragSession};

/// Instance data for one file's promise resolver.
struct PromiseIvars {
    file: DragFile,
    fetch: Arc<dyn DragFetch>,
    /// The background queue `writePromiseToURL:` runs on (so the blocking
    /// download never lands on the main thread).
    queue: Retained<NSOperationQueue>,
}

define_class!(
    // SAFETY:
    // - The superclass NSObject has no subclassing requirements.
    // - `PromiseDelegate` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[ivars = PromiseIvars]
    struct PromiseDelegate;

    unsafe impl NSObjectProtocol for PromiseDelegate {}

    // SAFETY: the selectors and signatures match `NSFilePromiseProviderDelegate`.
    // The `MainThreadMarker` parameters in the protocol declaration are an objc2
    // fiction (never part of the real Objective-C selector), so they are omitted
    // here; AppKit invokes `fileNameForType:`/`operationQueueFor…:` on the main
    // thread and `writePromiseToURL:` on our background queue.
    unsafe impl NSFilePromiseProviderDelegate for PromiseDelegate {
        #[unsafe(method_id(filePromiseProvider:fileNameForType:))]
        fn file_name_for_type(
            &self,
            _provider: &NSFilePromiseProvider,
            _file_type: &NSString,
        ) -> Retained<NSString> {
            NSString::from_str(&self.ivars().file.name)
        }

        #[unsafe(method(filePromiseProvider:writePromiseToURL:completionHandler:))]
        fn write_promise_to_url(
            &self,
            _provider: &NSFilePromiseProvider,
            url: &NSURL,
            completion_handler: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            let ivars = self.ivars();
            let result = match url.path() {
                Some(path) => {
                    let dest = PathBuf::from(path.to_string());
                    ivars.fetch.fetch(&ivars.file, &dest)
                }
                None => Err(DragError::fetch("drop target has no file path")),
            };
            match result {
                // Success: pass a null error pointer.
                Ok(()) => completion_handler.call((core::ptr::null_mut(),)),
                // Failure: hand the OS a generic, credential-free error so it
                // discards the (partial) file. The detail stays internal.
                Err(_) => {
                    let domain = NSString::from_str("dev.nyx.drag");
                    // SAFETY: `domain` is a valid NSString; user-info omitted.
                    let err = unsafe { NSError::errorWithDomain_code_userInfo(&domain, 1, None) };
                    completion_handler.call((Retained::as_ptr(&err) as *mut NSError,));
                }
            }
        }

        #[unsafe(method_id(operationQueueForFilePromiseProvider:))]
        fn operation_queue(&self, _provider: &NSFilePromiseProvider) -> Retained<NSOperationQueue> {
            self.ivars().queue.clone()
        }
    }
);

impl PromiseDelegate {
    fn new(file: DragFile, fetch: Arc<dyn DragFetch>) -> Retained<Self> {
        let queue = NSOperationQueue::new();
        let this = Self::alloc().set_ivars(PromiseIvars { file, fetch, queue });
        unsafe { msg_send![super(this), init] }
    }
}

/// Instance data for the dragging source. It exists only to keep the per-file
/// delegates alive for the duration of the drag: the providers hold weak
/// references to them, but AppKit retains the *source* until the session ends.
struct SourceIvars {
    #[allow(dead_code)]
    delegates: Vec<Retained<PromiseDelegate>>,
}

define_class!(
    // SAFETY:
    // - The superclass NSObject has no subclassing requirements.
    // - `DragSource` does not implement `Drop`.
    // - `NSDraggingSource` requires `MainThreadOnly`, which we declare.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = SourceIvars]
    struct DragSource;

    unsafe impl NSObjectProtocol for DragSource {}

    // SAFETY: the selector and signature match `NSDraggingSource`.
    unsafe impl NSDraggingSource for DragSource {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn source_operation_mask(
            &self,
            _session: &NSDraggingSession,
            _context: NSDraggingContext,
        ) -> NSDragOperation {
            // A drag onto the Finder/desktop is a copy (download), never a move.
            NSDragOperation::Copy
        }
    }
);

impl DragSource {
    fn new(mtm: MainThreadMarker, delegates: Vec<Retained<PromiseDelegate>>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(SourceIvars { delegates });
        unsafe { msg_send![super(this), init] }
    }
}

/// A generic multi-document icon shown under the cursor. Custom per-file previews
/// are a later polish step (the `DragIcon` pixels are not yet rendered).
fn drag_image() -> Option<Retained<NSImage>> {
    // SAFETY: reading the framework's named-image global.
    let name = unsafe { NSImageNameMultipleDocuments };
    NSImage::imageNamed(name)
}

pub fn start_file_drag(
    window: &impl HasWindowHandle,
    files: Vec<DragFile>,
    fetch: Arc<dyn DragFetch>,
    icon: Option<DragIcon>,
) -> Result<DragSession, DragError> {
    let _ = icon; // reserved for a future custom preview

    let mtm = MainThreadMarker::new().ok_or(DragError::NotMainThread)?;

    let handle = window
        .window_handle()
        .map_err(|err| DragError::Window(err.to_string()))?;
    let view_ptr = match handle.as_raw() {
        RawWindowHandle::AppKit(h) => h.ns_view.as_ptr().cast::<NSView>(),
        other => return Err(DragError::Window(format!("unexpected handle: {other:?}"))),
    };
    // SAFETY: GPUI keeps the `NSView` alive for the window's lifetime, and we are
    // on the main thread (we hold a `MainThreadMarker`) for the whole call.
    let view: &NSView = unsafe { &*view_ptr };

    // The drag must be anchored to the event currently being processed (the
    // mouse-down/-drag that triggered this handler).
    let app = NSApplication::sharedApplication(mtm);
    let event = app.currentEvent().ok_or(DragError::NoEvent)?;

    let image = drag_image();
    let mut items: Vec<Retained<NSDraggingItem>> = Vec::with_capacity(files.len());
    let mut delegates: Vec<Retained<PromiseDelegate>> = Vec::with_capacity(files.len());
    for (i, file) in files.into_iter().enumerate() {
        // `public.folder` makes the OS create a directory at the drop URL (which
        // `writePromiseToURL:` then fills recursively); `public.data` advertises a
        // generic byte stream for a file. The real name comes from the delegate's
        // `fileNameForType:`.
        let uti_str = if file.is_dir {
            "public.folder"
        } else {
            "public.data"
        };
        let delegate = PromiseDelegate::new(file, fetch.clone());
        let uti = NSString::from_str(uti_str);
        let provider = NSFilePromiseProvider::initWithFileType_delegate(
            NSFilePromiseProvider::alloc(),
            &uti,
            ProtocolObject::from_ref(&*delegate),
        );
        let item = NSDraggingItem::initWithPasteboardWriter(
            NSDraggingItem::alloc(),
            ProtocolObject::from_ref(&*provider),
        );
        // Stagger multi-file frames so they don't fully overlap under the cursor.
        let offset = 16.0 * i as f64;
        let frame = NSRect::new(NSPoint::new(offset, offset), NSSize::new(64.0, 64.0));
        match image.as_ref() {
            Some(image) => {
                let contents: &AnyObject = (**image).as_ref();
                // SAFETY: `contents` is a valid `NSImage`.
                unsafe { item.setDraggingFrame_contents(frame, Some(contents)) };
            }
            None => item.setDraggingFrame(frame),
        }
        items.push(item);
        delegates.push(delegate);
    }

    let source = DragSource::new(mtm, delegates);
    let items_array = NSArray::from_retained_slice(&items);
    let _session = view.beginDraggingSessionWithItems_event_source(
        &items_array,
        &event,
        ProtocolObject::from_ref(&*source),
    );

    Ok(DragSession {})
}
