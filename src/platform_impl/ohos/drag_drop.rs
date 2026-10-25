//! Owned drag payloads and rich drag-and-drop support for OpenHarmony.
//!
//! While `winit` emits `WindowEvent::HoveredFile`, `WindowEvent::DroppedFile`, and
//! `WindowEvent::HoveredFileCancelled` for dragged files, applications that also need
//! cursor coordinates or outgoing native file drag support can use the helpers in this module.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

use dpi::PhysicalPosition;
use openharmony_ability::{
    DragInputData, DragOperation, DragPhase, DragPreview, NativeDragResult, NativeFileDrag,
    OpenHarmonyApp,
};

use crate::event::WindowEvent;
use crate::window::WindowId;

/// Data carried by an OpenHarmony drag-and-drop operation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DragData {
    /// File paths resolved from dragged `file://` URIs (or raw URIs when resolution fails).
    pub paths: Vec<PathBuf>,
    /// Raw `file://` URIs carried by the drag event.
    pub uris: Vec<String>,
}

/// Platform drag-and-drop lifecycle event with window and position context.
#[derive(Debug, Clone, PartialEq)]
pub enum DragDropEvent {
    /// A drag entered the window surface.
    Entered {
        window_id: WindowId,
        position: (f64, f64),
        data: DragData,
    },
    /// A drag moved over the window surface.
    Over {
        window_id: WindowId,
        position: (f64, f64),
    },
    /// Data was dropped onto the window surface.
    Dropped {
        window_id: WindowId,
        position: (f64, f64),
        data: DragData,
    },
    /// A drag left the window surface without dropping.
    Left {
        window_id: WindowId,
    },
    /// An outgoing native file drag completed with a drop.
    OutgoingDropped {
        window_id: WindowId,
        id: i64,
        operation: DragOperation,
    },
    /// An outgoing native file drag was cancelled or failed.
    OutgoingCanceled {
        window_id: WindowId,
        id: i64,
    },
}

type DragDropCallback = Box<dyn Fn(DragDropEvent) + Send + Sync>;

static DRAG_DROP_CALLBACK: Mutex<Option<DragDropCallback>> = Mutex::new(None);

/// Register a global callback invoked for OpenHarmony drag-and-drop events.
///
/// Registering a new callback replaces any previously registered callback.
pub fn register_drag_drop_callback<F>(callback: F)
where
    F: Fn(DragDropEvent) + Send + Sync + 'static,
{
    if let Ok(mut slot) = DRAG_DROP_CALLBACK.lock() {
        *slot = Some(Box::new(callback));
    }
}

/// Clear the registered drag-and-drop callback.
pub fn clear_drag_drop_callback() {
    if let Ok(mut slot) = DRAG_DROP_CALLBACK.lock() {
        *slot = None;
    }
}

pub(crate) fn emit_drag_drop_event(event: DragDropEvent) {
    if let Ok(slot) = DRAG_DROP_CALLBACK.lock() {
        if let Some(cb) = slot.as_ref() {
            cb(event);
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct DragDrop {
    active: bool,
    files: Vec<PathBuf>,
}

impl DragDrop {
    pub fn event(
        &mut self,
        window: WindowId,
        drag: &DragInputData,
        offset: PhysicalPosition<i32>,
    ) -> Vec<WindowEvent> {
        let position = (
            f64::from(drag.window_x) - f64::from(offset.x),
            f64::from(drag.window_y) - f64::from(offset.y),
        );
        let resolve_paths = |uris: &[String]| -> Vec<PathBuf> {
            uris.iter()
                .map(|uri| file_path(uri).unwrap_or_else(|| PathBuf::from(uri)))
                .collect()
        };

        match drag.phase {
            DragPhase::Enter => {
                self.active = true;
                drag.response.accept();
                let paths = resolve_paths(&drag.file_uris);
                self.files = paths.clone();
                emit_drag_drop_event(DragDropEvent::Entered {
                    window_id: window,
                    position,
                    data: DragData {
                        paths: paths.clone(),
                        uris: drag.file_uris.clone(),
                    },
                });
                paths.into_iter().map(WindowEvent::HoveredFile).collect()
            }
            DragPhase::Move => {
                if self.active {
                    drag.response.accept();
                    emit_drag_drop_event(DragDropEvent::Over {
                        window_id: window,
                        position,
                    });
                }
                Vec::new()
            }
            DragPhase::Drop => {
                if !self.active {
                    return Vec::new();
                }
                self.active = false;
                drag.response.accept();
                let mut paths = resolve_paths(&drag.file_uris);
                if paths.is_empty() {
                    paths = std::mem::take(&mut self.files);
                } else {
                    self.files.clear();
                }
                emit_drag_drop_event(DragDropEvent::Dropped {
                    window_id: window,
                    position,
                    data: DragData {
                        paths: paths.clone(),
                        uris: drag.file_uris.clone(),
                    },
                });
                paths.into_iter().map(WindowEvent::DroppedFile).collect()
            }
            DragPhase::Leave => {
                if !self.active {
                    return Vec::new();
                }
                self.active = false;
                self.files.clear();
                emit_drag_drop_event(DragDropEvent::Left { window_id: window });
                vec![WindowEvent::HoveredFileCancelled]
            }
        }
    }
}

/// Convert OHOS provider URIs, including bundle authorities, to accessible paths.
pub fn file_path(uri: &str) -> Option<PathBuf> {
    if !uri.starts_with("file://") {
        return None;
    }
    ohos_fileuri_binding::get_path_from_uri(uri)
        .ok()
        .map(Into::into)
}

/// Encode an accessible local path for a native file drag or clipboard record.
pub fn file_uri(path: &Path) -> Option<String> {
    ohos_fileuri_binding::get_uri_from_path(path.to_str()?).ok()
}

// Native drag actions and their callbacks belong to the Ability's UI thread.
// Keep them outside Send/Sync window handles and release before surface destruction.
thread_local! {
    #[allow(clippy::missing_const_for_thread_local)]
    static OUTGOING: std::cell::RefCell<Option<Outgoing>> = const { std::cell::RefCell::new(None) };
    #[allow(clippy::missing_const_for_thread_local)]
    static POINTER: std::cell::Cell<(WindowId, i32)> = const { std::cell::Cell::new((WindowId::dummy(), 0)) };
}

struct Outgoing {
    id: i64,
    source: WindowId,
    session: Option<NativeFileDrag>,
    failed: bool,
}

fn next_id() -> i64 {
    static NEXT: AtomicI64 = AtomicI64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

pub(crate) fn set_pointer(window: WindowId, pointer: Option<i32>) {
    POINTER.set((
        window,
        pointer.filter(|id| (0..=9).contains(id)).unwrap_or(0),
    ));
}

/// Start a native file drag outside the input callback.
pub fn start_file_drag(
    app: &OpenHarmonyApp,
    source: WindowId,
    uris: Vec<String>,
) -> Result<i64, String> {
    start_file_drag_with_options(app, source, uris, None)
}

/// Start a native file drag with an optional preview image.
pub fn start_file_drag_with_options(
    app: &OpenHarmonyApp,
    source: WindowId,
    uris: Vec<String>,
    preview: Option<DragPreview>,
) -> Result<i64, String> {
    let scheduler = app.main_thread().map_err(|e| e.to_string())?;
    let raw_window_id = u64::from(source) as i64;
    if uris.is_empty()
        || uris.len() > 512
        || uris.iter().any(|uri| !uri.starts_with("file://"))
        || app.native_window_for(raw_window_id).is_none()
    {
        return Err("Invalid file drag URIs or unavailable native window".into());
    }
    let id = next_id();
    OUTGOING.with_borrow_mut(|outgoing| -> Result<(), String> {
        if outgoing.is_some() {
            return Err("Another outgoing file drag is already active".into());
        }
        *outgoing = Some(Outgoing {
            id,
            source,
            session: None,
            failed: false,
        });
        Ok(())
    })?;
    let (window, pointer) = POINTER.get();
    let pointer = if window == source { pointer } else { 0 };
    let app = app.clone();
    let files: Vec<_> = uris
        .into_iter()
        .map(|uri| {
            let directory = file_path(&uri).is_some_and(|path| path.is_dir());
            (uri, directory)
        })
        .collect();
    let dispatched = scheduler.dispatch(move || {
        if !OUTGOING.with_borrow(|outgoing| outgoing.as_ref().is_some_and(|drag| drag.id == id)) {
            return;
        }
        let session = app
            .with_xcomponent_for(raw_window_id, NativeFileDrag::node_handle)
            .map(|node| {
                NativeFileDrag::start_with_preview(
                    node,
                    pointer,
                    &files,
                    preview,
                    app.create_waker(),
                )
            });
        OUTGOING.with_borrow_mut(|outgoing| {
            if let Some(outgoing) = outgoing.as_mut().filter(|drag| drag.id == id) {
                match session {
                    Some(Ok(session)) => {
                        tracing::info!("OHOS native file drag started for {source:?}");
                        outgoing.session = Some(session);
                    }
                    error => {
                        if let Some(Err(error)) = error {
                            tracing::warn!("OHOS file drag: {error}");
                        }
                        outgoing.failed = true;
                    }
                }
            }
        });
        super::frame::main_wake(&app).notify();
    });
    if dispatched.is_err() {
        cancel_outgoing(Some(source));
        return Err("Failed to dispatch file drag to main thread".into());
    }
    Ok(id)
}

pub(crate) fn drain_outgoing() {
    let Some(outgoing) = OUTGOING.with_borrow_mut(|outgoing| {
        if outgoing.as_ref().is_some_and(|drag| {
            drag.failed || drag.session.as_ref().is_some_and(|session| session.ended())
        }) {
            outgoing.take()
        } else {
            None
        }
    }) else {
        return;
    };
    let event = match outgoing
        .session
        .as_ref()
        .and_then(|session| session.result())
    {
        Some(NativeDragResult::Dropped(operation)) if !outgoing.failed => {
            DragDropEvent::OutgoingDropped {
                window_id: outgoing.source,
                id: outgoing.id,
                operation,
            }
        }
        _ => DragDropEvent::OutgoingCanceled {
            window_id: outgoing.source,
            id: outgoing.id,
        },
    };
    tracing::info!(
        "OHOS native file drag completed for {:?}: {event:?}",
        outgoing.source
    );
    emit_drag_drop_event(event);
}

pub(crate) fn cancel_outgoing(window: Option<WindowId>) {
    let old = OUTGOING.with_borrow_mut(|outgoing| {
        if outgoing
            .as_ref()
            .is_some_and(|drag| window.is_none_or(|window| drag.source == window))
        {
            outgoing.take()
        } else {
            None
        }
    });
    drop(old);
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;

    #[test]
    fn drag_drop_callback_receives_and_clears_events() {
        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = count.clone();
        register_drag_drop_callback(move |event| {
            if matches!(event, DragDropEvent::Left { .. }) {
                count_clone.fetch_add(1, Ordering::SeqCst);
            }
        });

        emit_drag_drop_event(DragDropEvent::Left { window_id: WindowId::dummy() });
        assert_eq!(count.load(Ordering::SeqCst), 1);

        clear_drag_drop_callback();
        emit_drag_drop_event(DragDropEvent::Left { window_id: WindowId::dummy() });
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
