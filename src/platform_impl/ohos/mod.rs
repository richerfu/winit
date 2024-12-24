use std::borrow::{Borrow, BorrowMut};
use std::cell::Cell;
use std::hash::Hash;
use std::num::{NonZeroU16, NonZeroU32};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use tracing::{debug, trace, warn};

use openharmony_ability::{
    Configuration, Event as MainEvent, OpenHarmonyApp, OpenHarmonyWaker, Rect,
};

use crate::application::ApplicationHandler;
use crate::cursor::Cursor;
use crate::dpi::{PhysicalPosition, PhysicalSize, Position, Size};
use crate::error::{EventLoopError, NotSupportedError, RequestError};
use crate::event::{self, DeviceId, Force, StartCause, SurfaceSizeWriter};
use crate::event_loop::{
    ActiveEventLoop as RootActiveEventLoop, ControlFlow, DeviceEvents,
    EventLoopProxy as RootEventLoopProxy, OwnedDisplayHandle as RootOwnedDisplayHandle,
};
use crate::monitor::MonitorHandle as RootMonitorHandle;
use crate::window::{
    self, CursorGrabMode, CustomCursor, CustomCursorSource, Fullscreen, ImePurpose,
    ResizeDirection, Theme, Window as CoreWindow, WindowAttributes, WindowButtons, WindowId,
    WindowLevel,
};

// mod keycodes;

pub(crate) use crate::cursor::{
    NoCustomCursor as PlatformCustomCursor, NoCustomCursor as PlatformCustomCursorSource,
};
pub(crate) use crate::icon::NoIcon as PlatformIcon;

static HAS_FOCUS: AtomicBool = AtomicBool::new(true);

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct KeyEventExtra {}

pub struct EventLoop {
    pub(crate) openharmony_app: OpenHarmonyApp,
    window_target: ActiveEventLoop,
    running: bool,
    cause: StartCause,
    combining_accent: Option<char>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct PlatformSpecificEventLoopAttributes {
    pub(crate) openharmony_app: Option<OpenHarmonyApp>,
}

impl Default for PlatformSpecificEventLoopAttributes {
    fn default() -> Self {
        Self { openharmony_app: Default::default() }
    }
}

// Android currently only supports one window
const GLOBAL_WINDOW: WindowId = WindowId::from_raw(0);

impl EventLoop {
    pub(crate) fn new(
        attributes: &PlatformSpecificEventLoopAttributes,
    ) -> Result<Self, EventLoopError> {
        let proxy_wake_up = Arc::new(AtomicBool::new(false));

        let openharmony_app = attributes.openharmony_app.as_ref().expect(
            "An `OpenHarmonyApp` as passed to lib is required to create an `EventLoop` on \
             OpenHarmony or HarmonyNext",
        );

        Ok(Self {
            openharmony_app: openharmony_app.clone(),
            window_target: ActiveEventLoop {
                app: openharmony_app.clone(),
                control_flow: Cell::new(ControlFlow::default()),
                exit: Cell::new(false),
            },
            running: false,
            cause: StartCause::Init,
            combining_accent: None,
        })
    }

    pub(crate) fn window_target(&self) -> &dyn RootActiveEventLoop {
        &self.window_target
    }

    // fn handle_input_event<A: ApplicationHandler>(
    //     &mut self,
    //     android_app: &AndroidApp,
    //     event: &InputEvent<'_>,
    //     app: &mut A,
    // ) -> InputStatus {
    //     let mut input_status = InputStatus::Handled;
    //     match event {
    //         InputEvent::MotionEvent(motion_event) => {
    //             let device_id = Some(DeviceId::from_raw(motion_event.device_id() as i64));
    //             let action = motion_event.action();

    //             let pointers: Option<
    //                 Box<dyn Iterator<Item = openharmony_ability::input::Pointer<'_>>>,
    //             > = match action {
    //                 MotionAction::Down
    //                 | MotionAction::PointerDown
    //                 | MotionAction::Up
    //                 | MotionAction::PointerUp => Some(Box::new(std::iter::once(
    //                     motion_event.pointer_at_index(motion_event.pointer_index()),
    //                 ))),
    //                 MotionAction::Move | MotionAction::Cancel => {
    //                     Some(Box::new(motion_event.pointers()))
    //                 },
    //                 // TODO mouse events
    //                 _ => None,
    //             };

    //             if let Some(pointers) = pointers {
    //                 for pointer in pointers {
    //                     let tool_type = pointer.tool_type();
    //                     let position =
    //                         PhysicalPosition { x: pointer.x() as _, y: pointer.y() as _ };
    //                     trace!(
    //                         "Input event {device_id:?}, {action:?}, loc={position:?}, \
    //                          pointer={pointer:?}, tool_type={tool_type:?}"
    //                     );
    //                     let finger_id = event::FingerId(FingerId(pointer.pointer_id()));
    //                     let force = Some(Force::Normalized(pointer.pressure() as f64));

    //                     match action {
    //                         MotionAction::Down | MotionAction::PointerDown => {
    //                             let event = event::WindowEvent::PointerEntered {
    //                                 device_id,
    //                                 position,
    //                                 kind: match tool_type {
    //                                     android_activity::input::ToolType::Finger => {
    //                                         event::PointerKind::Touch(finger_id)
    //                                     },
    //                                     // TODO mouse events
    //                                     android_activity::input::ToolType::Mouse => continue,
    //                                     _ => event::PointerKind::Unknown,
    //                                 },
    //                             };
    //                             app.window_event(&self.window_target, GLOBAL_WINDOW, event);
    //                             let event = event::WindowEvent::PointerButton {
    //                                 device_id,
    //                                 state: event::ElementState::Pressed,
    //                                 position,
    //                                 button: match tool_type {
    //                                     android_activity::input::ToolType::Finger => {
    //                                         event::ButtonSource::Touch { finger_id, force }
    //                                     },
    //                                     // TODO mouse events
    //                                     android_activity::input::ToolType::Mouse => continue,
    //                                     _ => event::ButtonSource::Unknown(0),
    //                                 },
    //                             };
    //                             app.window_event(&self.window_target, GLOBAL_WINDOW, event);
    //                         },
    //                         MotionAction::Move => {
    //                             let event = event::WindowEvent::PointerMoved {
    //                                 device_id,
    //                                 position,
    //                                 source: match tool_type {
    //                                     android_activity::input::ToolType::Finger => {
    //                                         event::PointerSource::Touch { finger_id, force }
    //                                     },
    //                                     // TODO mouse events
    //                                     android_activity::input::ToolType::Mouse => continue,
    //                                     _ => event::PointerSource::Unknown,
    //                                 },
    //                             };
    //                             app.window_event(&self.window_target, GLOBAL_WINDOW, event);
    //                         },
    //                         MotionAction::Up | MotionAction::PointerUp | MotionAction::Cancel => {
    //                             if let MotionAction::Up | MotionAction::PointerUp = action {
    //                                 let event = event::WindowEvent::PointerButton {
    //                                     device_id,
    //                                     state: event::ElementState::Released,
    //                                     position,
    //                                     button: match tool_type {
    //                                         android_activity::input::ToolType::Finger => {
    //                                             event::ButtonSource::Touch { finger_id, force }
    //                                         },
    //                                         // TODO mouse events
    //                                         android_activity::input::ToolType::Mouse => continue,
    //                                         _ => event::ButtonSource::Unknown(0),
    //                                     },
    //                                 };
    //                                 app.window_event(&self.window_target, GLOBAL_WINDOW, event);
    //                             }

    //                             let event = event::WindowEvent::PointerLeft {
    //                                 device_id,
    //                                 position: Some(position),
    //                                 kind: match tool_type {
    //                                     android_activity::input::ToolType::Finger => {
    //                                         event::PointerKind::Touch(finger_id)
    //                                     },
    //                                     // TODO mouse events
    //                                     android_activity::input::ToolType::Mouse => continue,
    //                                     _ => event::PointerKind::Unknown,
    //                                 },
    //                             };
    //                             app.window_event(&self.window_target, GLOBAL_WINDOW, event);
    //                         },
    //                         _ => unreachable!(),
    //                     }
    //                 }
    //             }
    //         },
    //         InputEvent::KeyEvent(key) => {
    //             match key.key_code() {
    //                 // Flag keys related to volume as unhandled. While winit does not have a way for
    //                 // applications to configure what keys to flag as handled,
    //                 // this appears to be a good default until winit
    //                 // can be configured.
    //                 Keycode::VolumeUp | Keycode::VolumeDown | Keycode::VolumeMute
    //                     if self.ignore_volume_keys =>
    //                 {
    //                     input_status = InputStatus::Unhandled
    //                 },
    //                 keycode => {
    //                     let state = match key.action() {
    //                         KeyAction::Down => event::ElementState::Pressed,
    //                         KeyAction::Up => event::ElementState::Released,
    //                         _ => event::ElementState::Released,
    //                     };

    //                     let key_char = keycodes::character_map_and_combine_key(
    //                         android_app,
    //                         key,
    //                         &mut self.combining_accent,
    //                     );

    //                     let event = event::WindowEvent::KeyboardInput {
    //                         device_id: Some(DeviceId::from_raw(key.device_id() as i64)),
    //                         event: event::KeyEvent {
    //                             state,
    //                             physical_key: keycodes::to_physical_key(keycode),
    //                             logical_key: keycodes::to_logical(key_char, keycode),
    //                             location: keycodes::to_location(keycode),
    //                             repeat: key.repeat_count() > 0,
    //                             text: None,
    //                             platform_specific: KeyEventExtra {},
    //                         },
    //                         is_synthetic: false,
    //                     };

    //                     app.window_event(&self.window_target, GLOBAL_WINDOW, event);
    //                 },
    //             }
    //         },
    //         _ => {
    //             warn!("Unknown android_activity input event {event:?}")
    //         },
    //     }

    //     input_status
    // }

    pub fn run_app<A: ApplicationHandler>(self, mut app: A) -> Result<(), EventLoopError> {
        trace!("Mainloop iteration");

        let cause = self.cause;

        app.new_events(&self.window_target, cause);

        let openharmony_app = self.openharmony_app.clone();

        openharmony_app.run_loop(|event| {
            match event {
                MainEvent::SurfaceCreate { .. } => {
                    app.can_create_surfaces(&self.window_target);
                },
                MainEvent::SurfaceDestroy { .. } => {
                    app.destroy_surfaces(&self.window_target);
                },
                MainEvent::WindowResize { .. } => {
                    let win = self.openharmony_app.native_window();
                    let size = if let Some(win) = win {
                        PhysicalSize::new(win.width() as _, win.height() as _)
                    } else {
                        PhysicalSize::new(0, 0)
                    };
                    let event = event::WindowEvent::SurfaceResized(size);
                    app.window_event(&self.window_target, GLOBAL_WINDOW, event);
                },
                MainEvent::WindowRedraw { .. } => {
                    let event = event::WindowEvent::RedrawRequested;
                    app.window_event(&self.window_target, GLOBAL_WINDOW, event);
                },
                MainEvent::ContentRectChange { .. } => {
                    warn!("TODO: find a way to notify application of content rect change");
                },
                MainEvent::GainedFocus => {
                    HAS_FOCUS.store(true, Ordering::Relaxed);
                    let event = event::WindowEvent::Focused(true);
                    app.window_event(&self.window_target, GLOBAL_WINDOW, event);
                },
                MainEvent::LostFocus => {
                    HAS_FOCUS.store(false, Ordering::Relaxed);
                    let event = event::WindowEvent::Focused(false);
                    app.window_event(&self.window_target, GLOBAL_WINDOW, event);
                },
                MainEvent::ConfigChanged { .. } => {
                    let win = self.openharmony_app.native_window();
                    if let Some(win) = win {
                        let scale = self.openharmony_app.scale();
                        let width = win.width();
                        let height = win.height();
                        let new_surface_size =
                            Arc::new(Mutex::new(PhysicalSize::new(width as _, height as _)));
                        let event = event::WindowEvent::ScaleFactorChanged {
                            surface_size_writer: SurfaceSizeWriter::new(Arc::downgrade(
                                &new_surface_size,
                            )),
                            scale_factor: scale as _,
                        };
                        app.window_event(&self.window_target, GLOBAL_WINDOW, event);
                    }
                },
                MainEvent::LowMemory => {
                    app.memory_warning(&self.window_target);
                },
                MainEvent::Start => {
                    app.resumed(self.window_target());
                },
                MainEvent::Resume { .. } => {
                    debug!("App Resumed - is running");
                    // TODO: This is incorrect - will be solved in https://github.com/rust-windowing/winit/pull/3897
                    // self.running = true;
                },
                MainEvent::SaveState { .. } => {
                    // XXX: how to forward this state to applications?
                    // XXX: also how do we expose state restoration to apps?
                    warn!("TODO: forward saveState notification to application");
                },
                MainEvent::Pause => {
                    debug!("App Paused - stopped running");
                    // TODO: This is incorrect - will be solved in https://github.com/rust-windowing/winit/pull/3897
                    // self.running = false;
                },
                MainEvent::Stop => {
                    app.suspended(self.window_target());
                },
                MainEvent::Destroy => {
                    // XXX: maybe exit mainloop to drop things before being
                    // killed by the OS?
                    warn!("TODO: forward onDestroy notification to application");
                },
                MainEvent::Input(_) => {
                    warn!("TODO: forward onDestroy notification to application");
                    // let openharmony_app = self.openharmony_app.clone();
                    // self.handle_input_event(openharmony_app, event, app)
                },
                unknown => {
                    trace!("Unknown MainEvent {unknown:?} (ignored)");
                },
            }
        });

        Ok(())
    }

    fn control_flow(&self) -> ControlFlow {
        self.window_target.control_flow()
    }

    fn exiting(&self) -> bool {
        self.window_target.exiting()
    }
}

#[derive(Clone)]
pub struct EventLoopProxy {
    waker: OpenHarmonyWaker,
}

impl EventLoopProxy {
    pub fn wake_up(&self) {
        self.waker.wake();
    }
}

pub struct ActiveEventLoop {
    pub(crate) app: OpenHarmonyApp,
    control_flow: Cell<ControlFlow>,
    exit: Cell<bool>,
}

impl ActiveEventLoop {
    fn clear_exit(&self) {
        self.exit.set(false);
    }
}

impl RootActiveEventLoop for ActiveEventLoop {
    fn create_proxy(&self) -> RootEventLoopProxy {
        let event_loop_proxy = EventLoopProxy { waker: self.app.create_waker() };
        RootEventLoopProxy { event_loop_proxy }
    }

    fn create_window(
        &self,
        window_attributes: WindowAttributes,
    ) -> Result<Box<dyn CoreWindow>, RequestError> {
        Ok(Box::new(Window::new(self, window_attributes)?))
    }

    fn create_custom_cursor(
        &self,
        _source: CustomCursorSource,
    ) -> Result<CustomCursor, RequestError> {
        Err(NotSupportedError::new("create_custom_cursor is not supported").into())
    }

    fn available_monitors(&self) -> Box<dyn Iterator<Item = RootMonitorHandle>> {
        Box::new(std::iter::empty())
    }

    fn primary_monitor(&self) -> Option<RootMonitorHandle> {
        None
    }

    fn system_theme(&self) -> Option<Theme> {
        None
    }

    fn listen_device_events(&self, _allowed: DeviceEvents) {}

    fn set_control_flow(&self, control_flow: ControlFlow) {
        self.control_flow.set(control_flow)
    }

    fn control_flow(&self) -> ControlFlow {
        self.control_flow.get()
    }

    fn exit(&self) {
        self.exit.set(true)
    }

    fn exiting(&self) -> bool {
        self.exit.get()
    }

    fn owned_display_handle(&self) -> RootOwnedDisplayHandle {
        RootOwnedDisplayHandle { platform: OwnedDisplayHandle }
    }

    #[cfg(feature = "rwh_06")]
    fn rwh_06_handle(&self) -> &dyn rwh_06::HasDisplayHandle {
        self
    }
}

#[cfg(feature = "rwh_06")]
impl rwh_06::HasDisplayHandle for ActiveEventLoop {
    fn display_handle(&self) -> Result<rwh_06::DisplayHandle<'_>, rwh_06::HandleError> {
        let raw = rwh_06::OhosDisplayHandle::new();
        Ok(unsafe { rwh_06::DisplayHandle::borrow_raw(raw.into()) })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct OwnedDisplayHandle;

impl OwnedDisplayHandle {
    #[cfg(feature = "rwh_06")]
    #[inline]
    pub fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        Ok(rwh_06::OhosDisplayHandle::new().into())
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FingerId(i32);

impl FingerId {
    #[cfg(test)]
    pub const fn dummy() -> Self {
        FingerId(0)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PlatformSpecificWindowAttributes;

pub(crate) struct Window {
    app: OpenHarmonyApp,
}

impl Window {
    pub(crate) fn new(
        el: &ActiveEventLoop,
        _window_attrs: window::WindowAttributes,
    ) -> Result<Self, RequestError> {
        // FIXME this ignores requested window attributes

        Ok(Self { app: el.app.clone() })
    }

    pub fn config(&self) -> Configuration {
        self.app.config()
    }

    pub fn content_rect(&self) -> Rect {
        self.app.content_rect()
    }

    #[cfg(feature = "rwh_06")]
    // Allow the usage of HasRawWindowHandle inside this function
    #[allow(deprecated)]
    fn raw_window_handle_rwh_06(&self) -> Result<rwh_06::RawWindowHandle, rwh_06::HandleError> {
        if let Some(native_window) = self.app.native_window().as_ref() {
            if let Some(window) = native_window.raw_window_handle() {
                Ok(window)
            } else {
                tracing::error!("Cannot get the native window handle, it's null.");
                Err(rwh_06::HandleError::Unavailable)
            }
        } else {
            tracing::error!(
                "Cannot get the native window, it's null and will always be null before \
                 Event::Resumed and after Event::Suspended. Make sure you only call this function \
                 between those events."
            );
            Err(rwh_06::HandleError::Unavailable)
        }
    }

    #[cfg(feature = "rwh_06")]
    fn raw_display_handle_rwh_06(&self) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        Ok(rwh_06::RawDisplayHandle::Ohos(rwh_06::OhosDisplayHandle::new()))
    }
}

#[cfg(feature = "rwh_06")]
impl rwh_06::HasDisplayHandle for Window {
    fn display_handle(&self) -> Result<rwh_06::DisplayHandle<'_>, rwh_06::HandleError> {
        let raw = self.raw_display_handle_rwh_06()?;
        unsafe { Ok(rwh_06::DisplayHandle::borrow_raw(raw)) }
    }
}

#[cfg(feature = "rwh_06")]
impl rwh_06::HasWindowHandle for Window {
    fn window_handle(&self) -> Result<rwh_06::WindowHandle<'_>, rwh_06::HandleError> {
        let raw = self.raw_window_handle_rwh_06()?;
        unsafe { Ok(rwh_06::WindowHandle::borrow_raw(raw)) }
    }
}

impl CoreWindow for Window {
    fn id(&self) -> WindowId {
        GLOBAL_WINDOW
    }

    fn request_redraw(&self) {}
    fn scale_factor(&self) -> f64 {
        1.0
    }

    fn primary_monitor(&self) -> Option<RootMonitorHandle> {
        None
    }

    fn available_monitors(&self) -> Box<dyn Iterator<Item = RootMonitorHandle>> {
        Box::new(std::iter::empty())
    }

    fn current_monitor(&self) -> Option<RootMonitorHandle> {
        None
    }

    fn pre_present_notify(&self) {}

    fn inner_position(&self) -> Result<PhysicalPosition<i32>, RequestError> {
        Err(NotSupportedError::new("inner_position is not supported").into())
    }

    fn outer_position(&self) -> Result<PhysicalPosition<i32>, RequestError> {
        Err(NotSupportedError::new("outer_position is not supported").into())
    }

    fn set_outer_position(&self, _position: Position) {
        // no effect
    }

    fn surface_size(&self) -> PhysicalSize<u32> {
        // self.outer_size()
        PhysicalSize { width: 1080, height: 2720 }
    }

    fn request_surface_size(&self, _size: Size) -> Option<PhysicalSize<u32>> {
        // Some(self.surface_size())
        None
    }

    fn outer_size(&self) -> PhysicalSize<u32> {
        PhysicalSize { width: 1080, height: 2720 }
    }

    fn set_min_surface_size(&self, _: Option<Size>) {}

    fn set_max_surface_size(&self, _: Option<Size>) {}

    fn surface_resize_increments(&self) -> Option<PhysicalSize<u32>> {
        None
    }

    fn set_surface_resize_increments(&self, _increments: Option<Size>) {}

    fn set_title(&self, _title: &str) {}

    fn set_transparent(&self, _transparent: bool) {}

    fn set_blur(&self, _blur: bool) {}

    fn set_visible(&self, _visibility: bool) {}

    fn is_visible(&self) -> Option<bool> {
        None
    }

    fn set_resizable(&self, _resizeable: bool) {}

    fn is_resizable(&self) -> bool {
        false
    }

    fn set_enabled_buttons(&self, _buttons: WindowButtons) {}

    fn enabled_buttons(&self) -> WindowButtons {
        WindowButtons::all()
    }

    fn set_minimized(&self, _minimized: bool) {}

    fn is_minimized(&self) -> Option<bool> {
        None
    }

    fn set_maximized(&self, _maximized: bool) {}

    fn is_maximized(&self) -> bool {
        false
    }

    fn set_fullscreen(&self, _monitor: Option<Fullscreen>) {
        warn!("Cannot set fullscreen on Android");
    }

    fn fullscreen(&self) -> Option<Fullscreen> {
        None
    }

    fn set_decorations(&self, _decorations: bool) {}

    fn is_decorated(&self) -> bool {
        true
    }

    fn set_window_level(&self, _level: WindowLevel) {}

    fn set_window_icon(&self, _window_icon: Option<crate::icon::Icon>) {}

    fn set_ime_cursor_area(&self, _position: Position, _size: Size) {}

    fn set_ime_allowed(&self, _allowed: bool) {}

    fn set_ime_purpose(&self, _purpose: ImePurpose) {}

    fn focus_window(&self) {}

    fn request_user_attention(&self, _request_type: Option<window::UserAttentionType>) {}

    fn set_cursor(&self, _: Cursor) {}

    fn set_cursor_position(&self, _: Position) -> Result<(), RequestError> {
        Err(NotSupportedError::new("set_cursor_position is not supported").into())
    }

    fn set_cursor_grab(&self, _: CursorGrabMode) -> Result<(), RequestError> {
        Err(NotSupportedError::new("set_cursor_grab is not supported").into())
    }

    fn set_cursor_visible(&self, _: bool) {}

    fn drag_window(&self) -> Result<(), RequestError> {
        Err(NotSupportedError::new("drag_window is not supported").into())
    }

    fn drag_resize_window(&self, _direction: ResizeDirection) -> Result<(), RequestError> {
        Err(NotSupportedError::new("drag_resize_window").into())
    }

    #[inline]
    fn show_window_menu(&self, _position: Position) {}

    fn set_cursor_hittest(&self, _hittest: bool) -> Result<(), RequestError> {
        Err(NotSupportedError::new("set_cursor_hittest is not supported").into())
    }

    fn set_theme(&self, _theme: Option<Theme>) {}

    fn theme(&self) -> Option<Theme> {
        None
    }

    fn set_content_protected(&self, _protected: bool) {}

    fn has_focus(&self) -> bool {
        HAS_FOCUS.load(Ordering::Relaxed)
    }

    fn title(&self) -> String {
        String::new()
    }

    fn reset_dead_keys(&self) {}

    #[cfg(feature = "rwh_06")]
    fn rwh_06_display_handle(&self) -> &dyn rwh_06::HasDisplayHandle {
        self
    }

    #[cfg(feature = "rwh_06")]
    fn rwh_06_window_handle(&self) -> &dyn rwh_06::HasWindowHandle {
        self
    }
}

#[derive(Default, Clone, Debug)]
pub struct OsError;

use std::fmt::{self, Display, Formatter};
impl Display for OsError {
    fn fmt(&self, fmt: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        write!(fmt, "Android OS Error")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MonitorHandle;

impl MonitorHandle {
    pub fn name(&self) -> Option<String> {
        unreachable!()
    }

    pub fn position(&self) -> Option<PhysicalPosition<i32>> {
        unreachable!()
    }

    pub fn scale_factor(&self) -> f64 {
        unreachable!()
    }

    pub fn current_video_mode(&self) -> Option<VideoModeHandle> {
        unreachable!()
    }

    pub fn video_modes(&self) -> std::iter::Empty<VideoModeHandle> {
        unreachable!()
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct VideoModeHandle;

impl VideoModeHandle {
    pub fn size(&self) -> PhysicalSize<u32> {
        unreachable!()
    }

    pub fn bit_depth(&self) -> Option<NonZeroU16> {
        unreachable!()
    }

    pub fn refresh_rate_millihertz(&self) -> Option<NonZeroU32> {
        unreachable!()
    }

    pub fn monitor(&self) -> MonitorHandle {
        unreachable!()
    }
}
