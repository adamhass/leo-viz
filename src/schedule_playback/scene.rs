//! Build immutable, status-colored AOIs and sweep geometry on the loading worker.
use super::*;

impl Scene {
    pub fn overlays(&self) -> Vec<(&Arc<Preview>, f32, f32)> {
        self.layers.iter().map(|p| (p, 0.95, 0.14)).collect()
    }
    pub fn epoch(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp_millis((self.replay.document.epoch_unix_s * 1000.0) as i64)
            .unwrap()
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn load(
        path: &str,
        mut progress: impl FnMut(f32, String),
    ) -> Result<(Self, crate::aoi::storage::Loaded), String> {
        let replay = Replay::load_with_progress(std::path::Path::new(path), &mut progress)?;
        if chrono::DateTime::from_timestamp_millis((replay.document.epoch_unix_s * 1000.0) as i64)
            .is_none()
        {
            return Err("Replay epoch is outside the supported date range".into());
        }
        progress(0.29, "Preparing AOI status colors…".into());
        let colors = replay
            .jobs
            .iter()
            .enumerate()
            .map(|(j, job)| {
                // A missed deadline overrides incidental or partial selected coverage.
                (
                    job.aoi.key(),
                    if replay.missed_jobs[j] {
                        MISSED_COLOR
                    } else if replay.scheduled_jobs[j] {
                        ACTIVE_COLOR
                    } else {
                        INACTIVE_COLOR
                    },
                )
            })
            .collect();
        let loaded = crate::aoi::storage::load_embedded(
            &replay.document.preset,
            Some(&colors),
            |f, message| progress(0.30 + 0.62 * f, message),
        )?;
        let workers = leo_capture::parallel::worker_count(replay.document.satellites.len());
        let pieces = leo_capture::parallel::map(
            replay.document.satellites.iter().collect(),
            workers,
            |sat| {
                Ok(sat
                    .captures
                    .iter()
                    .map(|c| {
                        let a = replay.scanline(sat.satellite, c.mode, c.start_s);
                        let b = replay.scanline(sat.satellite, c.mode, c.end_s);
                        Footprint {
                            satellite: sat.satellite,
                            mode: c.mode,
                            start_s: c.start_s,
                            end_s: c.end_s,
                            corners: [a[0], a[1], b[1], b[0]],
                        }
                    })
                    .collect::<Vec<_>>())
            },
            |done, total| {
                progress(0.93 + 0.06 * done as f32 / total.max(1) as f32,
            format!("Preparing sweep trails · {done} / {total} satellites · {workers} CPU workers"))
            },
        )?;
        let mut footprints: Vec<_> = pieces.into_iter().flatten().collect();
        footprints.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));
        let layers = loaded.layers.iter().flatten().cloned().collect();
        progress(1.0, "Schedule ready".into());
        Ok((
            Self {
                replay,
                layers,
                footprints,
            },
            loaded,
        ))
    }
}
