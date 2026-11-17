//! Full polygon unions for pruning and marginal insertion coverage.
//! Unlike reward-only evaluation, retain partial coverage and completed unions.
use super::super::{exact::Geometry, reward::RewardModel, verify, Model, Plan, Result};
use crate::geometry::Shape;
use geo::{Area, BooleanOps, MultiPolygon};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(super) struct Request {
    selected: BTreeSet<usize>,
    patches: BTreeMap<usize, Shape>,
    pub area: f64,
    pub complete: bool,
    pub reward: f64,
}

pub(super) struct Ledger<'a> {
    model: &'a Model,
    geometry: &'a Geometry,
    reward: &'a RewardModel,
    pub requests: Vec<Request>,
    pub evaluations: usize,
}

fn merge(patches: &mut BTreeMap<usize, Shape>, patch: usize, shape: &Shape) -> Result<f64> {
    let previous = patches
        .entry(patch)
        .or_insert_with(|| MultiPolygon::new(vec![]));
    let before = previous.unsigned_area();
    let combined = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| previous.union(shape)))
        .map_err(|_| "Postprocessing polygon union failed".to_string())?;
    let delta = combined.unsigned_area() - before;
    *previous = combined;
    Ok(delta)
}

impl<'a> Ledger<'a> {
    pub fn new(
        model: &'a Model,
        geometry: &'a Geometry,
        reward: &'a RewardModel,
        plan: &Plan,
    ) -> Result<Self> {
        let mut ledger = Self {
            model,
            geometry,
            reward,
            requests: Vec::new(),
            evaluations: 0,
        };
        let mut selected = vec![BTreeSet::new(); model.requests.len()];
        for (s, path) in plan.iter().enumerate() {
            for &a in path {
                for &id in &geometry.actions[s][a as usize] {
                    selected[geometry.crops[id].request].insert(id);
                }
            }
        }
        for (r, ids) in selected.into_iter().enumerate() {
            let request = ledger.evaluate(r, ids)?;
            ledger.requests.push(request);
        }
        Ok(ledger)
    }

    pub fn tolerance(&self, r: usize) -> f64 {
        // Tighter than completion tolerance, and always measured against the
        // start of the pruning pass to prevent accumulated area loss.
        1e-10 * self.model.atlas.job_areas_km2[self.model.requests[r].job].max(1.0)
    }

    fn evaluate(&mut self, r: usize, selected: BTreeSet<usize>) -> Result<Request> {
        self.evaluations += 1;
        let mut out = Request {
            selected,
            ..Default::default()
        };
        let job = &self.model.jobs[self.model.requests[r].job];
        let total = self.model.atlas.job_areas_km2[self.model.requests[r].job];
        let mut credited_area = 0.0;
        for &id in &out.selected {
            let crop = &self.geometry.crops[id];
            out.area += merge(&mut out.patches, crop.patch, &crop.shape)?;
            // Match the verifier's archive-order completion cutoff for reward,
            // while retaining the FULL union for safe pruning and refilling.
            if !out.complete {
                credited_area = out.area;
                out.complete = out.area >= total * (1.0 - verify::AREA_TOLERANCE);
            }
        }
        out.reward = self
            .reward
            .earned(job, total, credited_area.max(0.0), out.complete)?;
        Ok(out)
    }

    pub fn change(&mut self, s: usize, a: u32, add: bool) -> Result<Vec<(usize, Request)>> {
        let mut affected = BTreeMap::new();
        for &id in &self.geometry.actions[s][a as usize] {
            let r = self.geometry.crops[id].request;
            let ids = affected
                .entry(r)
                .or_insert_with(|| self.requests[r].selected.clone());
            if add {
                ids.insert(id);
            } else {
                ids.remove(&id);
            }
        }
        affected
            .into_iter()
            .map(|(r, ids)| Ok((r, self.evaluate(r, ids)?)))
            .collect()
    }

    pub fn accept(&mut self, change: Vec<(usize, Request)>) {
        for (r, request) in change {
            self.requests[r] = request;
        }
    }

    pub fn upper(&self, s: usize, a: usize, targets: &[bool]) -> f64 {
        self.geometry.actions[s][a]
            .iter()
            .map(|&id| {
                let c = &self.geometry.crops[id];
                if targets[c.request] && !self.requests[c.request].complete {
                    c.shape.unsigned_area() * self.weight(c.request)
                } else {
                    0.0
                }
            })
            .sum()
    }

    fn weight(&self, r: usize) -> f64 {
        self.reward.priority_base.powi(i32::from(
            self.model.jobs[self.model.requests[r].job]
                .acquisition
                .priority,
        ))
    }

    pub fn marginal(&mut self, s: usize, a: usize, targets: &[bool]) -> Result<f64> {
        self.evaluations += 1;
        let mut affected = BTreeMap::new();
        for &id in &self.geometry.actions[s][a] {
            let crop = &self.geometry.crops[id];
            let r = crop.request;
            if !targets[r] || self.requests[r].complete {
                continue;
            }
            let (patches, delta) = affected
                .entry(r)
                .or_insert_with(|| (self.requests[r].patches.clone(), 0.0));
            *delta += merge(patches, crop.patch, &crop.shape)?;
        }
        Ok(affected
            .into_iter()
            .map(|(r, (_, delta))| {
                if delta > self.tolerance(r) {
                    delta * self.weight(r)
                } else {
                    0.0
                }
            })
            .sum())
    }

    pub fn total_reward(&self) -> f64 {
        self.requests.iter().map(|r| r.reward).sum()
    }
}
