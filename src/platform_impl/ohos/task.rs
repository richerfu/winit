//! Worker executor for typed Ability bridge calls. Never block the ArkTS UI thread.
use std::{future::Future, sync::OnceLock};

pub fn spawn(task: impl Future<Output = ()> + Send + 'static) {
    static POOL: OnceLock<futures_executor::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        futures_executor::ThreadPoolBuilder::new()
            .pool_size(1)
            .name_prefix("winit-ohos-bridge-")
            .create()
            .expect("cannot start OHOS bridge executor")
    })
    .spawn_ok(task);
}

/// Serializes mutations of one platform object while allowing unrelated objects
/// and dialogs to make progress on the executor.
#[derive(Default)]
pub struct CommandQueue(std::sync::Mutex<Option<futures_channel::oneshot::Receiver<()>>>);

impl std::fmt::Debug for CommandQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandQueue").finish_non_exhaustive()
    }
}

impl CommandQueue {
    pub fn push(&self, task: impl Future<Output = ()> + Send + 'static) {
        let (done, next) = futures_channel::oneshot::channel();
        let previous = self.0.lock().unwrap().replace(next);
        spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            task.await;
            let _ = done.send(());
        });
    }
}
