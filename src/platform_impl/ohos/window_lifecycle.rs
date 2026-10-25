#[derive(Debug, Default)]
pub(crate) struct WindowLifecycle {
    pub(crate) surface_available: bool,
    pub(crate) dropped_by_app: bool,
    pub(crate) closed: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct CloseEvents {
    pub(crate) close_requested: bool,
    pub(crate) destroyed: bool,
}

impl WindowLifecycle {
    pub(crate) fn surface_created(&mut self) -> bool {
        if self.closed {
            return false;
        }
        self.surface_available = true;
        true
    }

    pub(crate) fn surface_destroyed(&mut self) -> CloseEvents {
        self.surface_available = false;
        if self.dropped_by_app {
            self.close()
        } else {
            CloseEvents::default()
        }
    }

    pub(crate) fn close(&mut self) -> CloseEvents {
        if self.closed {
            return CloseEvents::default();
        }
        self.closed = true;
        self.surface_available = false;
        CloseEvents {
            close_requested: !self.dropped_by_app,
            destroyed: true,
        }
    }

    pub(crate) fn presentable(&self, visible: bool, minimized: bool) -> bool {
        self.surface_available && visible && !minimized && !self.closed && !self.dropped_by_app
    }

    pub(crate) fn take_redraw(
        &self,
        tick: bool,
        visible: bool,
        minimized: bool,
        pending: &mut bool,
    ) -> bool {
        tick && self.presentable(visible, minimized) && std::mem::take(pending)
    }
}
