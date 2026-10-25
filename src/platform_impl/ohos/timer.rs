use std::{
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
    time::Instant,
};

#[derive(Default)]
struct State {
    deadline: Option<Instant>,
    closed: bool,
}

pub(crate) struct Timer {
    state: Arc<(Mutex<State>, Condvar)>,
    worker: Mutex<Option<JoinHandle<()>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl std::fmt::Debug for Timer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Timer").finish_non_exhaustive()
    }
}

impl Timer {
    pub(crate) fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            state: Arc::new((Mutex::new(State::default()), Condvar::new())),
            worker: Mutex::new(None),
            wake: Arc::new(wake),
        }
    }

    pub(crate) fn set_deadline(&self, deadline: Option<Instant>) {
        let immediate = deadline.is_some_and(|deadline| deadline <= Instant::now());
        let (lock, changed) = &*self.state;
        lock.lock().unwrap().deadline = if immediate { None } else { deadline };
        if deadline.is_some() && !immediate {
            let mut worker = self.worker.lock().unwrap();
            if worker.is_none() {
                let state = self.state.clone();
                let wake = self.wake.clone();
                *worker = Some(
                    std::thread::Builder::new()
                        .name("winit-ohos-timer".into())
                        .spawn(move || {
                            let (lock, changed) = &*state;
                            let mut state = lock.lock().unwrap();
                            loop {
                                if state.closed {
                                    return;
                                }
                                match state.deadline {
                                    Some(deadline) if deadline <= Instant::now() => {
                                        state.deadline = None;
                                        drop(state);
                                        wake();
                                        state = lock.lock().unwrap();
                                    }
                                    Some(deadline) => {
                                        state = changed
                                            .wait_timeout(
                                                state,
                                                deadline.saturating_duration_since(Instant::now()),
                                            )
                                            .unwrap()
                                            .0;
                                    }
                                    None => state = changed.wait(state).unwrap(),
                                }
                            }
                        })
                        .expect("create OHOS timer thread"),
                );
            }
        }
        changed.notify_one();
        if immediate {
            (self.wake)();
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        let (lock, changed) = &*self.state;
        lock.lock().unwrap().closed = true;
        changed.notify_one();
        if let Some(worker) = self.worker.get_mut().unwrap().take() {
            let _ = worker.join();
        }
    }
}
