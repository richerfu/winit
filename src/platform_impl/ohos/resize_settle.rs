use std::time::{Duration, Instant};

pub(crate) const SETTLE: Duration = Duration::from_millis(200);

#[derive(Debug, Default)]
pub(crate) struct ResizeSettle {
    until: Option<Instant>,
}

impl ResizeSettle {
    pub(crate) fn extend(&mut self, now: Instant) {
        let until = now + SETTLE;
        self.until = Some(self.until.map_or(until, |old| old.max(until)));
    }

    pub(crate) fn active(&mut self, now: Instant) -> bool {
        if self.until.is_some_and(|until| now < until) {
            return true;
        }
        self.until = None;
        false
    }
}
