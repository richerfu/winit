//! # OpenHarmony
//!
//! The OpenHarmony backend builds on (and exposes types from) the [`ohos-rs`](https://docs.rs/ohos-rs/) crate.
//!
//! Native OpenHarmony applications need some form of "glue" crate that is responsible
//! for defining the main entry point for your Rust application as well as tracking
//! various life-cycle events and synchronizing with the main thread.
//!
//! Winit uses the [openharmony-ability](https://docs.rs/openharmony-ability/) as a
//! glue crate.

use crate::event_loop::{ActiveEventLoop, EventLoop, EventLoopBuilder};
use crate::window::Window;

use self::ability::{Configuration, OpenHarmonyApp, Rect};

pub use crate::platform_impl::{drag_drop, events, file_access, plugins, task, ImeEdit};

/// Additional methods on [`EventLoop`] that are specific to OpenHarmony.
pub trait EventLoopExtOpenHarmony {
    /// Get the [`OpenHarmonyApp`] which was used to create this event loop.
    fn openharmony_app(&self) -> &OpenHarmonyApp;
}

impl<T> EventLoopExtOpenHarmony for EventLoop<T> {
    fn openharmony_app(&self) -> &OpenHarmonyApp {
        &self.event_loop.openharmony_app
    }
}

/// Additional methods on [`ActiveEventLoop`] that are specific to OpenHarmony.
pub trait ActiveEventLoopExtOpenHarmony {
    /// Get the [`OpenHarmonyApp`] which was used to create this event loop.
    fn openharmony_app(&self) -> &OpenHarmonyApp;
}

impl ActiveEventLoopExtOpenHarmony for ActiveEventLoop {
    fn openharmony_app(&self) -> &OpenHarmonyApp {
        &self.p.app
    }
}

/// Additional methods on [`Window`] that are specific to OpenHarmony.
pub trait WindowExtOpenHarmony {
    /// Get the [`OpenHarmonyApp`] that owns this window.
    fn openharmony_app(&self) -> &OpenHarmonyApp;

    /// Consume native IME edits accompanying an `Ime::Enabled` notification.
    /// Selection and deletion offsets use UTF-16, as required by the OHOS IME.
    fn take_ime_edits(&self) -> Vec<ImeEdit>;

    /// Update the surrounding text and UTF-8 byte selection/cursor offsets reported to the OHOS IME.
    fn set_ime_surrounding_text(&self, text: String, cursor: usize, anchor: usize);

    /// Get the content rectangle of this window.
    fn content_rect(&self) -> Rect;

    /// Get the current system configuration of the ability.
    fn config(&self) -> Configuration;
}

impl WindowExtOpenHarmony for Window {
    fn openharmony_app(&self) -> &OpenHarmonyApp {
        self.window.openharmony_app()
    }

    fn take_ime_edits(&self) -> Vec<ImeEdit> {
        self.window.take_ime_edits()
    }

    fn set_ime_surrounding_text(&self, text: String, cursor: usize, anchor: usize) {
        self.window.set_ime_surrounding_text(text, cursor, anchor);
    }

    fn content_rect(&self) -> Rect {
        self.window.content_rect()
    }

    fn config(&self) -> Configuration {
        self.window.config()
    }
}

/// Additional methods on [`EventLoopBuilder`] that are specific to OpenHarmony.
pub trait EventLoopBuilderExtOpenHarmony {
    /// Associates the [`OpenHarmonyApp`] that was passed to `openharmony-ability::ability` with the event loop.
    ///
    /// This must be called on OpenHarmony since the [`OpenHarmonyApp`] is not global state.
    fn with_openharmony_app(&mut self, app: OpenHarmonyApp) -> &mut Self;
}

impl<T> EventLoopBuilderExtOpenHarmony for EventLoopBuilder<T> {
    fn with_openharmony_app(&mut self, app: OpenHarmonyApp) -> &mut Self {
        self.platform_specific.openharmony_app = Some(app);
        self
    }
}

/// Re-export of the `openharmony-ability` API.
pub mod ability {
    #[doc(no_inline)]
    pub use openharmony_ability::*;

    #[doc(no_inline)]
    pub use openharmony_ability_derive::*;
}
