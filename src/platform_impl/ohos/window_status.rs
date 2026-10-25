#[derive(Debug, PartialEq, Eq)]
pub(crate) struct WindowStatus {
    pub(crate) minimized: bool,
    pub(crate) maximized: bool,
    pub(crate) fullscreen: bool,
}

pub(crate) fn window_status(status: i32) -> Option<WindowStatus> {
    (1..=5).contains(&status).then_some(WindowStatus {
        minimized: status == 3,
        maximized: status == 2,
        fullscreen: status == 1,
    })
}
