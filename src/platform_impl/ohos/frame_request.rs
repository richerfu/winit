//! A request ticket and its frame time share one lock across render sessions.
use std::{sync::Mutex, time::Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameTicket(u64);

#[derive(Debug, Default)]
struct State {
    generation: u64,
    active: bool,
    requested: Option<FrameTicket>,
    pending: Option<Instant>,
    failed: bool,
}

#[derive(Debug, Default)]
pub(crate) struct FrameRequest {
    state: Mutex<State>,
}

impl FrameRequest {
    pub(crate) fn set_active(&self, active: bool) {
        let mut state = self.state.lock().unwrap();
        if state.active != active {
            state.active = active;
            state.requested = None;
            state.pending = None;
            state.failed = false;
        }
    }

    pub(crate) fn request(&self) -> Option<FrameTicket> {
        let mut state = self.state.lock().unwrap();
        if !state.active || state.requested.is_some() || state.failed {
            return None;
        }
        state.generation = state.generation.wrapping_add(1);
        let ticket = FrameTicket(state.generation);
        state.requested = Some(ticket);
        Some(ticket)
    }

    pub(crate) fn complete(&self, ticket: FrameTicket, frame_time: Instant) -> bool {
        let mut state = self.state.lock().unwrap();
        if !state.active || state.requested != Some(ticket) {
            return false;
        }
        state.requested = None;
        // Replace queued ticks instead of replaying missed animation frames.
        // Timestamp and ticket validation must be published together: a late
        // callback from a hidden/destroyed surface cannot overwrite a new tick.
        state.pending = Some(state.pending.map_or(frame_time, |old| old.max(frame_time)));
        true
    }

    pub(crate) fn fail(&self, ticket: FrameTicket) {
        let mut state = self.state.lock().unwrap();
        if state.requested == Some(ticket) {
            state.requested = None;
            state.failed = true;
        }
    }

    pub(crate) fn failed(&self) -> bool {
        self.state.lock().unwrap().failed
    }

    pub(crate) fn take_pending(&self) -> Option<Instant> {
        self.state.lock().unwrap().pending.take()
    }
}
