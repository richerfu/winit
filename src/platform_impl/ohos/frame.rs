use std::{
    sync::{Arc, OnceLock},
    time::Instant,
};

use ohos_vsync_binding::Vsync;
use openharmony_ability::{FrameInputDelivery, OpenHarmonyApp};

use super::{frame_request::FrameRequest, main_wake::MainWake};

pub(crate) fn main_wake(app: &OpenHarmonyApp) -> Arc<MainWake> {
    static WAKE: OnceLock<Arc<MainWake>> = OnceLock::new();
    WAKE.get_or_init(|| {
        let wake = Arc::new(MainWake::default());
        let app = app.clone();
        wake.set_waker(move || app.create_waker().wake());
        wake
    })
    .clone()
}

pub(crate) struct FrameScheduler {
    vsync: Option<Vsync>,
    state: Arc<FrameRequest>,
    wake: Arc<MainWake>,
}

impl std::fmt::Debug for FrameScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameScheduler")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl FrameScheduler {
    pub(crate) fn new(id: i64, wake: Arc<MainWake>) -> Self {
        let vsync = Vsync::try_new(format!("winit-ohos-{id}"));
        if vsync.is_none() {
            tracing::warn!("OHOS VSync unavailable for window {id}");
        }
        Self {
            vsync,
            state: Arc::new(FrameRequest::default()),
            wake,
        }
    }

    pub(crate) fn healthy(&self) -> bool {
        self.vsync.is_some() && !self.state.failed()
    }

    pub(crate) fn set_active(&self, active: bool) {
        self.state.set_active(active);
    }

    pub(crate) fn take_pending(&self) -> bool {
        self.state.take_pending().is_some()
    }

    pub(crate) fn request_frame(&self) {
        let Some(vsync) = &self.vsync else {
            return;
        };
        let Some(ticket) = self.state.request() else {
            return;
        };
        let state = self.state.clone();
        let wake = self.wake.clone();
        let result = vsync.request_frame_once(move |_| {
            if state.complete(ticket, Instant::now()) {
                wake.notify();
            }
        });
        if result != 0 {
            self.state.fail(ticket);
            tracing::warn!("OHOS VSync request failed: {result}");
            self.wake.notify();
        }
    }

    pub(crate) fn sync_delivery(&self, app: &OpenHarmonyApp, id: i64) {
        let delivery = if self.healthy() {
            FrameInputDelivery::OnDemand
        } else {
            FrameInputDelivery::Continuous
        };
        if let Err(error) = app.set_frame_input_delivery_for(id, delivery) {
            tracing::warn!("OHOS frame delivery for window {id}: {error}");
        }
    }
}
