//! Access to files selected by the system picker.
//!
//! A picker URI is not a Rust filesystem path. Persistent grants are best effort,
//! require `ohos.permission.FILE_ACCESS_PERSIST`, and must run on a worker.

use std::path::PathBuf;

pub use ohos_fileshare_binding as permissions;

/// Persist read (or read/write) access and resolve the selected native paths.
/// An empty selection represents cancellation. Only unresolved or non-absolute paths
/// are errors. Persisting and activating grants is best effort and requires
/// `ohos.permission.FILE_ACCESS_PERSIST`; successful grants can be revoked later.
pub fn persist_paths(uris: &[String], writable: bool) -> Result<Vec<PathBuf>, String> {
    if uris.is_empty() {
        return Ok(Vec::new());
    }
    let paths = uris
        .iter()
        .map(|uri| {
            super::drag_drop::file_path(uri)
                .filter(|path| path.is_absolute())
                .ok_or_else(|| format!("Cannot resolve selected file URI: {uri}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut policies: Vec<_> = uris
        .iter()
        .map(|uri| permissions::PolicyInfo {
            uri: uri.clone(),
            operation_mode: if writable { 3 } else { 1 },
        })
        .collect();
    match permissions::persist_permission(&policies) {
        Ok(failed) => {
            if !failed.is_empty() {
                tracing::warn!("Cannot persist selected file permissions: {failed:?}");
                policies.retain(|policy| !failed.iter().any(|error| error.uri == policy.uri));
            }
            if !policies.is_empty() {
                match permissions::activate_permission(&policies) {
                    Ok(failed) if !failed.is_empty() => {
                        tracing::warn!("Cannot activate selected file permissions: {failed:?}")
                    }
                    Err(error) => {
                        tracing::warn!("Cannot activate selected file permissions: {error}")
                    }
                    _ => {}
                }
            }
        }
        Err(error) => tracing::warn!("Cannot persist selected file permissions: {error}"),
    }
    Ok(paths)
}
