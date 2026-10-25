//! Snapshot the native display list; no FFI-owned memory escapes the query.
use std::collections::VecDeque;
use std::fmt;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use crate::dpi::{PhysicalPosition, PhysicalSize};

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MonitorHandle {
    id: u32,
    name: String,
    size: PhysicalSize<u32>,
    scale_milli: u32,
    rate: u32,
}

impl fmt::Debug for MonitorHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MonitorHandle")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("size", &self.size)
            .field("scale", &self.scale_factor())
            .field("rate", &self.rate)
            .finish()
    }
}

impl MonitorHandle {
    pub(crate) fn native_id(&self) -> u64 {
        u64::from(self.id)
    }

    pub fn name(&self) -> Option<String> {
        Some(self.name.clone())
    }

    pub fn size(&self) -> PhysicalSize<u32> {
        self.size
    }

    pub fn position(&self) -> PhysicalPosition<i32> {
        PhysicalPosition::new(0, 0)
    }

    pub fn scale_factor(&self) -> f64 {
        f64::from(self.scale_milli) / 1000.0
    }

    pub fn refresh_rate_millihertz(&self) -> Option<u32> {
        Some(self.rate.saturating_mul(1000))
    }

    pub fn video_modes(&self) -> std::vec::IntoIter<VideoModeHandle> {
        vec![VideoModeHandle {
            size: self.size,
            bit_depth: 32,
            refresh_rate_millihertz: self.rate.saturating_mul(1000),
            monitor: self.clone(),
        }]
        .into_iter()
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct VideoModeHandle {
    size: PhysicalSize<u32>,
    bit_depth: u16,
    refresh_rate_millihertz: u32,
    monitor: MonitorHandle,
}

impl VideoModeHandle {
    pub fn size(&self) -> PhysicalSize<u32> {
        self.size
    }

    pub fn bit_depth(&self) -> u16 {
        self.bit_depth
    }

    pub fn refresh_rate_millihertz(&self) -> u32 {
        self.refresh_rate_millihertz
    }

    pub fn monitor(&self) -> MonitorHandle {
        self.monitor.clone()
    }
}

pub(crate) fn all() -> VecDeque<MonitorHandle> {
    type CachedMonitors = Option<(Instant, VecDeque<MonitorHandle>)>;
    static CACHE: OnceLock<Mutex<CachedMonitors>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap();
    if let Some((time, monitors)) = &*cache {
        if time.elapsed().as_millis() < 500 {
            return monitors.clone();
        }
    }
    let monitors = query();
    *cache = Some((Instant::now(), monitors.clone()));
    monitors
}

fn query() -> VecDeque<MonitorHandle> {
    use ohos_display_sys::*;
    let mut ptr = std::ptr::null_mut();
    // NativeDisplayManager owns both the array and its nested fields. Copy the
    // small immutable snapshot before calling its matching destructor.
    unsafe {
        if OH_NativeDisplayManager_CreateAllDisplays(&mut ptr) != 0 || ptr.is_null() {
            return VecDeque::new();
        }
        let info = &*ptr;
        let displays = if info.displaysLength <= 128 && !info.displaysInfo.is_null() {
            std::slice::from_raw_parts(info.displaysInfo, info.displaysLength as usize)
        } else {
            &[]
        };
        let result = displays
            .iter()
            .filter(|d| d.isAlive)
            .map(|d| {
                let name: Vec<_> = d
                    .name
                    .iter()
                    .take_while(|c| **c != 0)
                    .map(|c| c.to_ne_bytes()[0])
                    .collect();
                let scale = if d.densityPixels > 0.0 {
                    d.densityPixels as f64
                } else {
                    1.0
                };
                MonitorHandle {
                    id: d.id,
                    name: String::from_utf8_lossy(&name).into_owned(),
                    size: PhysicalSize::new(d.width.max(0) as u32, d.height.max(0) as u32),
                    scale_milli: ((scale * 1000.0).round() as u32).max(1),
                    rate: d.refreshRate,
                }
            })
            .collect();
        OH_NativeDisplayManager_DestroyAllDisplays(ptr);
        result
    }
}

pub(crate) fn primary() -> Option<MonitorHandle> {
    let mut id = 0;
    unsafe {
        ohos_display_sys::OH_NativeDisplayManager_GetDefaultDisplayId(&mut id);
    }
    all().into_iter().find(|m| m.native_id() == id)
}
