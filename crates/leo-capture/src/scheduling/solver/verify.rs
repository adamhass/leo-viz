use super::*;
use geo::{Area, BooleanOps, MultiPolygon};
use std::{collections::BTreeMap, io::BufRead};

const AREA_TOLERANCE: f64 = 1e-6;
#[derive(Default)]
pub(super) struct Request {
    patches: BTreeMap<usize, crate::geometry::Shape>,
    pub(super) area: f64,
    pub(super) complete: bool,
}
impl Request {
    pub(super) fn add(
        &mut self,
        patch: usize,
        shape: &crate::geometry::Shape,
        total: f64,
    ) -> Result<()> {
        if self.complete {
            return Ok(());
        }
        let previous = self
            .patches
            .entry(patch)
            .or_insert_with(|| MultiPolygon::new(vec![]));
        let before = previous.unsigned_area();
        let merged =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| previous.union(shape)))
                .map_err(|_| "Polygon coverage union failed".to_string())?;
        self.area += merged.unsigned_area() - before;
        *previous = merged;
        if self.area >= total * (1.0 - AREA_TOLERANCE) {
            self.complete = true;
            self.patches.clear();
        }
        Ok(())
    }
}
pub(super) fn validate_path(g: &Graph, path: &[u32]) -> Result<()> {
    let mut reward = vec![0.0; g.actions.len()];
    let mut previous = None;
    for &a in path {
        if a as usize >= g.actions.len()
            || reward[a as usize] != 0.0
            || previous.is_some_and(|t| g.actions[a as usize].start_s < t - 1e-9)
        {
            return Err("Schedule contains an invalid, repeated or unordered capture".into());
        }
        reward[a as usize] = 1.0;
        previous = Some(g.actions[a as usize].end_s);
    }
    // All selected captures must fit on one actual source-to-sink graph path.
    if search::longest_path(g, &reward).0 != path.len() as f64 {
        return Err(format!(
            "Satellite {} has an infeasible selected path",
            g.satellite
        ));
    }
    Ok(())
}
pub(super) fn validate_maneuvers(model: &Model, g: &Graph, path: &[u32]) -> Result<()> {
    let mut previous = super::super::Mode::new(0.0, 0.0);
    let mut ready_s = 0.0;
    for &id in path {
        let a = g.actions.get(id as usize).ok_or("Invalid capture ID")?;
        let next = model
            .manifest
            .modes
            .get(a.mode)
            .ok_or("Invalid pointing mode")?;
        let slew = previous.transition_s(next, &model.manifest.config.options);
        if a.start_s + 1e-9 < ready_s + slew {
            return Err(format!(
                "Satellite {} capture {} violates precise maneuver timing",
                g.satellite, id
            ));
        }
        ready_s = a.end_s;
        previous = next.clone();
    }
    Ok(())
}
pub(super) fn candidates(
    model: &Model,
    config: &Config,
    candidates: &[Candidate],
) -> Result<(usize, Vec<serde_json::Value>)> {
    for candidate in candidates {
        if candidate.plan.len() != model.graphs.len() {
            return Err("Wrong number of satellite paths".into());
        }
        for (g, path) in model.graphs.iter().zip(&candidate.plan) {
            validate_path(g, path)?;
            validate_maneuvers(model, g, path)?;
        }
    }
    // Identical incumbent plans share one verifier state.
    let mut unique: Vec<usize> = Vec::new();
    let mapping: Vec<_> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if let Some(k) = unique.iter().position(|&j| candidates[j].plan == c.plan) {
                k
            } else {
                unique.push(i);
                unique.len() - 1
            }
        })
        .collect();
    let mut states: Vec<Vec<Vec<Request>>> = unique
        .iter()
        .map(|_| {
            model
                .demands
                .iter()
                .map(|d| (0..d.slots).map(|_| Request::default()).collect())
                .collect()
        })
        .collect();
    let originals: BTreeMap<_, _> = model
        .atlas
        .patches
        .iter()
        .enumerate()
        .flat_map(|(p, patch)| {
            patch
                .pieces
                .iter()
                .map(move |piece| ((piece.job, p), &piece.shape))
        })
        .collect();
    eprintln!(
        "Verifying {} distinct candidate plans with polygon unions",
        unique.len()
    );
    for (s, g) in model.graphs.iter().enumerate() {
        let mut selected = vec![Vec::new(); g.actions.len()];
        for (k, &candidate) in unique.iter().enumerate() {
            for &a in &candidates[candidate].plan[s] {
                selected[a as usize].push(k);
            }
        }
        let mut read = 0;
        for line in input::reader(
            &config
                .graph_directory
                .join(format!("satellite-{:03}.actions.jsonl.gz", g.satellite)),
        )?
        .lines()
        {
            // Files contain a chronological prefix; parse only that prefix.
            if read == g.actions.len() {
                break;
            }
            let record: input::ActionRecord =
                serde_json::from_str(&line.map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            if record.id as usize != read
                || record.tick != g.actions[read].tick
                || record.mode != g.actions[read].mode
                || record.action(model.manifest.config.options.step_s).start_s
                    != g.actions[read].start_s
                || record.action(model.manifest.config.options.step_s).end_s
                    != g.actions[read].end_s
            {
                return Err("Action file changed during scheduling".into());
            }
            let targets = &selected[read];
            read += 1;
            if targets.is_empty() {
                continue;
            }
            for crop in record.crops {
                input::validate_crop(&crop, &g.actions[record.id as usize], &model.jobs)?;
                if crop.slot as usize >= model.demands[crop.job].slots {
                    continue;
                }
                let original = originals
                    .get(&(crop.job, crop.patch))
                    .ok_or("Unknown crop chart during verification")?;
                let shape = input::shape(crop.polygons_xy_km);
                let clipped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    shape.intersection(*original)
                }))
                .map_err(|_| "Polygon clipping failed during verification".to_string())?;
                for &k in targets {
                    states[k][crop.job][crop.slot as usize]
                        .add(crop.patch, &clipped, model.atlas.job_areas_km2[crop.job])
                        .map_err(|e| {
                            format!(
                                "Job {}, slot {}, satellite {}: {e}",
                                crop.job, crop.slot, g.satellite
                            )
                        })?;
                }
            }
            if read % 128 == 0 {
                resources::check_memory()?;
            }
        }
        if read != g.actions.len() {
            return Err("Action file truncated during verification".into());
        }
        eprintln!(
            "Polygon verification: {}/{} satellites",
            s + 1,
            model.graphs.len()
        );
    }
    let levels = search::priorities(model);
    let jobs_in_scope = model.included_jobs.iter().filter(|&&yes| yes).count();
    let index: BTreeMap<_, _> = levels.iter().enumerate().map(|(i, &p)| (p, i)).collect();
    let mut scores = Vec::new();
    let mut summaries = Vec::new();
    for state in &states {
        let mut score = Score {
            complete_requests: vec![0; levels.len()],
            fractional_requests: 0.0,
        };
        let mut due_jobs = vec![0u64; levels.len()];
        let mut due_requests = vec![0u64; levels.len()];
        let mut partial = vec![0u64; levels.len()];
        let mut no_coverage = vec![0u64; levels.len()];
        let mut collection_modes = BTreeMap::<&str, [u64; 4]>::new();
        for (j, requests) in state.iter().enumerate() {
            if requests.is_empty() {
                continue;
            }
            let level = index[&model.jobs[j].acquisition.priority];
            due_jobs[level] += 1;
            due_requests[level] += requests.len() as u64;
            score.complete_requests[level] += requests.iter().filter(|r| r.complete).count() as u64;
            partial[level] += requests
                .iter()
                .filter(|r| !r.complete && r.area > 0.0)
                .count() as u64;
            no_coverage[level] += requests
                .iter()
                .filter(|r| !r.complete && r.area <= 0.0)
                .count() as u64;
            let mode = if model.jobs[j].acquisition.fulfilment
                == crate::workload::Fulfilment::Independent
            {
                "Independent crops"
            } else {
                model.jobs[j].acquisition.mosaic_timing.label()
            };
            let counts = collection_modes.entry(mode).or_default();
            for request in requests {
                counts[0] += 1;
                counts[if request.complete {
                    1
                } else if request.area > 0.0 {
                    2
                } else {
                    3
                }] += 1;
            }
            score.fractional_requests += requests
                .iter()
                .map(|r| (r.area / model.atlas.job_areas_km2[j]).min(1.0))
                .sum::<f64>();
        }
        let due = due_requests.iter().sum::<u64>();
        let met = score.complete_requests.iter().sum::<u64>();
        summaries.push(serde_json::json!({
            "path_feasibility":"passed","coverage_verification":"polygon_union",
            "relative_area_tolerance":AREA_TOLERANCE,
            "deadline_assessment":"planned coverage, not executed acquisitions",
            "ongoing_jobs":model.jobs.len(),
            "jobs_in_scope":jobs_in_scope,
            "jobs_excluded_from_run":model.jobs.len()-jobs_in_scope,
            "jobs_with_due_deadlines":due_jobs.iter().sum::<u64>(),
            "jobs_without_due_deadlines":jobs_in_scope as u64-due_jobs.iter().sum::<u64>(),
            "due_acquisition_deadlines":due,
            "planned_met_deadlines":met,
            "planned_missed_deadlines":due-met,
            "planned_miss_rate_percent":miss_rate(due,met),
            "missed_deadlines_with_partial_coverage":partial.iter().sum::<u64>(),
            "missed_deadlines_with_no_coverage":no_coverage.iter().sum::<u64>(),
            "pending_acquisition_deadlines":model.jobs.iter().zip(&model.included_jobs)
                .filter(|(j,included)|**included && deadline_counts(j,config.horizon_s).1).count(),
            "by_cadence":cadence_summary(model,config.horizon_s),
            "by_collection_mode":collection_modes.iter().map(|(mode,c)|serde_json::json!({
                "mode":mode,"due_acquisition_deadlines":c[0],"planned_met_deadlines":c[1],
                "planned_missed_deadlines":c[0]-c[1],"planned_miss_rate_percent":miss_rate(c[0],c[1]),
                "missed_deadlines_with_partial_coverage":c[2],"missed_deadlines_with_no_coverage":c[3]
            })).collect::<Vec<_>>(),
            "score":score,
            "by_priority":levels.iter().enumerate().map(|(i,&priority)|serde_json::json!({
                "priority":priority,"jobs_with_due_deadlines":due_jobs[i],
                "due_acquisition_deadlines":due_requests[i],"planned_met_deadlines":score.complete_requests[i],
                "planned_missed_deadlines":due_requests[i]-score.complete_requests[i],
                "planned_miss_rate_percent":miss_rate(due_requests[i],score.complete_requests[i]),
                "missed_deadlines_with_partial_coverage":partial[i],"missed_deadlines_with_no_coverage":no_coverage[i]
            })).collect::<Vec<_>>()
        }));
        scores.push(score);
    }
    let reports: Vec<_> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let mut value = summaries[mapping[i]].clone();
            value["candidate"] = serde_json::json!(c.name);
            value["capture_count"] = serde_json::json!(c.plan.iter().map(Vec::len).sum::<usize>());
            value["sampled_score"] =
                serde_json::json!(search::evaluate(model, &search::counts(model, &c.plan)));
            value
        })
        .collect();
    let mut best = 0;
    for i in 1..candidates.len() {
        if scores[mapping[i]].better_than(&scores[mapping[best]]) {
            best = i;
        }
    }
    eprintln!(
        "Selected {}: {} polygon-verified acquisition deadlines met",
        candidates[best].name,
        scores[mapping[best]].complete_requests.iter().sum::<u64>()
    );
    Ok((best, reports))
}

pub(super) fn cadence_summary(model: &Model, horizon_s: f64) -> Vec<serde_json::Value> {
    let mut groups = BTreeMap::<u64, (u64, u64, u64, f64, f64)>::new();
    for (job, &included) in model.jobs.iter().zip(&model.included_jobs) {
        if !included {
            continue;
        }
        let first_deadline_s = job.acquisition.first_deadline_s(job.cadence_hours);
        let entry = groups.entry(job.cadence_hours.to_bits()).or_insert((
            0,
            0,
            0,
            first_deadline_s,
            first_deadline_s,
        ));
        let (due, pending) = deadline_counts(job, horizon_s);
        entry.0 += 1;
        entry.1 += due as u64;
        entry.2 += u64::from(pending);
        entry.3 = entry.3.min(first_deadline_s);
        entry.4 = entry.4.max(first_deadline_s);
    }
    groups.into_iter().map(|(cadence,(jobs,due,pending,first_min,first_max))|serde_json::json!({
        "cadence_hours":f64::from_bits(cadence),"jobs":jobs,"due_acquisition_deadlines":due,
        "pending_acquisition_deadlines":pending,"first_deadline_s_after_epoch_min":first_min,
        "first_deadline_s_after_epoch_max":first_max
    })).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo::Rect;
    #[test]
    fn overlapping_crops_are_not_summed_and_complementary_crops_complete() {
        let shape = |x0, x1| MultiPolygon::new(vec![Rect::new((x0, 0.), (x1, 1.)).to_polygon()]);
        let mut request = Request::default();
        request.add(0, &shape(0., 0.6), 1.).unwrap();
        request.add(0, &shape(0., 0.6), 1.).unwrap();
        assert!(!request.complete);
        assert!((request.area - 0.6).abs() < 1e-7);
        request.add(0, &shape(0.5, 1.), 1.).unwrap();
        assert!(request.complete);
    }
}
