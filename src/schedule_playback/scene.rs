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
        let job_colors = status::JobColors::new(&replay);
        let colors = replay
            .jobs
            .iter()
            .zip(job_colors.at(&replay, 0.0))
            .map(|(job, color)| (job.aoi.key(), color))
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
            |sat| Ok(sweep_footprints(&replay, sat)),
            |done, total| {
                progress(0.93 + 0.06 * done as f32 / total.max(1) as f32,
            format!("Preparing sweep trails · {done} / {total} satellites · {workers} CPU workers"))
            },
        )?;
        let mut footprints: Vec<_> = pieces.into_iter().flatten().collect();
        footprints.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));
        let layers: Vec<_> = loaded.layers.iter().flatten().cloned().collect();
        let job_indices: std::collections::HashMap<_, _> = replay
            .jobs
            .iter()
            .enumerate()
            .map(|(i, job)| (job.aoi.key(), i))
            .collect();
        let layer_jobs = layers
            .iter()
            .map(|p| {
                p.aois
                    .iter()
                    .map(|a| {
                        job_indices
                            .get(&a.key())
                            .copied()
                            .ok_or_else(|| format!("Replay AOI {} has no job", a.key()))
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        progress(1.0, "Schedule ready".into());
        Ok((
            Self {
                replay,
                layers,
                footprints,
                job_colors,
                layer_jobs,
                color_time: std::sync::Mutex::new(Some(0.0)),
            },
            loaded,
        ))
    }
}

/// Draw long actions as short curved sweep pieces without changing capture identity.
fn sweep_footprints(
    replay: &Replay,
    sat: &leo_capture::scheduling::playback::Satellite,
) -> Vec<Footprint> {
    sat.captures
        .iter()
        .flat_map(|c| {
            let count = ((c.end_s - c.start_s) / 5.0).ceil().max(1.0) as usize;
            (0..count).map(move |i| {
                let start_s = c.start_s + (c.end_s - c.start_s) * i as f64 / count as f64;
                let end_s = c.start_s + (c.end_s - c.start_s) * (i + 1) as f64 / count as f64;
                let a = replay.scanline(sat.satellite, c.mode, start_s);
                let b = replay.scanline(sat.satellite, c.mode, end_s);
                Footprint {
                    satellite: sat.satellite,
                    mode: c.mode,
                    start_s,
                    end_s,
                    corners: [a[0], a[1], b[1], b[0]],
                }
            })
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn long_capture_trails_follow_the_orbit_without_splitting_scheduled_actions() {
        let mut scene = super::super::tests::fixture();
        scene.replay.document.schema_version = 2;
        scene.replay.document.satellites[0].captures[0].end_s = 109.5;
        let sat = &scene.replay.document.satellites[0];
        let pieces = sweep_footprints(&scene.replay, sat);
        assert_eq!(sat.captures.len(), 2);
        assert_eq!(pieces.len(), 3);
        assert_eq!(pieces[0].start_s, 100.0);
        assert_eq!(pieces[0].end_s, pieces[1].start_s);
        assert_eq!(pieces[1].end_s, 109.5);
        assert_eq!(pieces[0].corners[2], pieces[1].corners[1]);
        assert_eq!(scene.replay.pointing(sat, 108.0).state, State::Capturing);
    }
}
