//! The drag out of the window: a native drag session that carries the item's
//! file URL and offers both move and copy, so the destination decides as it
//! does for a drag of its own (Finder and Files: move within a volume, copy to
//! another, keys to force). A drop back in this window arrives as the
//! webview's drop event, which the frontend handles. The approach follows the
//! `drag` crate (CrabNebula, MIT or Apache-2.0); its single-operation mask
//! made the webview, which takes drops as copies, refuse a move in its own
//! window, and Finder could never move with a copy-only one.
//!
//! Everything here runs on the main thread.

pub use imp::{forced_keys, start};

#[cfg(target_os = "macos")]
mod imp {
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;

    use objc2::rc::Retained;
    use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSApp, NSDragOperation, NSDraggingContext, NSDraggingFormation, NSDraggingItem, NSDraggingSession, NSDraggingSource, NSEvent,
        NSEventModifierFlags, NSEventType, NSView, NSWorkspace,
    };
    use objc2_foundation::{NSArray, NSPoint, NSRect, NSSize, NSString, NSURL};

    use crate::dnd::Forced;

    /// Side of the drag image, in points.
    const ICON: f64 = 64.0;

    type Done = Box<dyn FnOnce(bool)>;

    struct SourceIvars {
        done: Cell<Option<Done>>,
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "GitDriveFilesDragSource"]
        #[ivars = SourceIvars]
        struct Source;

        unsafe impl NSObjectProtocol for Source {}

        unsafe impl NSDraggingSource for Source {
            #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
            fn operation_mask(&self, _session: &NSDraggingSession, _context: NSDraggingContext) -> NSDragOperation {
                NSDragOperation::Copy | NSDragOperation::Move | NSDragOperation::Generic
            }

            #[unsafe(method(draggingSession:endedAtPoint:operation:))]
            fn ended(&self, _session: &NSDraggingSession, _point: NSPoint, operation: NSDragOperation) {
                if let Some(done) = self.ivars().done.take() {
                    done(operation != NSDragOperation::None);
                }
            }
        }
    );

    impl Source {
        fn new(done: Done, mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(SourceIvars { done: Cell::new(Some(done)) });
            unsafe { msg_send![super(this), init] }
        }
    }

    thread_local! {
        // The latest drag's source, kept alive until the next drag replaces
        // it: the session's end calls into it, and dropping it from inside
        // that call would free the object while it runs.
        static LAST_SOURCE: RefCell<Option<Retained<Source>>> = const { RefCell::new(None) };
    }

    /// Starts dragging `paths` from the pointer; `done` learns whether something took them.
    pub fn start(window: &tauri::WebviewWindow, paths: &[PathBuf], done: impl FnOnce(bool) + 'static) -> Result<(), String> {
        let mtm = MainThreadMarker::new().ok_or("The drag must start on the main thread.")?;
        let view = window.ns_view().map_err(|e| e.to_string())?;
        // SAFETY: Tauri hands the window's live NSView, and this runs on the main thread.
        let view = unsafe { &*(view as *const NSView) };
        let ns_window = view.window().ok_or("The window has no NSWindow.")?;
        let content = ns_window.contentView().ok_or("The window has no content view.")?;
        let at = ns_window.mouseLocationOutsideOfEventStream();

        let workspace = NSWorkspace::sharedWorkspace();
        let mut items = Vec::with_capacity(paths.len());
        for (i, path) in paths.iter().enumerate() {
            let path_str = NSString::from_str(&path.to_string_lossy());
            let icon = workspace.iconForFile(&path_str);
            icon.setSize(NSSize::new(ICON, ICON));
            let url = NSURL::fileURLWithPath_isDirectory(&path_str, path.is_dir());
            let item = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), &ProtocolObject::from_retained(url));
            // A small pile under the pointer: each item a few points off the one before.
            let off = i.min(4) as f64 * 4.0;
            let frame = NSRect::new(NSPoint::new(at.x - ICON / 2.0 + off, at.y - ICON / 2.0 - off), NSSize::new(ICON, ICON));
            // SAFETY: the contents is an NSImage, the type AppKit expects there.
            unsafe { item.setDraggingFrame_contents(frame, Some(&icon)) };
            items.push(item);
        }
        let several = items.len() > 1;
        let items = NSArray::from_retained_slice(&items);

        // The session starts from a drag event at the pointer: the mouse
        // events that led here went to the webview and are gone.
        let timestamp = NSApp(mtm).currentEvent().map_or(0.0, |e| e.timestamp());
        let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            NSEventType::LeftMouseDragged,
            at,
            NSEventModifierFlags::empty(),
            timestamp,
            ns_window.windowNumber(),
            None,
            0,
            1,
            1.0,
        )
        .ok_or_else(|| t!("drag.start_failed"))?;

        let source = Source::new(Box::new(done), mtm);
        let session = content.beginDraggingSessionWithItems_event_source(&items, &event, ProtocolObject::from_ref(&*source));
        if several {
            session.setDraggingFormation(NSDraggingFormation::Stack);
        }
        LAST_SOURCE.with(|last| *last.borrow_mut() = Some(source));
        Ok(())
    }

    /// Option copies and Cmd moves, as in Finder.
    pub fn forced_keys(_window: &tauri::WebviewWindow) -> Forced {
        let flags = NSEvent::modifierFlags_class();
        Forced { copy: flags.contains(NSEventModifierFlags::Option), moves: flags.contains(NSEventModifierFlags::Command) }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    use gtk::prelude::*;
    use gtk::{gdk, gio, glib};

    use crate::dnd::Forced;

    /// Starts dragging `paths` from the pointer; `done` learns whether something took them.
    pub fn start(window: &tauri::WebviewWindow, paths: &[PathBuf], done: impl FnOnce(bool) + 'static) -> Result<(), String> {
        let win = window.gtk_window().map_err(|e| e.to_string())?;
        let first = paths.first().ok_or_else(|| t!("drag.nothing"))?;
        // Encoded URIs: a name with spaces or accents must reach Files whole.
        let uris = paths
            .iter()
            .map(|p| glib::filename_to_uri(p, None).map(|u| u.to_string()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let targets = gtk::TargetList::new(&[]);
        targets.add_uri_targets(0);

        let handlers = Rc::new(RefCell::new(Vec::new()));
        handlers.borrow_mut().push(win.connect_drag_data_get(move |_, _, data, _, _| {
            data.set_uris(&uris.iter().map(String::as_str).collect::<Vec<_>>());
        }));
        let done = RefCell::new(Some(done));
        let own = handlers.clone();
        handlers.borrow_mut().push(win.connect_drag_end(move |w, ctx| {
            if let Some(done) = done.borrow_mut().take() {
                done(ctx.drag_drop_succeeded());
            }
            // Disconnecting during the emission is safe: GLib frees the closure after it returns.
            for id in own.borrow_mut().drain(..) {
                w.disconnect(id);
            }
        }));

        match win.drag_begin_with_coordinates(&targets, gdk::DragAction::COPY | gdk::DragAction::MOVE, 1, None, -1, -1) {
            Some(ctx) => {
                ctx.drag_set_icon_name(&icon_name(first), 0, 0);
                Ok(())
            }
            None => {
                for id in handlers.borrow_mut().drain(..) {
                    win.disconnect(id);
                }
                Err(t!("drag.start_failed"))
            }
        }
    }

    /// The icon theme's name for the item: a folder, or the generic icon of its type.
    fn icon_name(path: &Path) -> String {
        if path.is_dir() {
            return "folder".into();
        }
        let (content_type, _) = gio::content_type_guess(Some(path), &[]);
        gio::content_type_get_generic_icon_name(&content_type).map_or_else(|| "text-x-generic".into(), |n| n.to_string())
    }

    /// Ctrl copies and Shift moves, as in Files.
    pub fn forced_keys(window: &tauri::WebviewWindow) -> Forced {
        let state = window
            .gtk_window()
            .ok()
            .and_then(|w| gdk::Keymap::for_display(&w.display()))
            .map_or(gdk::ModifierType::empty(), |k| gdk::ModifierType::from_bits_truncate(k.modifier_state()));
        Forced { copy: state.contains(gdk::ModifierType::CONTROL_MASK), moves: state.contains(gdk::ModifierType::SHIFT_MASK) }
    }
}
