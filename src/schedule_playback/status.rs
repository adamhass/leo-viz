//! Deadline status is fixed by the saved plan; solar eligibility follows the clock.
use super::*;
use leo_capture::workload::Illumination;
use leo_sim::geometry::{dot, gmst, ground_position, local_solar_time_hours, sun_direction};

pub(super) struct JobColors {
    pub due: Vec<bool>,
    base: Vec<[u8; 3]>,
    timed: Vec<(usize, Vector)>,
    completed_at_s: Vec<f64>,
}

impl JobColors {
    pub fn new(replay: &Replay) -> Self {
        let mut due = vec![false; replay.jobs.len()];
        if let Some(assessment) = &replay.document.assessment {
            for request in &assessment.requests {
                due[request.job] = true;
            }
        } else {
            for (d, job) in due.iter_mut().zip(&replay.jobs) {
                *d = job
                    .acquisition
                    .deadline_counts(job.cadence_hours, replay.document.horizon_s)
                    .0
                    > 0;
            }
        }
        let base: Vec<_> = replay
            .jobs
            .iter()
            .enumerate()
            .map(|(i, job)| {
                if !due[i] {
                    INACTIVE_COLOR
                } else if replay.missed_jobs[i] {
                    MISSED_COLOR
                } else if job.cadence_hours <= 24.0 {
                    DAILY_COLOR
                } else {
                    ACTIVE_COLOR
                }
            })
            .collect();
        let timed = replay
            .jobs
            .iter()
            .enumerate()
            .filter_map(|(i, job)| {
                (due[i]
                    && (replay.missed_jobs[i] || replay.scheduled_jobs[i])
                    && job.acquisition.illumination != Illumination::AnyTime)
                    .then(|| {
                        (
                            i,
                            ground_position(job.aoi.latitude, job.aoi.longitude, 1.0, 0.0),
                        )
                    })
            })
            .collect();
        Self {
            due,
            base,
            timed,
            completed_at_s: completion_times(replay),
        }
    }

    pub fn at(&self, replay: &Replay, time_s: f64) -> Vec<[u8; 3]> {
        let mut colors = self.base.clone();
        let instant = replay.document.epoch_unix_s + time_s;
        let sun = sun_direction(instant);
        let (s, c) = gmst(instant).sin_cos();
        for &(i, [x, y, z]) in &self.timed {
            let anchor = [c * x + s * z, y, -s * x + c * z];
            let solar_hours = local_solar_time_hours(anchor, sun);
            let elevation = dot(anchor, sun).clamp(-1.0, 1.0).asin().to_degrees();
            if !replay.jobs[i].acquisition.accepts(solar_hours, elevation) {
                colors[i] = if replay.missed_jobs[i] {
                    MISSED_UNAVAILABLE_COLOR
                } else {
                    UNAVAILABLE_COLOR
                };
            }
        }
        for (color, &completed_at) in colors.iter_mut().zip(&self.completed_at_s) {
            if completed_at.is_finite() && completed_at <= time_s {
                *color = CAPTURED_COLOR;
            }
        }
        colors
    }
}

/// Prefer the exporter's chronological polygon-union completion timestamp.
/// Legacy bundles only verify final coverage: their last selected crop is a
/// conservative fallback, which can be delayed by redundant later captures.
/// Every assessed deadline for a job must be fulfilled; future, unassessed
/// observations neither earn white nor delay the completed work in this replay.
fn completion_times(replay: &Replay) -> Vec<f64> {
    let Some(assessment) = &replay.document.assessment else {
        return vec![f64::INFINITY; replay.jobs.len()];
    };
    let mut ends: std::collections::HashMap<_, Option<f64>> = assessment
        .requests
        .iter()
        .map(|r| ((r.job, r.slot), None))
        .collect();
    for crop in replay
        .document
        .satellites
        .iter()
        .flat_map(|sat| &sat.captures)
        .flat_map(|capture| &capture.crops)
    {
        if let Some(end) = ends.get_mut(&(crop.job, crop.slot)) {
            *end = Some(end.map_or(crop.end_s, |previous| previous.max(crop.end_s)));
        }
    }
    let mut completed: Vec<Option<f64>> = vec![None; replay.jobs.len()];
    for request in &assessment.requests {
        let end = if request.complete {
            request
                .completed_at_s
                .or(ends[&(request.job, request.slot)])
                .unwrap_or(f64::INFINITY)
        } else {
            f64::INFINITY
        };
        let previous = &mut completed[request.job];
        *previous = Some(previous.map_or(end, |t| t.max(end)));
    }
    completed
        .into_iter()
        .map(|end| end.unwrap_or(f64::INFINITY))
        .collect()
}

impl Scene {
    /// Evaluate at the exact replay time, including paused/backward seeks.
    pub fn update_colors(&self, time_s: f64) {
        let mut previous = self.color_time.lock().unwrap();
        if *previous == Some(time_s) {
            return;
        }
        let colors = self.job_colors.at(&self.replay, time_s);
        for (layer, jobs) in self.layers.iter().zip(&self.layer_jobs) {
            layer.set_colors(&jobs.iter().map(|&j| colors[j]).collect::<Vec<_>>());
        }
        *previous = Some(time_s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leo_capture::workload::{Fulfilment, MosaicTiming};

    fn replay(cadences: &[f64]) -> Replay {
        let mut replay = super::super::tests::fixture().replay;
        replay.document.horizon_s = 86400.0;
        let template = replay.jobs[0].clone();
        replay.jobs = cadences
            .iter()
            .map(|&cadence| {
                let mut job = template.clone();
                job.cadence_hours = cadence;
                job.acquisition.first_deadline_hours = Some(cadence.min(12.0));
                job.acquisition.illumination = Illumination::AnyTime;
                job
            })
            .collect();
        replay.scheduled_jobs = vec![true; cadences.len()];
        replay.missed_jobs = vec![false; cadences.len()];
        replay
    }

    fn assess(replay: &mut Replay, outcomes: &[(usize, u64, bool)]) {
        use leo_capture::scheduling::playback::{Assessment, DeadlineOutcome};
        replay.document.assessment = Some(Assessment {
            job_cadence_hours: None,
            requests: outcomes
                .iter()
                .map(|&(job, slot, complete)| DeadlineOutcome {
                    completed_at_s: None,
                    job,
                    slot,
                    complete,
                    deadline_s: replay.jobs[job]
                        .acquisition
                        .deadline_s(slot, replay.jobs[job].cadence_hours),
                    coverage_fraction: if complete { 1.0 } else { 0.5 },
                })
                .collect(),
        });
        for &(job, _, complete) in outcomes {
            replay.missed_jobs[job] |= !complete;
        }
    }

    #[test]
    fn white_requires_verified_full_coverage_and_finished_crops_and_reverses_on_seek() {
        use leo_capture::scheduling::playback::Crop;
        let mut replay = replay(&[72.0, 72.0, 72.0]);
        assess(&mut replay, &[(0, 0, true), (1, 0, false), (2, 0, true)]);
        let job = &mut replay.jobs[0];
        job.acquisition.illumination = Illumination::Daylight;
        job.acquisition.solar_time_hours = 12.0;
        job.acquisition.solar_tolerance_hours = 4.0;
        // The fixture has two crops for job 0 ending at 101s and 111s.
        replay.document.satellites[0].captures[0].crops.push(Crop {
            job: 1,
            slot: 0,
            start_s: 100.0,
            end_s: 101.0,
        });
        // No selected crop supports job 2's completion claim: fail closed.
        replay.scheduled_jobs[2] = false;
        // Work for the next, unassessed cycle must not delay this cycle's white cue.
        let mut future = replay.document.satellites[0].captures[0].clone();
        future.action = 2;
        future.tick = 72000;
        future.start_s = 72000.0;
        future.end_s = 72001.0;
        future.crops = vec![Crop {
            job: 0,
            slot: 1,
            start_s: 72000.0,
            end_s: 72001.0,
        }];
        replay.document.satellites[0].captures.push(future);
        let colors = JobColors::new(&replay);
        for t in [0.0, 101.0, 110.999] {
            assert_eq!(colors.at(&replay, t)[0], UNAVAILABLE_COLOR);
        }
        for t in [111.0, 43200.0, 86400.0] {
            assert_eq!(
                colors.at(&replay, t),
                [CAPTURED_COLOR, MISSED_COLOR, ACTIVE_COLOR]
            );
        }
        assert_eq!(colors.at(&replay, 100.0)[0], UNAVAILABLE_COLOR);
        // New exports certify the first crop as sufficient; later duplicate
        // captures must no longer delay white until 111s.
        replay.document.assessment.as_mut().unwrap().requests[0].completed_at_s = Some(101.0);
        let exact = JobColors::new(&replay);
        assert_eq!(exact.at(&replay, 100.999)[0], UNAVAILABLE_COLOR);
        assert_eq!(exact.at(&replay, 101.0)[0], CAPTURED_COLOR);
        assert_eq!(exact.at(&replay, 111.0)[0], CAPTURED_COLOR);
        assert_eq!(exact.at(&replay, 100.0)[0], UNAVAILABLE_COLOR);
        replay.document.assessment = None;
        assert!(!JobColors::new(&replay)
            .at(&replay, 86400.0)
            .contains(&CAPTURED_COLOR));
    }

    #[test]
    fn white_waits_for_all_due_observations_and_never_hides_a_missed_deadline() {
        use leo_capture::scheduling::playback::Crop;
        let mut replay = replay(&[4.0]);
        replay.document.horizon_s = 8.0 * 3600.0;
        assess(&mut replay, &[(0, 0, true), (0, 1, true)]);
        let mut second = replay.document.satellites[0].captures[0].clone();
        second.action = 2;
        second.tick = 18000;
        second.start_s = 18000.0;
        second.end_s = 18001.0;
        second.crops = vec![Crop {
            job: 0,
            slot: 1,
            start_s: 18000.0,
            end_s: 18001.0,
        }];
        replay.document.satellites[0].captures.push(second);
        let colors = JobColors::new(&replay);
        assert_eq!(colors.at(&replay, 111.0), [DAILY_COLOR]);
        assert_eq!(colors.at(&replay, 18000.5), [DAILY_COLOR]);
        assert_eq!(colors.at(&replay, 18001.0), [CAPTURED_COLOR]);
        assess(&mut replay, &[(0, 0, true), (0, 1, false)]);
        assert_eq!(JobColors::new(&replay).at(&replay, 28800.0), [MISSED_COLOR]);
    }

    #[test]
    fn inactive_and_missed_override_daily_and_incidental_coverage() {
        let mut replay = replay(&[24.0, 4.0, 72.0, 24.0, 72.0]);
        replay.jobs[2].acquisition.first_deadline_hours = Some(48.0);
        replay.missed_jobs[3] = true;
        let colors = JobColors::new(&replay);
        assert_eq!(colors.due, [true, true, false, true, true]);
        assert_eq!(
            colors.at(&replay, 0.0),
            [
                DAILY_COLOR,
                DAILY_COLOR,
                INACTIVE_COLOR,
                MISSED_COLOR,
                ACTIVE_COLOR
            ]
        );
    }

    #[test]
    fn saved_assessment_defines_the_deadlines_in_the_scheduled_workload() {
        use leo_capture::scheduling::playback::{Assessment, DeadlineOutcome};
        let mut replay = replay(&[24.0, 72.0]);
        replay.document.assessment = Some(Assessment {
            job_cadence_hours: Some(24.0),
            requests: vec![DeadlineOutcome {
                completed_at_s: None,
                job: 0,
                slot: 0,
                deadline_s: 43200.0,
                complete: true,
                coverage_fraction: 1.0,
            }],
        });
        assert_eq!(
            JobColors::new(&replay).at(&replay, 0.0),
            [DAILY_COLOR, INACTIVE_COLOR]
        );
    }

    #[test]
    fn solar_colors_follow_forward_and_backward_seeks_and_saved_timing_modes() {
        let mut replay = replay(&[24.0, 72.0, 72.0, 4.0, 24.0, 72.0]);
        for job in &mut replay.jobs {
            job.aoi.latitude = 0.0;
            job.aoi.longitude = 0.0;
            job.acquisition.illumination = Illumination::Daylight;
            job.acquisition.fulfilment = Fulfilment::Mosaic;
            job.acquisition.mosaic_timing = MosaicTiming::SimilarSolarTime;
            job.acquisition.solar_time_hours = 12.0;
            job.acquisition.solar_tolerance_hours = 4.0;
            job.acquisition.min_solar_elevation_deg = 0.0;
        }
        // Noon in UTC is not exactly local solar noon, but is inside the 8h window.
        let noon = 12.0 * 3600.0;
        replay.jobs[1].acquisition.solar_time_hours = 0.0;
        replay.jobs[2].acquisition.solar_time_hours = 0.0;
        replay.jobs[2].acquisition.mosaic_timing = MosaicTiming::CloseInTime;
        replay.jobs[3].acquisition.illumination = Illumination::AnyTime;
        replay.missed_jobs[4] = true;
        replay.jobs[5].acquisition.first_deadline_hours = Some(48.0);
        let daylight = [
            DAILY_COLOR,
            UNAVAILABLE_COLOR,
            ACTIVE_COLOR,
            DAILY_COLOR,
            MISSED_COLOR,
            INACTIVE_COLOR,
        ];
        // Missed deadlines need the timing cue with zero captures as well as
        // with partial selected coverage.
        for selected in [false, true] {
            replay.scheduled_jobs[4] = selected;
            let colors = JobColors::new(&replay);
            assert_eq!(colors.at(&replay, noon), daylight);
            assert_eq!(
                colors.at(&replay, 0.0),
                [
                    UNAVAILABLE_COLOR,
                    UNAVAILABLE_COLOR,
                    UNAVAILABLE_COLOR,
                    DAILY_COLOR,
                    MISSED_UNAVAILABLE_COLOR,
                    INACTIVE_COLOR
                ]
            );
            assert_eq!(
                colors.at(&replay, noon),
                daylight,
                "Seeking must restore colors"
            );
        }
    }
}
