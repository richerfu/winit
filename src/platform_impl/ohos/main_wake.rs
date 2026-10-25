use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

#[derive(Default)]
pub(crate) struct MainWake {
    requested: AtomicBool,
    waker: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl std::fmt::Debug for MainWake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MainWake").finish_non_exhaustive()
    }
}

impl MainWake {
    pub(crate) fn set_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        *self.waker.lock().unwrap() = Some(Arc::new(waker));
    }

    pub(crate) fn notify(&self) {
        if self.requested.swap(true, Ordering::AcqRel) {
            return;
        }
        let waker = self.waker.lock().unwrap().clone();
        if let Some(waker) = waker {
            waker();
        } else {
            self.requested.store(false, Ordering::Release);
        }
    }

    pub(crate) fn begin_turn(&self) {
        self.requested.store(false, Ordering::Release);
    }
}
