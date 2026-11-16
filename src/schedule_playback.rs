//! Schedule playback on the regular globe/map, using immutable saved inputs.
#[cfg(not(target_arch = "wasm32"))]
mod loader;
mod overlay;
mod panel;
mod scene;
#[cfg(test)]
mod tests;

use crate::{aoi::preview::Preview, config::TabConfig};
use eframe::egui;
use leo_capture::scheduling::playback::{Replay, State};
use leo_sim::geometry::Vector;
#[cfg(not(target_arch = "wasm32"))]
use loader::LoadEvent;
pub(crate) use overlay::Overlay;
use std::sync::Arc;
pub(crate) const ACTIVE_COLOR: [u8; 3] = [75, 215, 175];
pub(crate) const MISSED_COLOR: [u8; 3] = [255, 75, 75];
pub(crate) const INACTIVE_COLOR: [u8; 3] = [75, 82, 92];
pub(crate) const TRAIL_SECONDS: f64 = 900.0;
const DEFAULT_REPLAY_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/schedules/worldview60-ongoing-baseline-24h.json.gz"
);

pub(crate) struct Footprint {
    pub satellite: usize,
    pub mode: usize,
    pub start_s: f64,
    pub end_s: f64,
    pub corners: [Vector; 4],
}
pub(crate) struct Scene {
    pub replay: Replay,
    pub layers: Vec<Arc<Preview>>,
    pub footprints: Vec<Footprint>,
}
pub(crate) struct Panel {
    path: String,
    pub auto_load: bool,
    error: Option<String>,
    progress: (f32, String),
    #[cfg(not(target_arch = "wasm32"))]
    started: Option<std::time::Instant>,
    #[cfg(not(target_arch = "wasm32"))]
    pending: Option<std::sync::mpsc::Receiver<LoadEvent>>,
}
impl Default for Panel {
    fn default() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let supplied = std::env::var("LEO_VIZ_SCHEDULE").ok();
        #[cfg(target_arch = "wasm32")]
        let supplied: Option<String> = None;
        Self {
            auto_load: supplied.is_some(),
            path: supplied.unwrap_or_else(|| DEFAULT_REPLAY_PATH.into()),
            error: None,
            progress: (0.0, String::new()),
            #[cfg(not(target_arch = "wasm32"))]
            started: None,
            #[cfg(not(target_arch = "wasm32"))]
            pending: None,
        }
    }
}
/// The schedule clock clamps at the horizon; rendering is a pure function of
/// time, so backward seeks and large playback steps never lose acquisitions.
pub(crate) fn advance(tab: &mut TabConfig, dt: f64) -> f64 {
    let previous = tab.settings.time;
    if tab.settings.animate {
        tab.settings.time += dt * tab.settings.speed;
    }
    if let Some(scene) = &tab.observation_replay {
        tab.settings.time = tab
            .settings
            .time
            .clamp(0.0, scene.replay.document.horizon_s);
        if tab.settings.time >= scene.replay.document.horizon_s {
            tab.settings.animate = false;
        }
    }
    tab.settings.time - previous
}

pub(crate) fn epoch(
    tab: &TabConfig,
    fallback: chrono::DateTime<chrono::Utc>,
) -> chrono::DateTime<chrono::Utc> {
    tab.observation_replay
        .as_ref()
        .map_or(fallback, |s| s.epoch())
}

#[cfg(target_arch = "wasm32")]
impl Panel {
    pub fn poll(&mut self, _: &egui::Context, _: &mut crate::aoi::AoiGenerator, _: &mut TabConfig) {
    }
}
