//! Platform events without a cross-platform winit equivalent.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};

static PENDING: AtomicBool = AtomicBool::new(false);

pub(crate) fn take_pending() -> bool {
    PENDING.swap(false, Ordering::AcqRel)
}

/// Native system events. Subscribe before the first application frame.
#[derive(Clone, Debug)]
pub enum PlatformEvent {
    /// Launch/deep-link data, including notification response parameters.
    Want { uri: String, parameters: String },
    #[cfg(feature = "extended-plugins")]
    Menu { id: String },
    #[cfg(feature = "extended-plugins")]
    MenuOpened { window_id: String },
    #[cfg(feature = "extended-plugins")]
    SystemState(super::plugins::system_state::SystemStateChangedEvent),
    #[cfg(feature = "extended-plugins")]
    ScreenReader(bool),
}

fn subscribers() -> &'static Mutex<Vec<mpsc::Sender<PlatformEvent>>> {
    static SUBSCRIBERS: OnceLock<Mutex<Vec<mpsc::Sender<PlatformEvent>>>> = OnceLock::new();
    SUBSCRIBERS.get_or_init(Default::default)
}

/// Drain this receiver during the application's next UI update.
pub fn subscribe() -> mpsc::Receiver<PlatformEvent> {
    let (tx, rx) = mpsc::channel();
    subscribers().lock().unwrap().push(tx);
    rx
}

pub(crate) fn emit(event: PlatformEvent) {
    subscribers()
        .lock()
        .unwrap()
        .retain(|tx| tx.send(event.clone()).is_ok());
    PENDING.store(true, Ordering::Release);
}

#[cfg(feature = "extended-plugins")]
pub(crate) fn register_plugins(app: &openharmony_ability::OpenHarmonyApp) {
    use super::plugins::{accessibility::AccessibilityClient, menu, system_state};
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        let (menu_tx, menu_rx) = crossbeam_channel::unbounded();
        let (open_tx, open_rx) = crossbeam_channel::unbounded();
        let (state_tx, state_rx) = crossbeam_channel::unbounded();
        menu::register_menu_event_sender(menu_tx);
        menu::register_menu_open_event_sender(open_tx);
        system_state::register_system_state_event_sender(state_tx);
        let wake = super::frame::main_wake(app);
        // These plugin callbacks run on ArkTS's thread. Receive elsewhere and wake
        // the event loop; never synchronously invoke UI or a bridge in a callback.
        std::thread::Builder::new()
            .name("ohos-platform-events".into())
            .spawn(move || loop {
                let event = crossbeam_channel::select! {
                    recv(menu_rx) -> value => value.ok().map(|id| PlatformEvent::Menu { id }),
                    recv(open_rx) -> value => value.ok().map(|event: menu::MenuOpenEvent| PlatformEvent::MenuOpened { window_id: event.window_id }),
                    recv(state_rx) -> value => value.ok().map(PlatformEvent::SystemState),
                };
                let Some(event) = event else { break };
                emit(event);
                wake.notify();
            })
            .expect("create OHOS platform event thread");
    });
    let wake = super::frame::main_wake(app);
    let app = app.clone();
    super::task::spawn(async move {
        let result = async {
            let client = AccessibilityClient::new(&app)?;
            client
                .subscribe_state_change(move |enabled| {
                    emit(PlatformEvent::ScreenReader(enabled));
                    wake.notify();
                })
                .await
        }
        .await;
        if let Err(error) = result {
            tracing::warn!("OHOS screen reader subscription: {error}");
        }
    });
}
