#![cfg(target_env = "ohos")]

use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::{self, Display, Formatter};
use std::hash::Hash;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use dpi::{PhysicalPosition, PhysicalSize, Position, Size};
use openharmony_ability::arkui::arkui_input_binding::{
    UIInputAction, UIInputEvent, UIInputSourceType, UIInputToolType,
};
use openharmony_ability::ime::{Action as ImeAction, Direction as ImeDirection, KeyboardStatus};
use openharmony_ability::xcomponent::{
    Action, KeyCode as Keycode, MouseAction as OhosMouseAction, MouseButton as OhosMouseButton,
    MouseEventData, TouchEvent, TouchEventData, TouchPointData,
};
use openharmony_ability::{
    ArkUiInputEvent, ColorMode, Configuration, Event as MainEvent, GestureEvent,
    GesturePhase as OhosGesturePhase, ImeEvent, InputEvent, KeyboardEventData,
    KeyboardInputDelivery, OpenHarmonyApp, Rect, XComponentInputEvent,
};
use smol_str::SmolStr;
use tracing::{trace, warn};

use crate::cursor::{Cursor, CustomCursor, CustomCursorSource};
use crate::error::{self, EventLoopError};
use crate::event::{
    self, DeviceEvent, ElementState, Force, Ime, InnerSizeWriter, KeyEvent, Modifiers, MouseButton,
    MouseScrollDelta, RawKeyEvent, StartCause, Touch, TouchPhase,
};
use crate::event_loop::{self, ControlFlow, DeviceEvents};
use crate::keyboard::{Key, ModifiersKeys, ModifiersState, NamedKey};
use crate::platform::pump_events::PumpStatus;
use crate::platform_impl::Fullscreen;
use crate::window::{
    self, CursorGrabMode, ImePurpose, ResizeDirection, Theme, WindowAttributes, WindowButtons,
    WindowLevel,
};

mod control_flow;
mod cursor;
pub mod drag_drop;
pub mod events;
pub mod file_access;
mod frame;
mod frame_request;
mod geometry;
mod keycodes;
mod main_wake;
mod monitor;
pub mod plugins;
mod resize_settle;
#[allow(dead_code)]
mod safe_area;
mod stylus;
pub mod task;
mod timer;
mod window_lifecycle;
mod window_status;

pub(crate) use crate::cursor::{
    OnlyCursorImage as PlatformCustomCursor, OnlyCursorImageSource as PlatformCustomCursorSource,
};
pub(crate) use crate::icon::RgbaIcon as PlatformIcon;
pub use monitor::{MonitorHandle, VideoModeHandle};

/// Edits absent from winit's cross-platform IME event vocabulary.
#[derive(Debug, Clone)]
pub enum ImeEdit {
    Selection { start: usize, end: usize },
    DeleteSurrounding { before: usize, after: usize },
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct KeyEventExtra {
    pub text_with_all_modifiers: Option<SmolStr>,
    pub key_without_modifiers: Key,
}

#[derive(Default, Clone, Debug)]
pub struct OsError(Option<String>);

impl OsError {
    pub(crate) fn from_msg(msg: impl Into<String>) -> Self {
        Self(Some(msg.into()))
    }
}

impl Display for OsError {
    fn fmt(&self, fmt: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        match &self.0 {
            Some(msg) => write!(fmt, "OpenHarmony OS Error: {msg}"),
            None => write!(fmt, "OpenHarmony OS Error"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WindowId(pub(crate) u64);

impl WindowId {
    pub const fn dummy() -> Self {
        WindowId(u64::MAX)
    }

    pub(crate) const fn from_raw(id: usize) -> Self {
        WindowId(id as u64)
    }

    pub(crate) const fn into_raw(self) -> usize {
        self.0 as usize
    }
}

impl From<WindowId> for u64 {
    fn from(id: WindowId) -> Self {
        id.0
    }
}

impl From<u64> for WindowId {
    fn from(id: u64) -> Self {
        Self(id)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DeviceId(pub(crate) i32);

impl DeviceId {
    pub const fn dummy() -> Self {
        DeviceId(0)
    }

    pub(crate) fn from_raw(id: i64) -> Self {
        Self(id as i32)
    }
}

const GLOBAL_WINDOW: WindowId = WindowId::from_raw(0);
static EVENT_LOOP_CREATED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Default)]
struct InputState {
    pressed_fingers: HashSet<i32>,
    pending_touch: Option<TouchEventData>,
    pen_metadata: Option<openharmony_ability::StylusInputData>,
    mouse_inside: bool,
    last_mouse_position: Option<PhysicalPosition<f64>>,
    modifiers: ModifiersState,
    modifier_keys: ModifiersKeys,
}

#[derive(Debug)]
struct SharedWindowState {
    commands: task::CommandQueue,
    ready: Mutex<Option<futures_channel::oneshot::Sender<()>>>,
    inner: Mutex<WindowState>,
    frame: frame::FrameScheduler,
    geometry: geometry::Geometry<GeometryTarget>,
}

#[derive(Debug, Clone, Copy)]
struct GeometryTarget {
    position: PhysicalPosition<i32>,
    size: Option<PhysicalSize<u32>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CursorKey {
    Icon(i32),
    Custom(CustomCursor),
}

#[derive(Debug, Clone, Copy)]
struct WindowDrag {
    direction: Option<ResizeDirection>,
    pointer: PhysicalPosition<f64>,
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
}

#[derive(Debug)]
struct WindowState {
    lifecycle: window_lifecycle::WindowLifecycle,
    cursor_key: Option<CursorKey>,
    cursor_pending: Option<CursorKey>,
    cursor_generation: u64,
    surface_size: PhysicalSize<u32>,
    surface_position: PhysicalPosition<i32>,
    outer_size: PhysicalSize<u32>,
    position: PhysicalPosition<i32>,
    scale_factor: f64,
    focused: bool,
    visible: bool,
    theme: Option<Theme>,
    ime_allowed: bool,
    ime_purpose: ImePurpose,
    ime_cursor_area: Option<(Position, Size)>,
    ime_surrounding_text: Option<(String, usize, usize)>,
    title: String,
    resizable: bool,
    decorations: bool,
    enabled_buttons: WindowButtons,
    minimized: bool,
    maximized: bool,
    fullscreen: Option<Fullscreen>,
    min_size: Option<PhysicalSize<u32>>,
    max_size: Option<PhysicalSize<u32>>,
    real_window_id: Option<i32>,
    initial_attributes: Option<WindowAttributes>,
    redraw_pending: bool,
    resize_settle: resize_settle::ResizeSettle,
    pointer_position: PhysicalPosition<f64>,
    dragging: Option<WindowDrag>,
    resize_increments: Option<PhysicalSize<u32>>,
    ime_edits: Vec<ImeEdit>,
}

impl SharedWindowState {
    fn new(app: &OpenHarmonyApp, id: i64, visible: bool) -> Self {
        let content_rect = app.content_rect_for(id);
        let window_rect = app.window_rect_for(id);
        let surface_size = physical_size(content_rect.width, content_rect.height);
        let outer_size = physical_size(window_rect.width, window_rect.height);

        let commands = task::CommandQueue::default();
        let ready = if id == 0 {
            None
        } else {
            let (ready, wait) = futures_channel::oneshot::channel();
            commands.push(async move {
                let _ = wait.await;
            });
            Some(ready)
        };

        Self {
            commands,
            ready: Mutex::new(ready),
            frame: frame::FrameScheduler::new(id, frame::main_wake(app)),
            geometry: Default::default(),
            inner: Mutex::new(WindowState {
                lifecycle: Default::default(),
                cursor_key: None,
                cursor_pending: None,
                cursor_generation: 0,
                surface_size,
                surface_position: PhysicalPosition::new(content_rect.left, content_rect.top),
                outer_size: nonzero_size_or(outer_size, surface_size),
                position: PhysicalPosition::new(window_rect.left, window_rect.top),
                scale_factor: 1.0,
                focused: false,
                visible,
                theme: theme(app.config().color_mode),
                ime_allowed: false,
                ime_purpose: ImePurpose::Normal,
                ime_cursor_area: None,
                ime_surrounding_text: None,
                title: String::new(),
                resizable: true,
                decorations: true,
                enabled_buttons: WindowButtons::all(),
                minimized: false,
                maximized: false,
                fullscreen: None,
                min_size: None,
                max_size: None,
                real_window_id: None,
                initial_attributes: None,
                redraw_pending: false,
                resize_settle: Default::default(),
                pointer_position: PhysicalPosition::default(),
                dragging: None,
                resize_increments: None,
                ime_edits: Vec::new(),
            }),
        }
    }

    fn request_redraw(&self) {
        let mut state = self.lock();
        state.redraw_pending = true;
        if state.presentable() && self.frame.healthy() {
            self.frame.request_frame();
        }
    }

    fn sync_active(&self, state: &WindowState) {
        self.frame.set_active(state.presentable());
        if state.presentable() && state.redraw_pending {
            self.frame.request_frame();
        }
    }

    fn lock(&self) -> MutexGuard<'_, WindowState> {
        self.inner.lock().unwrap()
    }
}

impl WindowState {
    fn invalidate_cursor(&mut self) {
        self.cursor_key = None;
        self.cursor_pending = None;
        self.cursor_generation = self.cursor_generation.wrapping_add(1);
    }

    fn presentable(&self) -> bool {
        self.lifecycle.presentable(self.visible, self.minimized)
    }

    fn occluded(&self) -> bool {
        !self.visible || self.minimized
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct PlatformSpecificEventLoopAttributes {
    pub(crate) openharmony_app: Option<OpenHarmonyApp>,
}

pub struct EventLoop<T: 'static> {
    pub(crate) openharmony_app: OpenHarmonyApp,
    window_target: event_loop::ActiveEventLoop,
    user_events_sender: mpsc::Sender<T>,
    user_events_receiver: mpsc::Receiver<T>,
    loop_running: bool,
    surface_available: bool,
    timer: timer::Timer,
    resumed: bool,
    current_window: WindowId,
    input: InputState,
    input_states: HashMap<WindowId, InputState>,
    wait_started: Instant,
}

impl<T: 'static> EventLoop<T> {
    pub(crate) fn new(
        attributes: &PlatformSpecificEventLoopAttributes,
    ) -> Result<Self, EventLoopError> {
        let openharmony_app = attributes.openharmony_app.as_ref().expect(
            "An `OpenHarmonyApp` as passed to lib is required to create an `EventLoop` on \
             OpenHarmony or HarmonyNext",
        );

        if let Err(error) =
            openharmony_app.set_keyboard_input_delivery(KeyboardInputDelivery::ArkUi)
        {
            warn!("ArkUI keyboard input unavailable; using raw key codes: {error}");
        }

        plugins::register_enabled(openharmony_app).map_err(|error| {
            EventLoopError::Os(os_error!(OsError::from_msg(error.reason.clone())))
        })?;

        if EVENT_LOOP_CREATED.swap(true, Ordering::AcqRel) {
            return Err(EventLoopError::RecreationAttempt);
        }

        let (user_events_sender, user_events_receiver) = mpsc::channel();
        let shared = Arc::new(SharedWindowState::new(openharmony_app, 0, true));

        shared.frame.sync_delivery(openharmony_app, 0);
        let main_wake = frame::main_wake(openharmony_app);
        let timer_wake = main_wake.clone();

        Ok(Self {
            timer: timer::Timer::new(move || timer_wake.notify()),
            openharmony_app: openharmony_app.clone(),
            window_target: event_loop::ActiveEventLoop {
                p: ActiveEventLoop {
                    app: openharmony_app.clone(),
                    main_wake,
                    visible: Cell::new(true),
                    control_flow: Cell::new(ControlFlow::default()),
                    exit: Cell::new(false),
                    drag_drop: Default::default(),
                    windows: Mutex::new(HashMap::from([(GLOBAL_WINDOW, shared.clone())])),
                    root_claimed: Arc::new(AtomicBool::new(false)),
                    shared,
                },
                _marker: PhantomData,
            },
            user_events_sender,
            user_events_receiver,
            loop_running: false,
            surface_available: false,
            resumed: false,
            current_window: GLOBAL_WINDOW,
            input: InputState::default(),
            input_states: HashMap::new(),
            wait_started: Instant::now(),
        })
    }

    pub fn window_target(&self) -> &event_loop::ActiveEventLoop {
        &self.window_target
    }

    pub fn create_proxy(&self) -> EventLoopProxy<T> {
        EventLoopProxy {
            user_events_sender: self.user_events_sender.clone(),
            main_wake: self.window_target.p.main_wake.clone(),
        }
    }

    fn emit_window_event<F>(
        &self,
        window_id: WindowId,
        event: event::WindowEvent,
        event_handler: &mut F,
    ) where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        if !self.resumed
            && !matches!(
                event,
                event::WindowEvent::Destroyed | event::WindowEvent::CloseRequested
            )
        {
            return;
        }
        event_handler(
            event::Event::WindowEvent {
                window_id: crate::window::WindowId(window_id),
                event,
            },
            &self.window_target,
        );
    }

    fn emit_device_event<F>(&self, device_id: DeviceId, event: DeviceEvent, event_handler: &mut F)
    where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        event_handler(
            event::Event::DeviceEvent {
                device_id: crate::event::DeviceId(device_id),
                event,
            },
            &self.window_target,
        );
    }
}

impl<T: 'static> EventLoop<T> {
    fn handle_input_event<F>(&mut self, event: &InputEvent, event_handler: &mut F)
    where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        if let Some(touch) = self.input.pending_touch.take() {
            let mouse_emulated_touch = matches!(
                event,
                InputEvent::ArkUi(ArkUiInputEvent::Pointer(pointer))
                    if pointer.event_type == UIInputEvent::Touch
                        && pointer.timestamp == touch.timestamp
                        && pointer.source_type == UIInputSourceType::Mouse
            );
            self.input.pen_metadata = match event {
                InputEvent::ArkUi(ArkUiInputEvent::Pointer(pointer))
                    if pointer.timestamp == touch.timestamp =>
                {
                    pointer.stylus
                }
                _ => None,
            };
            if !mouse_emulated_touch {
                self.handle_touch_event(&touch, event_handler);
            }
            self.input.pen_metadata = None;
        }
        match event {
            InputEvent::XComponent(event) => match event {
                XComponentInputEvent::Touch(motion_event) => {
                    self.input.pending_touch = Some(motion_event.clone());
                }
                XComponentInputEvent::Mouse(mouse_event) => {
                    self.handle_mouse_event(mouse_event, event_handler);
                }
                XComponentInputEvent::Hover(hovered) => {
                    if !hovered {
                        self.window_target.p.shared.lock().invalidate_cursor();
                    }
                    if !hovered && self.input.mouse_inside {
                        self.input.mouse_inside = false;
                        self.input.last_mouse_position = None;
                        self.emit_window_event(
                            self.current_window,
                            event::WindowEvent::CursorLeft {
                                device_id: crate::event::DeviceId(DeviceId::dummy()),
                            },
                            event_handler,
                        );
                    }
                }
                XComponentInputEvent::Key(key) => match key.action {
                    Action::Down => self.emit_key_event(
                        key.code,
                        ElementState::Pressed,
                        DeviceId::from_raw(key.device_id),
                        false,
                        false,
                        event_handler,
                    ),
                    Action::Up => self.emit_key_event(
                        key.code,
                        ElementState::Released,
                        DeviceId::from_raw(key.device_id),
                        false,
                        false,
                        event_handler,
                    ),
                    Action::Unknown => trace!("Ignoring an OHOS key event with unknown action"),
                },
            },
            InputEvent::ArkUi(event) => self.handle_arkui_input_event(event, event_handler),
            InputEvent::Ime(data) => match data {
                ImeEvent::TextInputEvent(s) => {
                    self.emit_window_event(
                        self.current_window,
                        event::WindowEvent::Ime(Ime::Commit(s.text.clone())),
                        event_handler,
                    );
                }
                ImeEvent::BackspaceEvent(count) | ImeEvent::DeleteForwardEvent(count) => {
                    let count = (*count).max(0) as usize;
                    let (before, after) = if matches!(data, ImeEvent::BackspaceEvent(_)) {
                        (count, 0)
                    } else {
                        (0, count)
                    };
                    self.window_target
                        .p
                        .shared
                        .lock()
                        .ime_edits
                        .push(ImeEdit::DeleteSurrounding { before, after });
                    self.emit_window_event(
                        self.current_window,
                        event::WindowEvent::Ime(Ime::Enabled),
                        event_handler,
                    );
                }
                ImeEvent::PreviewTextEvent { text, .. } => self.emit_window_event(
                    self.current_window,
                    event::WindowEvent::Ime(Ime::Preedit(
                        text.clone(),
                        Some((text.len(), text.len())),
                    )),
                    event_handler,
                ),
                ImeEvent::FinishPreviewEvent => self.emit_window_event(
                    self.current_window,
                    event::WindowEvent::Ime(Ime::Preedit(String::new(), None)),
                    event_handler,
                ),
                ImeEvent::MoveCursorEvent(direction) => {
                    let key = match direction {
                        ImeDirection::Left => Some(Keycode::DpadLeft),
                        ImeDirection::Right => Some(Keycode::DpadRight),
                        ImeDirection::Up => Some(Keycode::DpadUp),
                        ImeDirection::Down => Some(Keycode::DpadDown),
                        ImeDirection::None => None,
                    };
                    if let Some(key) = key {
                        self.emit_synthetic_key(key, event_handler);
                    }
                }
                ImeEvent::ExtendActionEvent(action) => {
                    let key = match action {
                        ImeAction::Copy => Keycode::Copy,
                        ImeAction::Cut => Keycode::Cut,
                        ImeAction::Paste => Keycode::Paste,
                        ImeAction::SelectAll => Keycode::A,
                    };
                    let select_all = matches!(action, ImeAction::SelectAll);
                    let modifiers = self.input.modifiers;
                    if select_all {
                        self.input.modifiers.insert(ModifiersState::CONTROL);
                        self.emit_window_event(
                            self.current_window,
                            event::WindowEvent::ModifiersChanged(Modifiers {
                                state: self.input.modifiers,
                                pressed_mods: self.input.modifier_keys,
                            }),
                            event_handler,
                        );
                    }
                    self.emit_synthetic_key(key, event_handler);
                    if select_all {
                        self.input.modifiers = modifiers;
                        self.emit_window_event(
                            self.current_window,
                            event::WindowEvent::ModifiersChanged(Modifiers {
                                state: self.input.modifiers,
                                pressed_mods: self.input.modifier_keys,
                            }),
                            event_handler,
                        );
                    }
                }
                ImeEvent::SelectionEvent { start, end } => {
                    self.window_target
                        .p
                        .shared
                        .lock()
                        .ime_edits
                        .push(ImeEdit::Selection {
                            start: (*start).max(0) as usize,
                            end: (*end).max(0) as usize,
                        });
                    self.emit_window_event(
                        self.current_window,
                        event::WindowEvent::Ime(Ime::Enabled),
                        event_handler,
                    );
                }
                ImeEvent::EnterEvent(_) => self.emit_synthetic_key(Keycode::Enter, event_handler),
                ImeEvent::ImeStatusEvent(status) => match status {
                    KeyboardStatus::Hide => {
                        self.window_target.p.shared.lock().ime_allowed = false;
                        self.emit_window_event(
                            self.current_window,
                            event::WindowEvent::Ime(Ime::Disabled),
                            event_handler,
                        );
                    }
                    KeyboardStatus::Show => {
                        self.emit_window_event(
                            self.current_window,
                            event::WindowEvent::Ime(Ime::Enabled),
                            event_handler,
                        );
                    }
                    KeyboardStatus::None => trace!("Ignoring an empty OHOS IME status event"),
                },
            },
        }
    }

    fn handle_arkui_input_event<F>(&mut self, event: &ArkUiInputEvent, event_handler: &mut F)
    where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        match event {
            ArkUiInputEvent::FocusChanged(focused) => {
                self.update_focus(*focused, event_handler);
            }
            ArkUiInputEvent::Pointer(pointer) => {
                drag_drop::set_pointer(
                    crate::window::WindowId(self.current_window),
                    pointer.pointer_id,
                );
            }
            ArkUiInputEvent::Key(key) => {
                key.response.consume();
                self.sync_key_modifiers(key, event_handler);
                if key.ime_consumed {
                    return;
                }
                let state = match key.action {
                    Action::Down => ElementState::Pressed,
                    Action::Up => ElementState::Released,
                    Action::Unknown => return,
                };
                self.emit_key_event_with_unicode(
                    key.code,
                    state,
                    DeviceId::from_raw(key.device_id),
                    false,
                    false,
                    char::from_u32(key.unicode).filter(|ch| !ch.is_control()),
                    event_handler,
                );
            }
            ArkUiInputEvent::Drag(drag) => {
                let offset = self.window_target.p.shared.lock().surface_position;
                let events = self
                    .window_target
                    .p
                    .drag_drop
                    .lock()
                    .unwrap()
                    .event(crate::window::WindowId(self.current_window), drag, offset);
                for event in events {
                    self.emit_window_event(self.current_window, event, event_handler);
                }
            }
            ArkUiInputEvent::Axis(event) => {
                let mouse_wheel = event.pointer.tool_type == UIInputToolType::Mouse;
                if mouse_wheel && event.pointer.action != UIInputAction::Move {
                    return;
                }
                let device_id = DeviceId::dummy();
                let position =
                    PhysicalPosition::new(f64::from(event.pointer.x), f64::from(event.pointer.y));
                self.ensure_mouse_entered(device_id, event_handler);
                self.emit_window_event(
                    self.current_window,
                    event::WindowEvent::CursorMoved {
                        device_id: crate::event::DeviceId(device_id),
                        position,
                    },
                    event_handler,
                );
                let delta = if mouse_wheel {
                    MouseScrollDelta::LineDelta(
                        (-event.delta_x / 15.0) as f32,
                        (-event.delta_y / 15.0) as f32,
                    )
                } else {
                    MouseScrollDelta::PixelDelta(PhysicalPosition::new(
                        -event.delta_x,
                        -event.delta_y,
                    ))
                };
                self.emit_window_event(
                    self.current_window,
                    event::WindowEvent::MouseWheel {
                        device_id: crate::event::DeviceId(device_id),
                        delta,
                        phase: if mouse_wheel {
                            TouchPhase::Moved
                        } else {
                            touch_phase_from_arkui(event.pointer.action)
                        },
                    },
                    event_handler,
                );
            }
            ArkUiInputEvent::Gesture(GestureEvent::Tap(event)) => {
                if self.openharmony_app.touch_input_delivery()
                    != openharmony_ability::TouchInputDelivery::ArkUiGestures
                {
                    trace!("Ignoring OHOS Tap alongside raw touch delivery");
                    return;
                }
                let finger_id = event.pointer.pointer_id.unwrap_or(0);
                self.emit_touch_point(
                    TouchEvent::Down,
                    finger_id,
                    event.pointer.x,
                    event.pointer.y,
                    0.0,
                    0,
                    event_handler,
                );
                self.emit_touch_point(
                    TouchEvent::Up,
                    finger_id,
                    event.pointer.x,
                    event.pointer.y,
                    0.0,
                    0,
                    event_handler,
                );
            }
            ArkUiInputEvent::Gesture(GestureEvent::Pan(event)) => {
                self.emit_window_event(
                    self.current_window,
                    event::WindowEvent::MouseWheel {
                        device_id: crate::event::DeviceId(DeviceId::dummy()),
                        delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(
                            f64::from(event.delta_x),
                            f64::from(event.delta_y),
                        )),
                        phase: touch_phase_from_gesture(event.phase),
                    },
                    event_handler,
                );
            }
            ArkUiInputEvent::Gesture(GestureEvent::Swipe(_)) => {
                trace!("Ignoring an OHOS swipe event after its pan stream");
            }
        }
    }

    fn handle_touch_event<F>(&mut self, motion_event: &TouchEventData, event_handler: &mut F)
    where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        match motion_event.event_type {
            TouchEvent::Move | TouchEvent::Cancel => {
                if motion_event.touch_points.is_empty() {
                    self.emit_touch_point(
                        motion_event.event_type,
                        motion_event.id,
                        motion_event.x,
                        motion_event.y,
                        motion_event.force,
                        motion_event.device_id,
                        event_handler,
                    );
                } else {
                    for point in &motion_event.touch_points {
                        self.emit_touch_data(motion_event, point, event_handler);
                    }
                }
            }
            TouchEvent::Down | TouchEvent::Up => {
                if let Some(point) = motion_event
                    .touch_points
                    .iter()
                    .find(|point| point.id == motion_event.id)
                {
                    self.emit_touch_data(motion_event, point, event_handler);
                } else {
                    self.emit_touch_point(
                        motion_event.event_type,
                        motion_event.id,
                        motion_event.x,
                        motion_event.y,
                        motion_event.force,
                        motion_event.device_id,
                        event_handler,
                    );
                }
            }
            TouchEvent::Unknown => trace!("Ignoring an OHOS touch event with unknown action"),
        }
    }

    fn emit_touch_data<F>(
        &mut self,
        event: &TouchEventData,
        point: &TouchPointData,
        event_handler: &mut F,
    ) where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        self.emit_touch_point(
            event.event_type,
            point.id,
            point.x,
            point.y,
            point.force,
            event.device_id,
            event_handler,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_touch_point<F>(
        &mut self,
        action: TouchEvent,
        raw_finger_id: i32,
        x: f32,
        y: f32,
        raw_force: f32,
        raw_device_id: i64,
        event_handler: &mut F,
    ) where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        self.update_window_drag(
            PhysicalPosition::new(f64::from(x), f64::from(y)),
            matches!(action, TouchEvent::Up | TouchEvent::Cancel),
        );
        let phase = match action {
            TouchEvent::Down => {
                self.input.pressed_fingers.insert(raw_finger_id);
                TouchPhase::Started
            }
            TouchEvent::Move => TouchPhase::Moved,
            TouchEvent::Up => {
                self.input.pressed_fingers.remove(&raw_finger_id);
                TouchPhase::Ended
            }
            TouchEvent::Cancel => {
                self.input.pressed_fingers.remove(&raw_finger_id);
                TouchPhase::Cancelled
            }
            TouchEvent::Unknown => return,
        };
        let force = self
            .input
            .pen_metadata
            .and_then(stylus::stylus_force)
            .or_else(|| Some(Force::Normalized(normalized_force(raw_force))));
        let device_id = DeviceId::from_raw(raw_device_id);
        let location = PhysicalPosition::new(f64::from(x), f64::from(y));
        let id = u64::try_from(raw_finger_id).unwrap_or(0);
        self.emit_window_event(
            self.current_window,
            event::WindowEvent::Touch(Touch {
                device_id: crate::event::DeviceId(device_id),
                phase,
                location,
                force,
                id,
            }),
            event_handler,
        );
    }

    fn update_window_drag(&self, position: PhysicalPosition<f64>, released: bool) {
        let mut state = self.window_target.p.shared.lock();
        state.pointer_position = position;
        if released {
            state.dragging = None;
            self.window_target.p.shared.geometry.cancel();
            return;
        }
        let Some(drag) = state.dragging else {
            return;
        };
        let screen = PhysicalPosition::new(
            position.x + f64::from(state.position.x),
            position.y + f64::from(state.position.y),
        );
        let dx = (screen.x - drag.pointer.x) as i32;
        let dy = (screen.y - drag.pointer.y) as i32;
        let mut origin = drag.position;
        let mut size = drag.size;
        let resize = drag.direction;
        if let Some(direction) = resize {
            use ResizeDirection::*;
            let west = matches!(direction, West | NorthWest | SouthWest);
            let east = matches!(direction, East | NorthEast | SouthEast);
            let north = matches!(direction, North | NorthEast | NorthWest);
            let south = matches!(direction, South | SouthEast | SouthWest);
            let min = state.min_size.unwrap_or(PhysicalSize::new(1, 1));
            let max = state
                .max_size
                .unwrap_or(PhysicalSize::new(u32::MAX, u32::MAX));
            if west || east {
                size.width = (i64::from(drag.size.width)
                    + if west { -i64::from(dx) } else { i64::from(dx) })
                .clamp(
                    i64::from(min.width.max(1)),
                    i64::from(max.width.max(min.width).max(1)),
                ) as u32;
            }
            if north || south {
                size.height = (i64::from(drag.size.height)
                    + if north { -i64::from(dy) } else { i64::from(dy) })
                .clamp(
                    i64::from(min.height.max(1)),
                    i64::from(max.height.max(min.height).max(1)),
                ) as u32;
            }
            if let Some(increments) = state.resize_increments {
                if increments.width > 0 {
                    size.width -= size.width % increments.width;
                }
                if increments.height > 0 {
                    size.height -= size.height % increments.height;
                }
            }
            if west {
                origin.x += drag.size.width as i32 - size.width as i32;
            }
            if north {
                origin.y += drag.size.height as i32 - size.height as i32;
            }
        } else {
            origin.x += dx;
            origin.y += dy;
        }
        drop(state);
        let window = Window {
            app: self.openharmony_app.clone(),
            shared: self.window_target.p.shared.clone(),
            id: self.current_window,
            root_claimed: self.window_target.p.root_claimed.clone(),
            owns_window: false,
        };
        window.submit_geometry(GeometryTarget {
            position: origin,
            size: resize.map(|_| size),
        });
    }

    fn handle_mouse_event<F>(&mut self, mouse_event: &MouseEventData, event_handler: &mut F)
    where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        let device_id = DeviceId::dummy();
        let position = PhysicalPosition::new(f64::from(mouse_event.x), f64::from(mouse_event.y));
        self.update_window_drag(
            position,
            matches!(mouse_event.action, OhosMouseAction::Release),
        );

        match mouse_event.action {
            OhosMouseAction::Press | OhosMouseAction::Release | OhosMouseAction::Move => {
                self.ensure_mouse_entered(device_id, event_handler);
            }
            _ => {}
        }

        match mouse_event.action {
            OhosMouseAction::Move => {
                if let Some(last) = self.input.last_mouse_position {
                    let delta = (position.x - last.x, position.y - last.y);
                    if delta.0 != 0.0 || delta.1 != 0.0 {
                        self.emit_device_event(
                            device_id,
                            DeviceEvent::MouseMotion { delta },
                            event_handler,
                        );
                    }
                }
                self.input.last_mouse_position = Some(position);
                self.emit_window_event(
                    self.current_window,
                    event::WindowEvent::CursorMoved {
                        device_id: crate::event::DeviceId(device_id),
                        position,
                    },
                    event_handler,
                );
            }
            OhosMouseAction::Press | OhosMouseAction::Release => {
                self.input.last_mouse_position = Some(position);
                self.emit_window_event(
                    self.current_window,
                    event::WindowEvent::MouseInput {
                        device_id: crate::event::DeviceId(device_id),
                        state: if matches!(mouse_event.action, OhosMouseAction::Press) {
                            ElementState::Pressed
                        } else {
                            ElementState::Released
                        },
                        button: map_mouse_button(mouse_event.button),
                    },
                    event_handler,
                );
            }
            _ => trace!("Ignoring an OHOS mouse event with no actionable state"),
        }
    }

    fn ensure_mouse_entered<F>(&mut self, device_id: DeviceId, event_handler: &mut F)
    where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        if self.input.mouse_inside {
            return;
        }
        self.input.mouse_inside = true;
        self.window_target.p.shared.lock().invalidate_cursor();
        self.emit_window_event(
            self.current_window,
            event::WindowEvent::CursorEntered {
                device_id: crate::event::DeviceId(device_id),
            },
            event_handler,
        );
    }

    fn emit_synthetic_key<F>(&mut self, keycode: Keycode, event_handler: &mut F)
    where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        for state in [ElementState::Pressed, ElementState::Released] {
            self.emit_key_event(
                keycode,
                state,
                DeviceId::dummy(),
                false,
                true,
                event_handler,
            );
        }
    }

    fn emit_key_event<F>(
        &mut self,
        keycode: Keycode,
        state: ElementState,
        device_id: DeviceId,
        repeat: bool,
        is_synthetic: bool,
        event_handler: &mut F,
    ) where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        self.emit_key_event_with_unicode(
            keycode,
            state,
            device_id,
            repeat,
            is_synthetic,
            None,
            event_handler,
        );
    }

    fn sync_key_modifiers<F>(&mut self, key: &KeyboardEventData, event_handler: &mut F)
    where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        let Some(pressed) = &key.pressed_keys else {
            return;
        };
        let before_state = self.input.modifiers;
        let before_keys = self.input.modifier_keys;
        self.input.modifiers = ModifiersState::empty();
        self.input.modifier_keys = ModifiersKeys::empty();
        for code in pressed {
            let _ = self.update_modifiers(*code, ElementState::Pressed);
        }
        if before_state != self.input.modifiers || before_keys != self.input.modifier_keys {
            self.emit_window_event(
                self.current_window,
                event::WindowEvent::ModifiersChanged(Modifiers {
                    state: self.input.modifiers,
                    pressed_mods: self.input.modifier_keys,
                }),
                event_handler,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_key_event_with_unicode<F>(
        &mut self,
        keycode: Keycode,
        state: ElementState,
        device_id: DeviceId,
        repeat: bool,
        is_synthetic: bool,
        unicode: Option<char>,
        event_handler: &mut F,
    ) where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        if let Some(modifiers) = self.update_modifiers(keycode, state) {
            self.emit_window_event(
                self.current_window,
                event::WindowEvent::ModifiersChanged(modifiers),
                event_handler,
            );
        }

        let physical_key = keycodes::to_physical_key(keycode);
        let key_without_modifiers = keycodes::to_logical(keycode);
        let logical_key = unicode.map_or_else(
            || key_without_modifiers.clone(),
            |ch| Key::Character(SmolStr::from(ch.encode_utf8(&mut [0; 4]))),
        );
        let text = if state.is_pressed() {
            match &logical_key {
                Key::Character(text) => Some(text.clone()),
                Key::Named(NamedKey::Space) => Some(SmolStr::new(" ")),
                _ => None,
            }
        } else {
            None
        };
        if !is_synthetic {
            self.emit_device_event(
                device_id,
                DeviceEvent::Key(RawKeyEvent {
                    physical_key,
                    state,
                }),
                event_handler,
            );
        }
        self.emit_window_event(
            self.current_window,
            event::WindowEvent::KeyboardInput {
                device_id: crate::event::DeviceId(device_id),
                event: KeyEvent {
                    state,
                    physical_key,
                    logical_key,
                    location: keycodes::to_location(keycode),
                    repeat,
                    text: text.clone(),
                    platform_specific: KeyEventExtra {
                        text_with_all_modifiers: text,
                        key_without_modifiers,
                    },
                },
                is_synthetic,
            },
            event_handler,
        );
    }

    fn update_modifiers(&mut self, keycode: Keycode, state: ElementState) -> Option<Modifiers> {
        let (state_flag, key_flag) = match keycode {
            Keycode::ShiftLeft => (ModifiersState::SHIFT, ModifiersKeys::LSHIFT),
            Keycode::ShiftRight => (ModifiersState::SHIFT, ModifiersKeys::RSHIFT),
            Keycode::AltLeft => (ModifiersState::ALT, ModifiersKeys::LALT),
            Keycode::AltRight => (ModifiersState::ALT, ModifiersKeys::RALT),
            Keycode::CtrlLeft => (ModifiersState::CONTROL, ModifiersKeys::LCONTROL),
            Keycode::CtrlRight => (ModifiersState::CONTROL, ModifiersKeys::RCONTROL),
            Keycode::MetaLeft => (ModifiersState::SUPER, ModifiersKeys::LSUPER),
            Keycode::MetaRight => (ModifiersState::SUPER, ModifiersKeys::RSUPER),
            _ => return None,
        };
        let before_state = self.input.modifiers;
        let before_keys = self.input.modifier_keys;
        if state.is_pressed() {
            self.input.modifiers.insert(state_flag);
            self.input.modifier_keys.insert(key_flag);
        } else {
            self.input.modifier_keys.remove(key_flag);
            let still_pressed = match state_flag {
                ModifiersState::SHIFT => self
                    .input
                    .modifier_keys
                    .intersects(ModifiersKeys::LSHIFT | ModifiersKeys::RSHIFT),
                ModifiersState::ALT => self
                    .input
                    .modifier_keys
                    .intersects(ModifiersKeys::LALT | ModifiersKeys::RALT),
                ModifiersState::CONTROL => self
                    .input
                    .modifier_keys
                    .intersects(ModifiersKeys::LCONTROL | ModifiersKeys::RCONTROL),
                ModifiersState::SUPER => self
                    .input
                    .modifier_keys
                    .intersects(ModifiersKeys::LSUPER | ModifiersKeys::RSUPER),
                _ => false,
            };
            if !still_pressed {
                self.input.modifiers.remove(state_flag);
            }
        }
        (before_state != self.input.modifiers || before_keys != self.input.modifier_keys)
            .then_some(Modifiers {
                state: self.input.modifiers,
                pressed_mods: self.input.modifier_keys,
            })
    }

    pub fn run<F>(mut self, event_handler: F) -> Result<(), EventLoopError>
    where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop),
    {
        let openharmony_app = self.openharmony_app.clone();
        let event_handler_cell: Box<dyn FnMut(event::Event<T>, &event_loop::ActiveEventLoop)> =
            Box::new(event_handler);
        // SAFETY: The event loop callback is owned by the Ability on the UI thread and dropped on exit.
        let mut handler_opt: Option<
            Box<dyn FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + 'static>,
        > = Some(unsafe { std::mem::transmute(event_handler_cell) });

        openharmony_app.run_loop(move |event| {
            self.window_target.p.main_wake.begin_turn();
            let destroyed = matches!(event, MainEvent::Destroy);
            if let Some(handler) = handler_opt.as_mut() {
                self.single_iteration(event, &mut **handler);
            }
            if (destroyed || self.window_target.p.exiting()) && handler_opt.take().is_some() {
                let bridge = self.openharmony_app.bridge();
                task::spawn(async move {
                    use plugins::app_control::{
                        AppControlBridgePlugin, TerminateRequest, TerminateResponse,
                    };
                    let result = async {
                        bridge?
                            .call_sync_from_worker::<
                                AppControlBridgePlugin,
                                TerminateRequest,
                                TerminateResponse,
                            >("terminate", TerminateRequest { code: 0 })
                            .await
                    }
                    .await;
                    if let Err(error) = result {
                        warn!("OHOS terminate: {error}");
                    }
                });
            }
        });
        Ok(())
    }

    pub fn run_on_demand<F>(&mut self, _event_handler: F) -> Result<(), EventLoopError>
    where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop),
    {
        Err(EventLoopError::NotSupported(error::NotSupportedError::new()))
    }

    pub fn pump_events<F>(&mut self, _timeout: Option<Duration>, _event_handler: F) -> PumpStatus
    where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop),
    {
        warn!("pump_events is not supported by the callback-driven OHOS Ability lifecycle");
        if self.window_target.p.exiting() {
            PumpStatus::Exit(0)
        } else {
            PumpStatus::Continue
        }
    }

    fn sync_frame_delivery(&self) {
        for (id, shared) in self.registered_windows() {
            shared
                .frame
                .sync_delivery(&self.openharmony_app, id.into_raw() as i64);
        }
    }

    fn registered_windows(&self) -> Vec<(WindowId, Arc<SharedWindowState>)> {
        let mut windows: Vec<_> = self
            .window_target
            .p
            .windows
            .lock()
            .unwrap()
            .iter()
            .map(|(id, shared)| (*id, shared.clone()))
            .collect();
        windows.sort_by_key(|(id, _)| id.into_raw());
        windows
    }

    fn request_window_redraw(&self, id: WindowId) {
        let shared = self
            .window_target
            .p
            .windows
            .lock()
            .unwrap()
            .get(&id)
            .cloned();
        if let Some(shared) = shared {
            shared.request_redraw();
        }
    }

    fn clear_window_input(&mut self, id: WindowId) {
        if self.current_window == id {
            self.input = InputState::default();
        }
        self.input_states.remove(&id);
    }

    fn close_window<F>(&mut self, id: WindowId, event_handler: &mut F)
    where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        let shared = self
            .window_target
            .p
            .windows
            .lock()
            .unwrap()
            .get(&id)
            .cloned();
        let Some(shared) = shared else {
            return;
        };
        let events = {
            let mut state = shared.lock();
            let events = state.lifecycle.close();
            state.dragging = None;
            shared.sync_active(&state);
            events
        };
        if !events.destroyed {
            return;
        }
        shared.geometry.cancel();
        drag_drop::cancel_outgoing(Some(crate::window::WindowId(id)));
        self.clear_window_input(id);
        if events.close_requested {
            self.emit_window_event(id, event::WindowEvent::CloseRequested, event_handler);
        }
        self.emit_window_event(id, event::WindowEvent::Destroyed, event_handler);
        self.window_target.p.windows.lock().unwrap().remove(&id);
    }

    fn single_iteration<F>(&mut self, event: MainEvent<'_>, event_handler: &mut F)
    where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        if self.window_target.p.exiting() {
            self.timer.set_deadline(None);
            return;
        }
        let id = match &event {
            MainEvent::WindowResize { window_id, .. }
            | MainEvent::WindowFocusChanged { window_id, .. }
            | MainEvent::TextInputReadyChanged { window_id, .. }
            | MainEvent::SubWindowInput { window_id, .. }
            | MainEvent::SubWindowRedraw { window_id, .. } => *window_id,
            MainEvent::SubWindowSurfaceCreate(id)
            | MainEvent::SubWindowSurfaceDestroy(id)
            | MainEvent::SubWindowClosed(id) => *id,
            MainEvent::ContentRectChange(content) => content.window_id,
            MainEvent::AvoidAreaChange(info) => info.window_id,
            _ => 0,
        };
        let id = WindowId::from_raw(id as usize);
        let shared = self
            .window_target
            .p
            .windows
            .lock()
            .unwrap()
            .get(&id)
            .cloned();
        if let Some(shared) = shared {
            if self.current_window != id {
                let old_input = std::mem::take(&mut self.input);
                if self
                    .window_target
                    .p
                    .windows
                    .lock()
                    .unwrap()
                    .contains_key(&self.current_window)
                {
                    self.input_states.insert(self.current_window, old_input);
                }
                self.input = self.input_states.remove(&id).unwrap_or_default();
            }
            self.current_window = id;
            self.window_target.p.shared = shared;
        }

        let user_turn = matches!(&event, MainEvent::UserEvent);
        let fallback_tick = match &event {
            MainEvent::WindowRedraw(_) | MainEvent::SubWindowRedraw { .. } => Some(id),
            _ => None,
        };
        let cause = if self.loop_running {
            start_cause(self.window_target.p.control_flow(), self.wait_started)
        } else {
            self.loop_running = true;
            StartCause::Init
        };
        event_handler(event::Event::NewEvents(cause), &self.window_target);
        for (id, status) in openharmony_ability::drain_pending_window_status() {
            let Some(status) = window_status::window_status(status) else {
                warn!("Ignoring invalid OHOS window status: {status}");
                continue;
            };
            let id = WindowId::from_raw(id as usize);
            let shared = self
                .window_target
                .p
                .windows
                .lock()
                .unwrap()
                .get(&id)
                .cloned();
            if let Some(shared) = shared {
                let (changed, occluded, restored) = {
                    let mut state = shared.lock();
                    let old_occluded = state.occluded();
                    let restored = state.minimized && !status.minimized;
                    state.minimized = status.minimized;
                    state.maximized = status.maximized;
                    state.fullscreen = status.fullscreen.then_some(Fullscreen::Borderless(None));
                    shared.sync_active(&state);
                    (old_occluded != state.occluded(), state.occluded(), restored)
                };
                if changed {
                    self.emit_window_event(
                        id,
                        event::WindowEvent::Occluded(occluded),
                        event_handler,
                    );
                }
                if restored {
                    shared.request_redraw();
                }
            }
        }
        for id in openharmony_ability::drain_pending_window_closes() {
            let id = WindowId::from_raw(id as usize);
            if id == GLOBAL_WINDOW {
                if self.window_target.p.windows.lock().unwrap().contains_key(&id) {
                    self.emit_window_event(
                        id,
                        event::WindowEvent::CloseRequested,
                        event_handler,
                    );
                }
            } else {
                self.close_window(id, event_handler);
            }
        }
        let registered = self.window_target.p.windows.lock().unwrap().contains_key(&id);
        if registered
            || matches!(
                &event,
                MainEvent::ConfigChanged(_)
                    | MainEvent::Start
                    | MainEvent::Stop
                    | MainEvent::Resume(_)
                    | MainEvent::Destroy
                    | MainEvent::LowMemory
                    | MainEvent::UserEvent
                    | MainEvent::NewWant { .. }
                    | MainEvent::Pause
                    | MainEvent::Create
                    | MainEvent::WindowCreate
                    | MainEvent::SaveState(_)
            )
        {
            self.handle_main_event(event, event_handler);
        }
        while let Ok(user_event) = self.user_events_receiver.try_recv() {
            event_handler(event::Event::UserEvent(user_event), &self.window_target);
        }
        drag_drop::drain_outgoing();
        if events::take_pending() {
            self.request_window_redraw(GLOBAL_WINDOW);
        }
        if !self.window_target.p.exiting() {
            for (id, shared) in self.registered_windows() {
                let tick = if user_turn {
                    shared
                        .frame
                        .sync_delivery(&self.openharmony_app, id.into_raw() as i64);
                    shared.frame.take_pending()
                } else {
                    fallback_tick == Some(id) && !shared.frame.healthy()
                };
                let redraw = {
                    let mut state = shared.lock();
                    let WindowState {
                        lifecycle,
                        visible,
                        minimized,
                        redraw_pending,
                        ..
                    } = &mut *state;
                    lifecycle.take_redraw(tick, *visible, *minimized, redraw_pending)
                };
                if redraw {
                    self.emit_window_event(
                        id,
                        event::WindowEvent::RedrawRequested,
                        event_handler,
                    );
                    let mut state = shared.lock();
                    if state.presentable() && state.resize_settle.active(Instant::now()) {
                        state.redraw_pending = true;
                        if shared.frame.healthy() {
                            shared.frame.request_frame();
                        }
                    }
                }
            }
        }
        event_handler(event::Event::AboutToWait, &self.window_target);
        if self.window_target.p.exiting() {
            self.timer.set_deadline(None);
            self.surface_destroyed(event_handler, true);
            event_handler(event::Event::LoopExiting, &self.window_target);
            return;
        }
        self.wait_started = Instant::now();
        let presentable = self
            .registered_windows()
            .into_iter()
            .find(|(_, shared)| shared.lock().presentable());
        use control_flow::{control_flow_action, Action, ControlFlow as Policy};
        let flow = match self.window_target.p.control_flow() {
            ControlFlow::Wait => Policy::Wait,
            ControlFlow::WaitUntil(deadline) => Policy::WaitUntil(deadline),
            ControlFlow::Poll => Policy::Poll,
        };
        match control_flow_action(flow, self.wait_started, presentable.is_some()) {
            Action::Wait => self.timer.set_deadline(None),
            Action::Deadline(deadline) => self.timer.set_deadline(Some(deadline)),
            Action::Wake => {
                self.timer.set_deadline(None);
                self.window_target.p.main_wake.notify();
            }
            Action::Frame => {
                self.timer.set_deadline(None);
                if let Some((_, shared)) = presentable {
                    let state = shared.lock();
                    if state.presentable() {
                        shared.frame.request_frame();
                    }
                }
            }
        }
    }

    fn handle_main_event<F>(&mut self, event: MainEvent<'_>, event_handler: &mut F)
    where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        match event {
            MainEvent::SurfaceCreate => self.surface_created(event_handler),
            MainEvent::SurfaceDestroy => self.surface_destroyed(event_handler, false),
            MainEvent::WindowResize { size, .. } => {
                self.update_surface_size(physical_size(size.width, size.height), event_handler)
            }
            MainEvent::WindowRedraw(_) => {}
            MainEvent::ContentRectChange(content) => {
                let position = PhysicalPosition::new(content.rect.left, content.rect.top);
                let outer_size = physical_size(content.rect.width, content.rect.height);
                let (moved, resized) = {
                    let mut state = self.window_target.p.shared.lock();
                    let moved = state.position != position;
                    let resized = state.outer_size != outer_size;
                    state.position = position;
                    state.outer_size = outer_size;
                    if resized {
                        state.resize_settle.extend(Instant::now());
                    }
                    (moved, resized)
                };
                if moved {
                    self.emit_window_event(
                        self.current_window,
                        event::WindowEvent::Moved(position),
                        event_handler,
                    );
                }
                if resized {
                    self.window_target.p.shared.request_redraw();
                }
            }
            MainEvent::AvoidAreaChange(_) => self.request_window_redraw(self.current_window),
            MainEvent::KeyboardEvent(_) => self.request_window_redraw(GLOBAL_WINDOW),
            MainEvent::TextInputReadyChanged { window_id, ready } => {
                if ready && self.window_target.p.shared.lock().ime_allowed {
                    if let Err(error) = self.openharmony_app.try_show_keyboard_for(window_id) {
                        warn!("Cannot show OHOS keyboard after TextInputReadyChanged: {error}");
                    }
                }
            }
            MainEvent::WindowFocusChanged { focused, .. } => {
                self.update_focus(focused, event_handler)
            }
            MainEvent::GainedFocus => self.update_focus(true, event_handler),
            MainEvent::LostFocus => self.update_focus(false, event_handler),
            MainEvent::ConfigChanged(configuration) => {
                self.update_configuration(&configuration, event_handler)
            }
            MainEvent::LowMemory => {
                event_handler(event::Event::MemoryWarning, &self.window_target);
            }
            MainEvent::Start => self.update_visibility(true, event_handler),
            MainEvent::Resume(_) => {
                self.update_visibility(true, event_handler);
                if self.surface_available {
                    self.ensure_resumed(event_handler);
                }
            }
            MainEvent::Pause => trace!("OHOS Ability paused"),
            MainEvent::Stop => {
                self.update_visibility(false, event_handler);
            }
            MainEvent::WindowDestroy => self.emit_window_event(
                self.current_window,
                event::WindowEvent::CloseRequested,
                event_handler,
            ),
            MainEvent::Destroy => {
                self.timer.set_deadline(None);
                self.surface_destroyed(event_handler, true);
                let mut windows = self.registered_windows();
                windows.sort_by_key(|(id, _)| (*id == GLOBAL_WINDOW, id.into_raw()));
                for (id, shared) in windows {
                    shared.lock().lifecycle.closed = true;
                    self.emit_window_event(id, event::WindowEvent::Destroyed, event_handler);
                    self.window_target.p.windows.lock().unwrap().remove(&id);
                }
                self.window_target.p.exit();
            }
            MainEvent::Input(input_event) => self.handle_input_event(&input_event, event_handler),
            MainEvent::Create | MainEvent::WindowCreate | MainEvent::SaveState(_) => {}
            MainEvent::UserEvent => {}
            MainEvent::SubWindowSurfaceCreate(_) => {
                {
                    let mut state = self.window_target.p.shared.lock();
                    state.lifecycle.surface_created();
                    state.resize_settle.extend(Instant::now());
                    state.invalidate_cursor();
                    self.window_target.p.shared.sync_active(&state);
                }
                self.sync_frame_delivery();
                self.synchronize_window_state();
                if let Some(ready) = self.window_target.p.shared.ready.lock().unwrap().take() {
                    let _ = ready.send(());
                }
                let attrs = self.window_target.p.shared.lock().initial_attributes.take();
                if let Some(attrs) = attrs {
                    let window = Window {
                        app: self.openharmony_app.clone(),
                        shared: self.window_target.p.shared.clone(),
                        id: self.current_window,
                        root_claimed: self.window_target.p.root_claimed.clone(),
                        owns_window: false,
                    };
                    window.configure(attrs);
                }
                if self.resumed {
                    event_handler(event::Event::Resumed, &self.window_target);
                }
                let size = self.window_target.p.shared.lock().surface_size;
                self.emit_window_event(
                    self.current_window,
                    event::WindowEvent::Resized(size),
                    event_handler,
                );
                self.window_target.p.shared.request_redraw();
            }
            MainEvent::SubWindowInput { event, .. } => {
                self.handle_input_event(&event, event_handler)
            }
            MainEvent::SubWindowRedraw { .. } => {}
            MainEvent::SubWindowSurfaceDestroy(_) => {
                let destroyed = {
                    let mut state = self.window_target.p.shared.lock();
                    let events = state.lifecycle.surface_destroyed();
                    state.dragging = None;
                    self.window_target.p.shared.sync_active(&state);
                    events.destroyed
                };
                self.window_target.p.shared.geometry.cancel();
                drag_drop::cancel_outgoing(Some(crate::window::WindowId(self.current_window)));
                self.clear_window_input(self.current_window);
                if destroyed {
                    self.emit_window_event(
                        self.current_window,
                        event::WindowEvent::Destroyed,
                        event_handler,
                    );
                    self.window_target
                        .p
                        .windows
                        .lock()
                        .unwrap()
                        .remove(&self.current_window);
                }
            }
            MainEvent::SubWindowClosed(_) => self.close_window(self.current_window, event_handler),
            MainEvent::NewWant { uri } => {
                events::emit(events::PlatformEvent::Want {
                    uri,
                    parameters: self.openharmony_app.take_want_parameters(),
                });
                self.request_window_redraw(GLOBAL_WINDOW);
            }
        }
    }

    fn surface_created<F>(&mut self, event_handler: &mut F)
    where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        let uri = self.openharmony_app.take_initial_want_uri();
        let parameters = self.openharmony_app.take_want_parameters();
        {
            let mut state = self.window_target.p.shared.lock();
            state.lifecycle.surface_created();
            state.resize_settle.extend(Instant::now());
            state.invalidate_cursor();
            self.window_target.p.shared.sync_active(&state);
        }
        self.sync_frame_delivery();
        self.synchronize_window_state();
        self.ensure_resumed(event_handler);
        if !self.surface_available {
            self.surface_available = true;
            let size = self.window_target.p.shared.lock().surface_size;
            self.emit_window_event(
                self.current_window,
                event::WindowEvent::Resized(size),
                event_handler,
            );
        }
        self.window_target.p.shared.request_redraw();
        if !uri.is_empty() || !parameters.is_empty() {
            events::emit(events::PlatformEvent::Want { uri, parameters });
        }
    }

    fn synchronize_window_state(&self) {
        let content_rect = self
            .openharmony_app
            .content_rect_for(self.current_window.into_raw() as i64);
        let window_rect = self
            .openharmony_app
            .window_rect_for(self.current_window.into_raw() as i64);
        let surface_size = physical_size(content_rect.width, content_rect.height);
        let outer_size = physical_size(window_rect.width, window_rect.height);
        let mut state = self.window_target.p.shared.lock();
        state.surface_size = surface_size;
        state.surface_position = PhysicalPosition::new(content_rect.left, content_rect.top);
        state.outer_size = nonzero_size_or(outer_size, surface_size);
        state.position = PhysicalPosition::new(window_rect.left, window_rect.top);
        state.scale_factor = scale_factor(&self.openharmony_app);
        state.theme = theme(self.openharmony_app.config().color_mode);
    }

    fn surface_destroyed<F>(&mut self, event_handler: &mut F, all: bool)
    where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        drag_drop::cancel_outgoing(if all {
            None
        } else {
            Some(crate::window::WindowId(GLOBAL_WINDOW))
        });
        if self.surface_available
            || (all
                && self
                    .registered_windows()
                    .iter()
                    .any(|(_, shared)| shared.lock().lifecycle.surface_available))
        {
            self.surface_available = false;
        }
        for (id, shared) in self.registered_windows() {
            if !all && id != GLOBAL_WINDOW {
                continue;
            }
            let mut state = shared.lock();
            state.lifecycle.surface_available = false;
            state.surface_size = PhysicalSize::new(0, 0);
            state.dragging = None;
            shared.sync_active(&state);
            shared.geometry.cancel();
            drop(state);
            self.clear_window_input(id);
        }
        self.suspend(event_handler);
    }

    fn ensure_resumed<F>(&mut self, event_handler: &mut F)
    where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        if !self.resumed {
            self.resumed = true;
            event_handler(event::Event::Resumed, &self.window_target);
        }
    }

    fn suspend<F>(&mut self, event_handler: &mut F)
    where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        if self.resumed {
            self.resumed = false;
            event_handler(event::Event::Suspended, &self.window_target);
        }
    }

    fn update_focus<F>(&mut self, focused: bool, event_handler: &mut F)
    where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        let mut changes = Vec::new();
        for (id, shared) in self.registered_windows() {
            if !focused && id != self.current_window {
                continue;
            }
            let next = focused && id == self.current_window;
            let changed = {
                let mut state = shared.lock();
                let changed = state.focused != next;
                state.focused = next;
                changed
            };
            changes.push((id, next, changed));
        }
        changes.sort_by_key(|(_, focused, _)| *focused);
        for (id, focused, changed) in changes {
            if changed {
                self.emit_window_event(id, event::WindowEvent::Focused(focused), event_handler);
            }
            if !focused {
                let clear_mods = if id == self.current_window {
                    if !self.input.modifiers.is_empty() || !self.input.modifier_keys.is_empty() {
                        self.input.modifiers = ModifiersState::empty();
                        self.input.modifier_keys = ModifiersKeys::empty();
                        true
                    } else {
                        false
                    }
                } else if let Some(input) = self.input_states.get_mut(&id) {
                    if !input.modifiers.is_empty() || !input.modifier_keys.is_empty() {
                        input.modifiers = ModifiersState::empty();
                        input.modifier_keys = ModifiersKeys::empty();
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                if clear_mods {
                    self.emit_window_event(
                        id,
                        event::WindowEvent::ModifiersChanged(Modifiers::default()),
                        event_handler,
                    );
                }
            }
        }
    }

    fn update_visibility<F>(&self, visible: bool, event_handler: &mut F)
    where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        self.window_target.p.visible.set(visible);
        for (id, shared) in self.registered_windows() {
            let (changed, occluded) = {
                let mut state = shared.lock();
                let old = state.occluded();
                state.visible = visible;
                shared.sync_active(&state);
                (old != state.occluded(), state.occluded())
            };
            if changed {
                self.emit_window_event(id, event::WindowEvent::Occluded(occluded), event_handler);
            }
            if visible {
                shared.request_redraw();
            }
        }
    }

    fn update_surface_size<F>(&self, size: PhysicalSize<u32>, event_handler: &mut F)
    where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        let content_rect = self
            .openharmony_app
            .content_rect_for(self.current_window.into_raw() as i64);
        let size = nonzero_size_or(physical_size(content_rect.width, content_rect.height), size);
        let changed = {
            let mut state = self.window_target.p.shared.lock();
            let changed = state.surface_size != size;
            state.surface_size = size;
            state.surface_position = PhysicalPosition::new(content_rect.left, content_rect.top);
            state.resize_settle.extend(Instant::now());
            if state.outer_size == PhysicalSize::new(0, 0) {
                state.outer_size = size;
            }
            changed
        };
        if changed {
            self.emit_window_event(
                self.current_window,
                event::WindowEvent::Resized(size),
                event_handler,
            );
        }
        self.window_target.p.shared.request_redraw();
    }

    fn update_configuration<F>(&self, configuration: &Configuration, event_handler: &mut F)
    where
        F: FnMut(event::Event<T>, &event_loop::ActiveEventLoop) + ?Sized,
    {
        let scale_factor = scale_factor(&self.openharmony_app);
        let theme = theme(configuration.color_mode);
        for (id, shared) in self.registered_windows() {
            let (old_scale, old_theme, current_size) = {
                let mut state = shared.lock();
                let old = (state.scale_factor, state.theme, state.surface_size);
                state.scale_factor = scale_factor;
                state.theme = theme;
                old
            };
            if (scale_factor - old_scale).abs() > f64::EPSILON {
                let requested_size = Arc::new(Mutex::new(current_size));
                self.emit_window_event(
                    id,
                    event::WindowEvent::ScaleFactorChanged {
                        inner_size_writer: InnerSizeWriter::new(Arc::downgrade(&requested_size)),
                        scale_factor,
                    },
                    event_handler,
                );
                if *requested_size.lock().unwrap() != current_size {
                    trace!("ignoring a surface-size request during an OHOS scale-factor change");
                }
            }
            if theme != old_theme {
                if let Some(theme) = theme {
                    self.emit_window_event(
                        id,
                        event::WindowEvent::ThemeChanged(theme),
                        event_handler,
                    );
                }
            }
            shared.request_redraw();
        }
    }
}
pub struct EventLoopProxy<T: 'static> {
    user_events_sender: mpsc::Sender<T>,
    main_wake: Arc<main_wake::MainWake>,
}

impl<T: 'static> Clone for EventLoopProxy<T> {
    fn clone(&self) -> Self {
        Self {
            user_events_sender: self.user_events_sender.clone(),
            main_wake: self.main_wake.clone(),
        }
    }
}

impl<T: 'static> EventLoopProxy<T> {
    pub fn send_event(&self, event: T) -> Result<(), event_loop::EventLoopClosed<T>> {
        self.user_events_sender
            .send(event)
            .map_err(|err| event_loop::EventLoopClosed(err.0))?;
        self.main_wake.notify();
        Ok(())
    }
}

pub struct ActiveEventLoop {
    pub(crate) app: OpenHarmonyApp,
    main_wake: Arc<main_wake::MainWake>,
    visible: Cell<bool>,
    control_flow: Cell<ControlFlow>,
    exit: Cell<bool>,
    shared: Arc<SharedWindowState>,
    windows: Mutex<HashMap<WindowId, Arc<SharedWindowState>>>,
    drag_drop: Mutex<drag_drop::DragDrop>,
    root_claimed: Arc<AtomicBool>,
}

impl ActiveEventLoop {
    pub fn create_custom_cursor(&self, source: CustomCursorSource) -> CustomCursor {
        CustomCursor {
            inner: PlatformCustomCursor(Arc::new(source.inner.0)),
        }
    }

    pub fn available_monitors(&self) -> VecDeque<MonitorHandle> {
        monitor::all()
    }

    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        monitor::primary()
    }

    #[cfg(feature = "rwh_05")]
    #[inline]
    pub fn raw_display_handle_rwh_05(&self) -> rwh_05::RawDisplayHandle {
        unreachable!("rwh_05 is not supported on OpenHarmony");
    }

    pub fn system_theme(&self) -> Option<Theme> {
        self.shared.lock().theme
    }

    pub fn listen_device_events(&self, _allowed: DeviceEvents) {}

    pub fn set_control_flow(&self, control_flow: ControlFlow) {
        self.control_flow.set(control_flow)
    }

    pub fn control_flow(&self) -> ControlFlow {
        self.control_flow.get()
    }

    pub fn exit(&self) {
        self.exit.set(true)
    }

    pub fn clear_exit(&self) {
        self.exit.set(false)
    }

    pub fn exiting(&self) -> bool {
        self.exit.get()
    }

    pub(crate) fn owned_display_handle(&self) -> OwnedDisplayHandle {
        OwnedDisplayHandle
    }

    #[cfg(feature = "rwh_06")]
    #[inline]
    pub fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        Ok(rwh_06::RawDisplayHandle::Ohos(
            rwh_06::OhosDisplayHandle::new(),
        ))
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct OwnedDisplayHandle;

impl OwnedDisplayHandle {
    #[cfg(feature = "rwh_05")]
    #[inline]
    pub fn raw_display_handle_rwh_05(&self) -> rwh_05::RawDisplayHandle {
        unreachable!("rwh_05 is not supported on OpenHarmony");
    }

    #[cfg(feature = "rwh_06")]
    #[inline]
    pub fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        Ok(rwh_06::OhosDisplayHandle::new().into())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PlatformSpecificWindowAttributes;

pub struct Window {
    app: OpenHarmonyApp,
    shared: Arc<SharedWindowState>,
    id: WindowId,
    root_claimed: Arc<AtomicBool>,
    owns_window: bool,
}

impl Window {
    pub(crate) fn new(
        el: &ActiveEventLoop,
        attrs: window::WindowAttributes,
    ) -> Result<Self, error::OsError> {
        let root = !el.root_claimed.swap(true, Ordering::AcqRel);
        let id = if root {
            GLOBAL_WINDOW
        } else {
            let size = attrs
                .inner_size
                .unwrap_or(Size::Physical(PhysicalSize::new(800, 600)))
                .to_physical::<u32>(scale_factor(&el.app));
            let position = attrs
                .position
                .unwrap_or(Position::Physical(PhysicalPosition::new(100, 100)))
                .to_physical::<i32>(scale_factor(&el.app));
            let id = openharmony_ability::window::create_os_window(
                openharmony_ability::window::WindowCreateParams {
                    name: attrs.title.clone(),
                    native_module_name: el.app.module_name(),
                    width: size.width as i32,
                    height: size.height as i32,
                    x: position.x,
                    y: position.y,
                    decorations: attrs.decorations,
                    transparent: attrs.transparent,
                    ..Default::default()
                },
            )
            .map_err(|err| os_error!(OsError::from_msg(err.to_string())))?;
            WindowId::from_raw(id as usize)
        };
        let shared = if root {
            el.windows
                .lock()
                .unwrap()
                .get(&GLOBAL_WINDOW)
                .unwrap()
                .clone()
        } else {
            let shared = Arc::new(SharedWindowState::new(
                &el.app,
                id.into_raw() as i64,
                el.visible.get(),
            ));
            shared.frame.sync_delivery(&el.app, id.into_raw() as i64);
            shared.lock().scale_factor = scale_factor(&el.app);
            el.windows.lock().unwrap().insert(id, shared.clone());
            shared
        };
        let window = Self {
            app: el.app.clone(),
            shared,
            id,
            root_claimed: el.root_claimed.clone(),
            owns_window: true,
        };
        if root {
            window.configure(attrs);
        } else {
            window.shared.lock().initial_attributes = Some(attrs);
        }
        Ok(window)
    }

    pub(crate) fn maybe_queue_on_main(&self, f: impl FnOnce(&Self) + Send + 'static) {
        f(self)
    }

    pub(crate) fn maybe_wait_on_main<R: Send>(&self, f: impl FnOnce(&Self) -> R + Send) -> R {
        f(self)
    }

    fn configure(&self, attrs: WindowAttributes) {
        let window = self;
        window.set_title(&attrs.title);
        if attrs.window_icon.is_some() {
            window.set_window_icon(attrs.window_icon.clone());
        }
        window.set_resizable(attrs.resizable);
        window.set_decorations(attrs.decorations);
        window.set_enabled_buttons(attrs.enabled_buttons);
        window.set_min_inner_size(attrs.min_inner_size);
        window.set_max_inner_size(attrs.max_inner_size);
        if attrs.transparent {
            window.set_transparent(true);
        }
        if attrs.blur {
            window.set_blur(true);
        }
        if attrs.maximized {
            window.set_maximized(true);
        }
        if let Some(fullscreen) = attrs.fullscreen {
            window.set_fullscreen(Some(fullscreen.into()));
        }
        if let Some(size) = attrs.inner_size {
            let _ = window.request_inner_size(size);
        }
        if let Some(position) = attrs.position {
            window.set_outer_position(position);
        }
        window.command("resolve window id", |client, state, id| async move {
            let id = client.get_real_window_id(id).await?;
            state.lock().real_window_id = i32::try_from(id).ok();
            Ok(())
        });
    }

    fn command<F, Fut>(&self, name: &'static str, operation: F)
    where
        F: FnOnce(plugins::window::WindowClient, Arc<SharedWindowState>, i64) -> Fut
            + Send
            + 'static,
        Fut: std::future::Future<Output = openharmony_ability::napi_ohos::Result<()>>
            + Send
            + 'static,
    {
        let client = plugins::window::WindowClient::new(&self.app);
        let state = self.shared.clone();
        let id = self.id.into_raw() as i64;
        self.shared.commands.push(async move {
            let result = match client {
                Ok(client) => operation(client, state, id).await,
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                warn!("OHOS {name}: {error}");
            }
        });
    }

    fn submit_geometry(&self, target: GeometryTarget) {
        {
            let state = self.shared.lock();
            if state.dragging.is_none()
                || !state.presentable()
                || !self.shared.geometry.submit(target)
            {
                return;
            }
        }
        let shared = self.shared.clone();
        let client = plugins::window::WindowClient::new(&self.app);
        let id = self.id.into_raw() as i64;
        self.shared.commands.push(async move {
            while let Some(target) = shared.geometry.take() {
                let result = async {
                    let client = client.as_ref().map_err(|error| {
                        openharmony_ability::napi_ohos::Error::from_reason(error.to_string())
                    })?;
                    client
                        .move_window_to(id, target.position.x.into(), target.position.y.into())
                        .await?;
                    if let Some(size) = target.size {
                        let border = {
                            let state = shared.lock();
                            PhysicalSize::new(
                                state
                                    .outer_size
                                    .width
                                    .saturating_sub(state.surface_size.width),
                                state
                                    .outer_size
                                    .height
                                    .saturating_sub(state.surface_size.height),
                            )
                        };
                        client
                            .resize_window(
                                id,
                                i64::from(size.width) + i64::from(border.width),
                                i64::from(size.height) + i64::from(border.height),
                            )
                            .await?;
                    }
                    Ok::<_, openharmony_ability::napi_ohos::Error>(())
                }
                .await;
                if let Err(error) = result {
                    warn!("OHOS drag geometry: {error}");
                }
            }
        });
    }

    fn apply_limits(&self) {
        let state = self.shared.lock();
        let min = state.min_size.unwrap_or_default();
        let max = state.max_size.unwrap_or_default();
        drop(state);
        self.command("window size limits", move |client, _, id| async move {
            client
                .set_window_limits(
                    id,
                    min.width.into(),
                    min.height.into(),
                    max.width.into(),
                    max.height.into(),
                )
                .await
        });
    }

    fn apply_buttons(&self) {
        let state = self.shared.lock();
        let buttons = state.enabled_buttons;
        let flags = i32::from(buttons.contains(WindowButtons::CLOSE))
            | (i32::from(buttons.contains(WindowButtons::MAXIMIZE)) << 1)
            | (i32::from(buttons.contains(WindowButtons::MINIMIZE)) << 2)
            | (i32::from(state.resizable) << 3);
        drop(state);
        self.command("window buttons", move |client, _, id| async move {
            client.set_window_decoration_flags(id, flags).await
        });
    }

    pub fn id(&self) -> WindowId {
        self.id
    }

    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        monitor::primary()
    }

    pub fn available_monitors(&self) -> VecDeque<MonitorHandle> {
        monitor::all()
    }

    pub fn current_monitor(&self) -> Option<MonitorHandle> {
        let id = self
            .shared
            .lock()
            .real_window_id
            .and_then(|id| {
                ohos_window_manager_binding::Window::from_id(id)
                    .properties()
                    .ok()
            })
            .map(|p| u64::from(p.display_id));
        monitor::all()
            .into_iter()
            .find(|monitor| Some(monitor.native_id()) == id)
            .or_else(monitor::primary)
    }

    pub fn scale_factor(&self) -> f64 {
        self.shared.lock().scale_factor
    }

    pub fn request_redraw(&self) {
        self.shared.request_redraw();
    }

    pub fn pre_present_notify(&self) {}

    pub fn inner_position(&self) -> Result<PhysicalPosition<i32>, error::NotSupportedError> {
        Ok(self.shared.lock().surface_position)
    }

    pub fn outer_position(&self) -> Result<PhysicalPosition<i32>, error::NotSupportedError> {
        Ok(self.shared.lock().position)
    }

    pub fn set_outer_position(&self, position: Position) {
        let position = position.to_physical::<i32>(self.scale_factor());
        self.command("move window", move |client, _, id| async move {
            client
                .move_window_to(id, position.x.into(), position.y.into())
                .await
        });
    }

    pub fn inner_size(&self) -> PhysicalSize<u32> {
        self.shared.lock().surface_size
    }

    pub fn request_inner_size(&self, size: Size) -> Option<PhysicalSize<u32>> {
        let size = size.to_physical::<u32>(self.scale_factor());
        let state = self.shared.lock();
        let border = PhysicalSize::new(
            state
                .outer_size
                .width
                .saturating_sub(state.surface_size.width),
            state
                .outer_size
                .height
                .saturating_sub(state.surface_size.height),
        );
        drop(state);
        self.command("resize window", move |client, _, id| async move {
            client
                .resize_window(
                    id,
                    i64::from(size.width) + i64::from(border.width),
                    i64::from(size.height) + i64::from(border.height),
                )
                .await
        });
        None
    }

    pub fn outer_size(&self) -> PhysicalSize<u32> {
        self.shared.lock().outer_size
    }

    pub fn set_min_inner_size(&self, size: Option<Size>) {
        let size = size.map(|size| size.to_physical(self.scale_factor()));
        self.shared.lock().min_size = size;
        self.apply_limits();
    }

    pub fn set_max_inner_size(&self, size: Option<Size>) {
        let size = size.map(|size| size.to_physical(self.scale_factor()));
        self.shared.lock().max_size = size;
        self.apply_limits();
    }

    pub fn resize_increments(&self) -> Option<PhysicalSize<u32>> {
        self.shared.lock().resize_increments
    }

    pub fn set_resize_increments(&self, increments: Option<Size>) {
        let increments = increments.map(|size| size.to_physical(self.scale_factor()));
        self.shared.lock().resize_increments = increments;
    }

    pub fn set_title(&self, title: &str) {
        let title = title.to_owned();
        self.command("window title", move |client, state, id| async move {
            client.set_window_title(id, title.clone()).await?;
            state.lock().title = title;
            Ok(())
        });
    }

    pub fn set_transparent(&self, transparent: bool) {
        self.command("window background", move |client, _, id| async move {
            client
                .set_window_background_color(id, if transparent { 0 } else { 0xff000000 })
                .await
        });
    }

    pub fn set_blur(&self, blur: bool) {
        self.command("window blur", move |client, _, id| async move {
            client
                .set_window_blur(id, if blur { 30.0 } else { 0.0 })
                .await
        });
    }

    pub fn set_visible(&self, visible: bool) {
        self.command("window visibility", move |client, _, id| async move {
            if visible {
                client.show_window(id).await
            } else {
                client.minimize_window(id).await
            }
        });
    }

    pub fn is_visible(&self) -> Option<bool> {
        None
    }

    pub fn set_resizable(&self, resizable: bool) {
        self.shared.lock().resizable = resizable;
        self.apply_buttons();
    }

    pub fn is_resizable(&self) -> bool {
        self.shared.lock().resizable
    }

    pub fn set_enabled_buttons(&self, buttons: WindowButtons) {
        self.shared.lock().enabled_buttons = buttons;
        self.apply_buttons();
    }

    pub fn enabled_buttons(&self) -> WindowButtons {
        self.shared.lock().enabled_buttons
    }

    pub fn set_minimized(&self, minimized: bool) {
        self.command("minimize window", move |client, _, id| async move {
            if minimized {
                client.minimize_window(id).await?;
            } else {
                client.show_window(id).await?;
            }
            Ok(())
        });
    }

    pub fn is_minimized(&self) -> Option<bool> {
        Some(self.shared.lock().minimized)
    }

    pub fn set_maximized(&self, maximized: bool) {
        self.command("maximize window", move |client, state, id| async move {
            if maximized {
                client.maximize_window(id).await?;
            } else {
                client.recover_window(id).await?;
            }
            state.lock().maximized = maximized;
            Ok(())
        });
    }

    pub fn is_maximized(&self) -> bool {
        self.shared.lock().maximized
    }

    pub fn set_fullscreen(&self, fullscreen: Option<Fullscreen>) {
        self.command("fullscreen", move |client, state, id| async move {
            client.set_fullscreen(id, fullscreen.is_some()).await?;
            state.lock().fullscreen = fullscreen;
            Ok(())
        });
    }

    pub fn fullscreen(&self) -> Option<Fullscreen> {
        self.shared.lock().fullscreen.clone()
    }

    pub fn set_decorations(&self, decorations: bool) {
        self.command("decorations", move |client, state, id| async move {
            client.set_window_decorations(id, decorations).await?;
            state.lock().decorations = decorations;
            Ok(())
        });
    }

    pub fn is_decorated(&self) -> bool {
        self.shared.lock().decorations
    }

    pub fn set_window_level(&self, level: WindowLevel) {
        self.command("window level", move |client, _, id| async move {
            client
                .set_window_topmost(id, level == WindowLevel::AlwaysOnTop)
                .await
        });
    }

    pub fn set_window_icon(&self, window_icon: Option<crate::icon::Icon>) {
        if self.id != GLOBAL_WINDOW {
            warn!("OHOS child windows share their Ability icon");
            return;
        }
        let (size, rgba) = if let Some(icon) = window_icon {
            let icon = &icon.inner;
            let size = icon.width.max(icon.height);
            if size > 1024 {
                warn!("OHOS window icon exceeds 1024 pixels");
                return;
            }
            let mut rgba = vec![0; (size * size * 4) as usize];
            let offset_x = (size - icon.width) / 2;
            let offset_y = (size - icon.height) / 2;
            for y in 0..icon.height {
                let start = ((y + offset_y) * size + offset_x) as usize * 4;
                let source = (y * icon.width) as usize * 4;
                rgba[start..start + icon.width as usize * 4]
                    .copy_from_slice(&icon.rgba[source..source + icon.width as usize * 4]);
            }
            (size, rgba)
        } else {
            (0, Vec::new())
        };
        self.command("window icon", move |client, _, id| async move {
            client
                .set_window_icon(plugins::window::WindowIconRequest {
                    window_id: id,
                    size,
                    rgba,
                })
                .await
        });
    }

    pub fn set_ime_cursor_area(&self, position: Position, size: Size) {
        use openharmony_ability::ime::Rect as ImeRect;
        let state = self.shared.lock();
        let pos = position.to_physical::<f64>(state.scale_factor);
        let sz = size.to_physical::<f64>(state.scale_factor);
        let rect = ImeRect {
            left: f64::from(state.position.x + state.surface_position.x) + pos.x,
            top: f64::from(state.position.y + state.surface_position.y) + pos.y,
            width: sz.width,
            height: sz.height,
        };
        drop(state);
        self.shared.lock().ime_cursor_area = Some((position, size));
        if let Err(error) = self
            .app
            .update_ime_cursor_for(self.id.into_raw() as i64, rect)
        {
            warn!("Cannot position OHOS IME: {error}");
        }
    }

    pub fn set_ime_allowed(&self, allowed: bool) {
        self.shared.lock().ime_allowed = allowed;
        let window_id = self.id.into_raw() as i64;
        if allowed {
            if let Err(error) = self.app.try_show_keyboard_for(window_id) {
                warn!("Cannot show OHOS keyboard: {error}");
            }
        } else {
            self.app.hide_keyboard_for(window_id);
        }
    }

    pub fn set_ime_purpose(&self, purpose: ImePurpose) {
        use openharmony_ability::ime::{EnterKey, InputType};
        self.shared.lock().ime_purpose = purpose;
        let input_type = match purpose {
            ImePurpose::Password => InputType::VisiblePassword,
            _ => InputType::Text,
        };
        if let Err(error) = self.app.configure_ime_for(
            self.id.into_raw() as i64,
            EnterKey::Done,
            input_type,
        ) {
            warn!("Cannot configure OHOS IME: {error}");
        }
    }

    pub fn set_ime_surrounding_text(&self, text: String, cursor: usize, anchor: usize) {
        let end_cursor = cursor.min(text.len());
        let end_anchor = anchor.min(text.len());
        let cursor_u16 = text[..end_cursor].encode_utf16().count();
        let anchor_u16 = text[..end_anchor].encode_utf16().count();
        self.shared.lock().ime_surrounding_text = Some((text.clone(), cursor, anchor));
        let _ = self.app.update_ime_text_state_for(
            self.id.into_raw() as i64,
            openharmony_ability::ime::TextState {
                text,
                offset: 0,
                selection: cursor_u16.min(anchor_u16)..cursor_u16.max(anchor_u16),
                reversed: cursor_u16 < anchor_u16,
            },
        );
    }

    pub fn focus_window(&self) {
        self.command("focus window", |client, _, id| async move {
            client.focus_window(id).await
        });
    }

    pub fn request_user_attention(&self, request_type: Option<window::UserAttentionType>) {
        if request_type.is_some() {
            self.command("request attention", |client, _, id| async move {
                client.request_user_attention(id).await
            });
        }
    }

    pub fn set_cursor(&self, cursor: Cursor) {
        let key = match &cursor {
            Cursor::Icon(icon) => CursorKey::Icon(cursor::style(*icon)),
            Cursor::Custom(custom) => {
                if custom.inner.0.width > 256 || custom.inner.0.height > 256 {
                    return;
                }
                CursorKey::Custom(custom.clone())
            }
        };
        let generation = {
            let mut state = self.shared.lock();
            if state.cursor_pending.as_ref() == Some(&key)
                || (state.cursor_pending.is_none() && state.cursor_key.as_ref() == Some(&key))
            {
                return;
            }
            state.cursor_pending = Some(key.clone());
            state.cursor_generation
        };
        let client = plugins::window::WindowClient::new(&self.app);
        let shared = self.shared.clone();
        let id = self.id.into_raw() as i64;
        self.shared.commands.push(async move {
            let result = async {
                let client = client?;
                match cursor {
                    Cursor::Icon(icon) => client.set_cursor_icon(id, cursor::style(icon)).await,
                    Cursor::Custom(custom) => {
                        let image = &custom.inner.0;
                        client
                            .set_custom_cursor(plugins::window::CustomCursorRequest {
                                window_id: id,
                                rgba: image.rgba.clone(),
                                width: u32::from(image.width),
                                height: u32::from(image.height),
                                hotspot_x: u32::from(image.hotspot_x),
                                hotspot_y: u32::from(image.hotspot_y),
                            })
                            .await
                    }
                }
            }
            .await;
            let mut state = shared.lock();
            match result {
                Ok(()) if state.cursor_generation == generation => {
                    state.cursor_key = Some(key.clone());
                    if state.cursor_pending.as_ref() == Some(&key) {
                        state.cursor_pending = None;
                    }
                }
                Err(error) => {
                    state.invalidate_cursor();
                    warn!("OHOS cursor: {error}");
                }
                _ => {}
            }
        });
    }

    pub fn set_cursor_position(&self, _: Position) -> Result<(), error::ExternalError> {
        Err(error::ExternalError::NotSupported(
            error::NotSupportedError::new(),
        ))
    }

    pub fn set_cursor_grab(&self, mode: CursorGrabMode) -> Result<(), error::ExternalError> {
        let id = self
            .shared
            .lock()
            .real_window_id
            .ok_or_else(|| error::ExternalError::NotSupported(error::NotSupportedError::new()))?;
        let window = ohos_window_manager_binding::Window::from_id(id);
        let result = match mode {
            CursorGrabMode::None => window.unlock_cursor(),
            CursorGrabMode::Confined => window.lock_cursor(true),
            CursorGrabMode::Locked => window.lock_cursor(false),
        };
        result.map_err(|error| {
            error::ExternalError::Os(os_error!(OsError::from_msg(error.to_string())))
        })
    }

    pub fn set_cursor_visible(&self, visible: bool) {
        self.command("cursor visibility", move |client, _, _id| async move {
            client.set_cursor_visible(visible).await
        });
    }

    pub fn drag_window(&self) -> Result<(), error::ExternalError> {
        let mut state = self.shared.lock();
        state.dragging = Some(WindowDrag {
            direction: None,
            pointer: PhysicalPosition::new(
                state.pointer_position.x + f64::from(state.position.x),
                state.pointer_position.y + f64::from(state.position.y),
            ),
            position: state.position,
            size: state.surface_size,
        });
        Ok(())
    }

    pub fn drag_resize_window(
        &self,
        direction: ResizeDirection,
    ) -> Result<(), error::ExternalError> {
        if !self.is_resizable() {
            return Err(error::ExternalError::NotSupported(
                error::NotSupportedError::new(),
            ));
        }
        self.drag_window()?;
        if let Some(drag) = &mut self.shared.lock().dragging {
            drag.direction = Some(direction);
        }
        Ok(())
    }

    #[inline]
    pub fn show_window_menu(&self, position: Position) {
        use plugins::window::{WindowClient, WindowPromptAnchor, WindowPromptRequest};
        let position = position.to_logical::<f64>(self.scale_factor());
        let app = self.app.clone();
        let id = self.id.into_raw() as i64;
        let maximized = self.is_maximized();
        let fullscreen = self.fullscreen().is_some();
        task::spawn(async move {
            let result = async {
                let client = WindowClient::new(&app)?;
                let selected = client
                    .show_prompt(WindowPromptRequest {
                        window_id: id,
                        message: "Window".into(),
                        detail: None,
                        level: 0,
                        appearance: None,
                        buttons: vec![
                            "Minimize".into(),
                            if maximized { "Restore" } else { "Maximize" }.into(),
                            if fullscreen {
                                "Leave fullscreen"
                            } else {
                                "Fullscreen"
                            }
                            .into(),
                            "Close".into(),
                            "Cancel".into(),
                        ],
                        cancel_index: Some(4),
                        anchor: Some(WindowPromptAnchor {
                            x: position.x.max(0.0),
                            y: position.y.max(0.0),
                        }),
                    })
                    .await?;
                match selected {
                    0 => client.minimize_window(id).await?,
                    1 if maximized => client.recover_window(id).await?,
                    1 => client.maximize_window(id).await?,
                    2 => client.set_fullscreen(id, !fullscreen).await?,
                    3 => {
                        if id != 0 {
                            client.destroy_window(id).await?;
                        }
                        openharmony_ability::notify_window_close(id as i32);
                        frame::main_wake(&app).notify();
                    }
                    _ => {}
                }
                Ok::<_, openharmony_ability::napi_ohos::Error>(())
            }
            .await;
            if let Err(error) = result {
                warn!("OHOS window menu: {error}");
            }
        });
    }

    pub fn set_cursor_hittest(&self, hittest: bool) -> Result<(), error::ExternalError> {
        self.command("cursor hit test", move |client, _, id| async move {
            client.set_window_touchable(id, hittest).await
        });
        Ok(())
    }

    pub fn set_theme(&self, theme: Option<Theme>) {
        use plugins::app_control::{
            AppControlBridgePlugin, SetColorModeRequest, SetColorModeResponse,
        };
        let bridge = self.app.bridge();
        task::spawn(async move {
            let result = async {
                bridge?
                    .call_sync_from_worker::<
                        AppControlBridgePlugin,
                        SetColorModeRequest,
                        SetColorModeResponse,
                    >(
                        "set-color-mode",
                        SetColorModeRequest {
                            color_mode: match theme {
                                Some(Theme::Dark) => 0,
                                Some(Theme::Light) => 1,
                                None => 2,
                            },
                        },
                    )
                    .await
            }
            .await;
            if let Err(error) = result {
                warn!("OHOS color mode: {error}");
            }
        });
    }

    pub fn theme(&self) -> Option<Theme> {
        self.shared.lock().theme
    }

    pub fn set_content_protected(&self, protected: bool) {
        self.command("content protection", move |client, _, id| async move {
            let id = client.get_real_window_id(id).await?;
            ohos_window_manager_binding::Window::from_id(id as i32)
                .set_privacy_mode(protected)
                .map_err(|error| {
                    openharmony_ability::napi_ohos::Error::from_reason(error.to_string())
                })
        });
    }

    pub fn has_focus(&self) -> bool {
        self.shared.lock().focused
    }

    pub fn title(&self) -> String {
        self.shared.lock().title.clone()
    }

    pub fn reset_dead_keys(&self) {}

    pub fn take_ime_edits(&self) -> Vec<ImeEdit> {
        std::mem::take(&mut self.shared.lock().ime_edits)
    }

    pub fn config(&self) -> Configuration {
        self.app.config()
    }

    pub fn content_rect(&self) -> Rect {
        self.app.content_rect_for(self.id.into_raw() as i64)
    }

    pub fn openharmony_app(&self) -> &OpenHarmonyApp {
        &self.app
    }

    #[cfg(feature = "rwh_04")]
    pub fn raw_window_handle_rwh_04(&self) -> rwh_04::RawWindowHandle {
        unreachable!("rwh_04 is not supported on OpenHarmony");
    }

    #[cfg(feature = "rwh_05")]
    pub fn raw_window_handle_rwh_05(&self) -> rwh_05::RawWindowHandle {
        unreachable!("rwh_05 is not supported on OpenHarmony");
    }

    #[cfg(feature = "rwh_05")]
    pub fn raw_display_handle_rwh_05(&self) -> rwh_05::RawDisplayHandle {
        unreachable!("rwh_05 is not supported on OpenHarmony");
    }

    #[cfg(feature = "rwh_06")]
    #[allow(deprecated)]
    pub fn raw_window_handle_rwh_06(&self) -> Result<rwh_06::RawWindowHandle, rwh_06::HandleError> {
        if !self.shared.lock().lifecycle.surface_available {
            return Err(rwh_06::HandleError::Unavailable);
        }
        if let Some(handle) = self
            .app
            .native_window_for(self.id.into_raw() as i64)
            .and_then(|window| window.raw_window_handle())
        {
            return Ok(handle);
        }

        tracing::trace!(
            "The OHOS native window is only available between Event::Resumed and \
             Event::Suspended callbacks"
        );
        Err(rwh_06::HandleError::Unavailable)
    }

    #[cfg(feature = "rwh_06")]
    pub fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        Ok(rwh_06::RawDisplayHandle::Ohos(
            rwh_06::OhosDisplayHandle::new(),
        ))
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        if !self.owns_window {
            return;
        }
        if self.id == GLOBAL_WINDOW {
            self.root_claimed.store(false, Ordering::Release);
        } else {
            {
                let mut state = self.shared.lock();
                if state.lifecycle.closed {
                    return;
                }
                state.lifecycle.dropped_by_app = true;
                state.dragging = None;
                self.shared.sync_active(&state);
                self.shared.geometry.cancel();
            }
            let wake = frame::main_wake(&self.app);
            self.command("destroy window", move |client, _, id| async move {
                client.destroy_window(id).await?;
                openharmony_ability::notify_window_close(id as i32);
                wake.notify();
                Ok(())
            });
        }
    }
}

fn physical_size(width: i32, height: i32) -> PhysicalSize<u32> {
    PhysicalSize::new(nonnegative_u32(width), nonnegative_u32(height))
}

fn scale_factor(app: &OpenHarmonyApp) -> f64 {
    sanitize_scale_factor(app.scale() as f64)
}

fn sanitize_scale_factor(scale_factor: f64) -> f64 {
    if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    }
}

fn normalized_force(force: f32) -> f64 {
    if force.is_finite() {
        force.clamp(0.0, 1.0) as f64
    } else {
        0.0
    }
}

fn touch_phase_from_arkui(action: UIInputAction) -> TouchPhase {
    match action {
        UIInputAction::Down => TouchPhase::Started,
        UIInputAction::Move => TouchPhase::Moved,
        UIInputAction::Up => TouchPhase::Ended,
        UIInputAction::Cancel => TouchPhase::Cancelled,
    }
}

fn touch_phase_from_gesture(phase: OhosGesturePhase) -> TouchPhase {
    match phase {
        OhosGesturePhase::Start => TouchPhase::Started,
        OhosGesturePhase::Update => TouchPhase::Moved,
        OhosGesturePhase::End => TouchPhase::Ended,
        OhosGesturePhase::Cancel => TouchPhase::Cancelled,
    }
}

fn nonnegative_u32(value: i32) -> u32 {
    u32::try_from(value).unwrap_or(0)
}

fn nonzero_size_or(preferred: PhysicalSize<u32>, fallback: PhysicalSize<u32>) -> PhysicalSize<u32> {
    if preferred.width == 0 || preferred.height == 0 {
        fallback
    } else {
        preferred
    }
}

fn theme(color_mode: ColorMode) -> Option<Theme> {
    match color_mode {
        ColorMode::Dark => Some(Theme::Dark),
        ColorMode::Light => Some(Theme::Light),
        ColorMode::NoSet => None,
    }
}

fn map_mouse_button(button: OhosMouseButton) -> MouseButton {
    match button {
        OhosMouseButton::LeftButton => MouseButton::Left,
        OhosMouseButton::RightButton => MouseButton::Right,
        OhosMouseButton::MiddleButton => MouseButton::Middle,
        OhosMouseButton::BackButton => MouseButton::Back,
        OhosMouseButton::ForwardButton => MouseButton::Forward,
        OhosMouseButton::NoneButton => MouseButton::Other(0),
    }
}

fn start_cause(control_flow: ControlFlow, start: Instant) -> StartCause {
    let now = Instant::now();
    match control_flow {
        ControlFlow::Poll => StartCause::Poll,
        ControlFlow::Wait => StartCause::WaitCancelled {
            start,
            requested_resume: None,
        },
        ControlFlow::WaitUntil(deadline) if now >= deadline => StartCause::ResumeTimeReached {
            start,
            requested_resume: deadline,
        },
        ControlFlow::WaitUntil(deadline) => StartCause::WaitCancelled {
            start,
            requested_resume: Some(deadline),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_sizes_do_not_wrap_negative_values() {
        assert_eq!(physical_size(-1, 20), PhysicalSize::new(0, 20));
    }

    #[test]
    fn invalid_scale_factors_fall_back_to_one() {
        assert_eq!(sanitize_scale_factor(0.0), 1.0);
        assert_eq!(sanitize_scale_factor(f64::NAN), 1.0);
        assert_eq!(sanitize_scale_factor(2.5), 2.5);
    }

    #[test]
    fn touch_force_is_finite_and_normalized() {
        assert_eq!(normalized_force(f32::NAN), 0.0);
        assert_eq!(normalized_force(-1.0), 0.0);
        assert_eq!(normalized_force(2.0), 1.0);
    }

    #[test]
    fn arkui_input_phases_map_to_winit_phases() {
        assert_eq!(
            touch_phase_from_arkui(UIInputAction::Down),
            TouchPhase::Started
        );
        assert_eq!(
            touch_phase_from_arkui(UIInputAction::Move),
            TouchPhase::Moved
        );
        assert_eq!(touch_phase_from_arkui(UIInputAction::Up), TouchPhase::Ended);
        assert_eq!(
            touch_phase_from_arkui(UIInputAction::Cancel),
            TouchPhase::Cancelled
        );
    }

    #[test]
    fn arkui_gesture_phases_map_to_winit_phases() {
        assert_eq!(
            touch_phase_from_gesture(OhosGesturePhase::Start),
            TouchPhase::Started
        );
        assert_eq!(
            touch_phase_from_gesture(OhosGesturePhase::Update),
            TouchPhase::Moved
        );
        assert_eq!(
            touch_phase_from_gesture(OhosGesturePhase::End),
            TouchPhase::Ended
        );
        assert_eq!(
            touch_phase_from_gesture(OhosGesturePhase::Cancel),
            TouchPhase::Cancelled
        );
    }

    #[test]
    fn color_mode_maps_to_winit_theme() {
        assert_eq!(theme(ColorMode::Dark), Some(Theme::Dark));
        assert_eq!(theme(ColorMode::Light), Some(Theme::Light));
        assert_eq!(theme(ColorMode::NoSet), None);
    }
}
