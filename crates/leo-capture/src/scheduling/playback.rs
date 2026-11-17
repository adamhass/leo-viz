//! Portable, validated replay of selected capture actions. No new scheduling.
mod export;
mod io;
#[cfg(test)]
mod tests;

pub use export::export;

use super::{Mode, Options};
use crate::workload::Job;
use leo_sim::{
    geometry::{self, Vector, EARTH_RADIUS_KM as R},
    orbit::WalkerShell,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Crop {
    pub job: usize,
    pub slot: u64,
    pub start_s: f64,
    pub end_s: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Capture {
    #[serde(alias = "id")]
    pub action: u32,
    pub tick: u32,
    pub mode: usize,
    pub start_s: f64,
    pub end_s: f64,
    #[serde(default)]
    pub crops: Vec<Crop>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Satellite {
    pub satellite: usize,
    pub captures: Vec<Capture>,
}
#[derive(Serialize, Deserialize)]
pub struct Document {
    /// Polygon-verified results for deadlines inside this run, absent in older bundles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assessment: Option<Assessment>,
    pub schema_version: u32,
    pub epoch_unix_s: f64,
    pub horizon_s: f64,
    pub options: Options,
    pub constellation: WalkerShell,
    pub modes: Vec<Mode>,
    /// Full saved AOIs and workload, making playback independent of graph files.
    pub preset: serde_json::Value,
    pub satellites: Vec<Satellite>,
    pub selected_candidate: String,
    pub source_preset_hash_fnv1a64: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeadlineOutcome {
    pub job: usize,
    pub slot: u64,
    pub deadline_s: f64,
    pub coverage_fraction: f64,
    pub complete: bool,
    /// Seconds since the replay epoch: first crop-end event whose polygon union
    /// meets the verifier's full-coverage threshold. Absent in legacy exports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_s: Option<f64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Assessment {
    pub job_cadence_hours: Option<f64>,
    pub requests: Vec<DeadlineOutcome>,
}
pub struct Replay {
    pub document: Document,
    pub jobs: Vec<Job>,
    pub scheduled_jobs: Vec<bool>,
    pub missed_jobs: Vec<bool>,
    pub first_capture_end_s: Vec<f64>,
    pub capture_ends: Vec<f64>,
    pub capture_starts: Vec<f64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Slewing,
    Settling,
    Capturing,
}
pub struct Pointing {
    pub centre: Vector,
    pub normal: Vector,
    pub satellite: Vector,
    pub state: State,
}
impl Replay {
    pub fn new(document: Document) -> Result<Self> {
        document.options.validate()?;
        if !matches!(document.schema_version, 1 | 2)
            || !document.epoch_unix_s.is_finite()
            || !document.horizon_s.is_finite()
            || document.horizon_s <= 0.0
            || document.horizon_s > document.options.duration_s
            || document.satellites.is_empty()
        {
            return Err("Invalid replay version, epoch or horizon".into());
        }
        // The visualizer currently replays the exact WorldView design shell.
        if serde_json::to_value(&document.constellation).map_err(|e| e.to_string())?
            != serde_json::to_value(leo_sim::worldview::Fleet::WorldView60.shell()).unwrap()
        {
            return Err("Replay requires the saved WorldView-60 design constellation".into());
        }
        let expected = document.options.modes();
        if document.schema_version == 1
            && (expected.len() != document.modes.len()
                || expected.iter().zip(&document.modes).any(|(a, b)| {
                    a.along_km != b.along_km
                        || a.cross_km != b.cross_km
                        || a.attitude
                            .iter()
                            .zip(b.attitude)
                            .any(|(x, y)| !y.is_finite() || (x - y).abs() > 1e-12)
                }))
        {
            return Err("Replay pointing modes disagree with its observation model".into());
        }
        for mode in &document.modes {
            if !mode.along_km.is_finite()
                || !mode.cross_km.is_finite()
                || mode.along_km.hypot(mode.cross_km)
                    > (document.options.reach_km - document.options.width_km) / 2.0 + 1e-8
                || Mode::new(mode.along_km, mode.cross_km)
                    .attitude
                    .iter()
                    .zip(mode.attitude)
                    .any(|(a, b)| !b.is_finite() || (a - b).abs() > 1e-12)
            {
                return Err("Invalid replay pointing geometry".into());
            }
        }
        let workload: crate::storage::Document =
            crate::storage::Document::deserialize(&document.preset["workload"])
                .map_err(|e| e.to_string())?;
        let jobs = workload.into_current()?.jobs;
        let mut scheduled_jobs = vec![false; jobs.len()];
        let mut first_capture_end_s = vec![f64::INFINITY; jobs.len()];
        let mut ends = Vec::new();
        let mut starts = Vec::new();
        let mut ids = BTreeSet::new();
        let mut request_crop_ends: BTreeMap<(usize, u64), Vec<f64>> = BTreeMap::new();
        for sat in &document.satellites {
            if sat.satellite >= 60 || !ids.insert(sat.satellite) {
                return Err("Invalid or duplicate replay satellite".into());
            }
            let mut previous = Mode::new(0.0, 0.0);
            let mut end = 0.0;
            let mut actions = BTreeSet::new();
            for c in &sat.captures {
                let mode = document
                    .modes
                    .get(c.mode)
                    .ok_or("Invalid replay pointing mode")?;
                if !c.start_s.is_finite()
                    || !c.end_s.is_finite()
                    || c.start_s < 0.0
                    || c.end_s <= c.start_s
                    || c.end_s > document.horizon_s
                    || c.start_s + 1e-8 < end + previous.transition_s(mode, &document.options)
                    || !actions.insert(c.action)
                    || document.schema_version == 1
                        && ((c.start_s - c.tick as f64 * document.options.step_s).abs() > 1e-8
                            || (c.end_s - c.start_s - document.options.step_s).abs() > 1e-8)
                    || c.crops.is_empty()
                {
                    return Err(format!(
                        "Invalid capture or maneuver on satellite {}",
                        sat.satellite
                    ));
                }
                for crop in &c.crops {
                    let job = jobs
                        .get(crop.job)
                        .ok_or("Replay crop references an unknown job")?;
                    let window = job
                        .acquisition
                        .collection_window_s(crop.slot, job.cadence_hours);
                    if !crop.start_s.is_finite()
                        || !crop.end_s.is_finite()
                        || crop.end_s <= crop.start_s
                        || crop.start_s < c.start_s - 1e-6
                        || crop.end_s > c.end_s + 1e-6
                        || crop.start_s < window.0 - 1e-6
                        || crop.end_s > window.1 + 1e-6
                    {
                        return Err(
                            "Replay crop lies outside its capture or collection window".into()
                        );
                    }
                    request_crop_ends
                        .entry((crop.job, crop.slot))
                        .or_default()
                        .push(crop.end_s);
                    scheduled_jobs[crop.job] = true;
                    first_capture_end_s[crop.job] = first_capture_end_s[crop.job].min(crop.end_s);
                }
                previous = mode.clone();
                end = c.end_s;
                ends.push(c.end_s);
                starts.push(c.start_s);
            }
        }
        let mut missed_jobs = vec![false; jobs.len()];
        if let Some(assessment) = &document.assessment {
            if assessment
                .job_cadence_hours
                .is_some_and(|h| !h.is_finite() || h <= 0.0)
            {
                return Err("Invalid deadline assessment scope".into());
            }
            let mut expected = BTreeSet::new();
            for (j, job) in jobs.iter().enumerate() {
                if assessment
                    .job_cadence_hours
                    .is_none_or(|h| h == job.cadence_hours)
                {
                    for slot in 0..job
                        .acquisition
                        .deadline_counts(job.cadence_hours, document.horizon_s)
                        .0
                    {
                        expected.insert((j, slot as u64));
                    }
                }
            }
            for r in &assessment.requests {
                let job = jobs
                    .get(r.job)
                    .ok_or("Unknown job in deadline assessment")?;
                let deadline = job
                    .acquisition
                    .collection_window_s(r.slot, job.cadence_hours)
                    .1;
                if !expected.remove(&(r.job, r.slot))
                    || !r.deadline_s.is_finite()
                    || (r.deadline_s - deadline).abs() > 1e-6
                    || !r.coverage_fraction.is_finite()
                    || !(0.0..=1.0).contains(&r.coverage_fraction)
                    || r.complete != (r.coverage_fraction >= 1.0 - 1e-6)
                    || r.completed_at_s.is_some_and(|time| {
                        !r.complete
                            || !time.is_finite()
                            || time < 0.0
                            || time > document.horizon_s
                            || time > deadline + 1e-6
                            || request_crop_ends.get(&(r.job, r.slot)).is_none_or(|ends| {
                                !ends.iter().any(|end| (end - time).abs() <= 1e-6)
                            })
                    })
                {
                    return Err("Invalid or duplicate deadline assessment".into());
                }
                missed_jobs[r.job] |= !r.complete;
            }
            if !expected.is_empty() {
                return Err("Deadline assessment is incomplete".into());
            }
        }
        ends.sort_by(f64::total_cmp);
        starts.sort_by(f64::total_cmp);
        Ok(Self {
            document,
            jobs,
            scheduled_jobs,
            missed_jobs,
            first_capture_end_s,
            capture_ends: ends,
            capture_starts: starts,
        })
    }
    pub fn captured_segments(&self, time: f64) -> usize {
        self.capture_ends.partition_point(|&end| end <= time)
    }
    pub fn next_capture(&self, time: f64) -> Option<f64> {
        self.capture_starts
            .get(
                self.capture_starts
                    .partition_point(|&start| start <= time + 1e-7),
            )
            .copied()
    }
    pub fn pointing(&self, satellite: &Satellite, time: f64) -> Pointing {
        let t = time.clamp(0.0, self.document.horizon_s);
        let captures = &satellite.captures;
        let i = captures.partition_point(|c| c.end_s <= t);
        let previous = if i == 0 {
            Mode::new(0.0, 0.0)
        } else {
            self.document.modes[captures[i - 1].mode].clone()
        };
        let mut mode = previous.clone();
        let mut state = State::Idle;
        if let Some(next) = captures.get(i) {
            let target = &self.document.modes[next.mode];
            let duration = previous.transition_s(target, &self.document.options);
            if t >= next.start_s {
                mode = target.clone();
                state = State::Capturing;
            } else if duration > 0.0 && t >= next.start_s - duration {
                let moving = duration - self.document.options.settle_s;
                let phase = ((t - (next.start_s - duration)) / moving.max(1e-12)).clamp(0.0, 1.0);
                mode = interpolate(&previous, target, phase);
                state = if phase < 1.0 {
                    State::Slewing
                } else {
                    State::Settling
                };
            }
        }
        let elements = self
            .document
            .constellation
            .elements(satellite.satellite / 3, satellite.satellite % 3);
        let (q, n) = super::sample(&elements, self.document.epoch_unix_s, t);
        let (centre, normal) = mode.offset(q, n);
        Pointing {
            centre,
            normal,
            satellite: geometry::scale(q, (R + leo_sim::worldview::ALTITUDE_KM) / R),
            state,
        }
    }
    /// Unsteered field of regard; deliberately independent of the selected mode.
    pub fn access_scanline(&self, satellite: usize, time: f64, width_km: f64) -> [Vector; 2] {
        let elements = self
            .document
            .constellation
            .elements(satellite / 3, satellite % 3);
        let (q, n) = super::sample(&elements, self.document.epoch_unix_s, time);
        edges(q, n, width_km)
    }
    pub fn scanline(&self, satellite: usize, mode: usize, time: f64) -> [Vector; 2] {
        let elements = self
            .document
            .constellation
            .elements(satellite / 3, satellite % 3);
        let (q, n) = super::sample(&elements, self.document.epoch_unix_s, time);
        let (q, n) = self.document.modes[mode].offset(q, n);
        edges(q, n, self.document.options.width_km)
    }
}
pub fn edges(q: Vector, n: Vector, width_km: f64) -> [Vector; 2] {
    let half = width_km / (2.0 * R);
    [-1.0, 1.0].map(|sign| std::array::from_fn(|i| q[i] * half.cos() + sign * n[i] * half.sin()))
}
// Shortest quaternion path at the graph's calibrated angular rate. Convert its
// look direction back to a ground intercept, retaining the orbit-relative frame.
fn interpolate(a: &Mode, b: &Mode, t: f64) -> Mode {
    if t <= 0.0 {
        return a.clone();
    }
    if t >= 1.0 {
        return b.clone();
    }
    let dot = a
        .attitude
        .iter()
        .zip(b.attitude)
        .map(|(x, y)| x * y)
        .sum::<f64>();
    let sign = dot.signum();
    let angle = dot.abs().clamp(0.0, 1.0).acos();
    let q: [f64; 4] = if angle < 1e-10 {
        a.attitude
    } else {
        std::array::from_fn(|i| {
            ((1.0 - t) * angle).sin() / angle.sin() * a.attitude[i]
                + (t * angle).sin() / angle.sin() * sign * b.attitude[i]
        })
    };
    let [w, x, y, z] = q;
    let look = [
        2.0 * (x * z + w * y),
        2.0 * (y * z - w * x),
        1.0 - 2.0 * (x * x + y * y),
    ];
    let radial = R + leo_sim::worldview::ALTITUDE_KM;
    let distance = radial * look[2]
        - (R * R - radial * radial * (1.0 - look[2] * look[2]))
            .max(0.0)
            .sqrt();
    let horizontal = look[0].hypot(look[1]);
    if horizontal < 1e-12 {
        return Mode::new(0.0, 0.0);
    }
    let ground = R * (distance * horizontal).atan2(radial - distance * look[2]);
    Mode::new(ground * look[0] / horizontal, ground * look[1] / horizontal)
}
