use std::time::Instant;

#[derive(Clone, Copy)]
pub(crate) enum ControlFlow {
    Wait,
    WaitUntil(Instant),
    Poll,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Wait,
    Deadline(Instant),
    Wake,
    Frame,
}

pub(crate) fn control_flow_action(flow: ControlFlow, now: Instant, presentable: bool) -> Action {
    match flow {
        ControlFlow::Wait => Action::Wait,
        ControlFlow::WaitUntil(deadline) if deadline <= now => Action::Wake,
        ControlFlow::WaitUntil(deadline) => Action::Deadline(deadline),
        ControlFlow::Poll if presentable => Action::Frame,
        ControlFlow::Poll => Action::Wait,
    }
}
