use std::sync::Mutex;

#[derive(Debug)]
struct State<T> {
    latest: Option<T>,
    in_flight: bool,
}

#[derive(Debug)]
pub(crate) struct Geometry<T>(Mutex<State<T>>);

impl<T> Default for Geometry<T> {
    fn default() -> Self {
        Self(Mutex::new(State {
            latest: None,
            in_flight: false,
        }))
    }
}

impl<T> Geometry<T> {
    pub(crate) fn submit(&self, target: T) -> bool {
        let mut state = self.0.lock().unwrap();
        state.latest = Some(target);
        if state.in_flight {
            return false;
        }
        state.in_flight = true;
        true
    }

    pub(crate) fn take(&self) -> Option<T> {
        let mut state = self.0.lock().unwrap();
        let target = state.latest.take();
        if target.is_none() {
            state.in_flight = false;
        }
        target
    }

    pub(crate) fn cancel(&self) {
        self.0.lock().unwrap().latest = None;
    }
}
