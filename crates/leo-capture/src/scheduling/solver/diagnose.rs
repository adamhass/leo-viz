//! Optimistic candidate-union coverage versus a saved, feasible plan.
use super::*;
use geo::BooleanOps;
use std::{collections::BTreeMap, io::BufRead, path::Path};

#[derive(Default, Serialize)]
struct Counts {
    due_requests: u64,
    scheduled_complete: u64,
    candidate_union_complete: u64,
    no_candidate_crops: u64,
    incomplete_even_with_all_candidates: u64,
    candidate_union_complete_but_scheduled_miss: u64,
    /// none, (0,50%), [50,90%), [90,99%), [99%,completion), complete
    scheduled_coverage_bins: [u64; 6],
    candidate_coverage_bins: [u64; 6],
    scheduled_fraction_sum: f64,
    candidate_fraction_sum: f64,
}
fn bin(fraction: f64, complete: bool) -> usize {
    if complete {
        5
    } else if fraction <= 0.0 {
        0
    } else if fraction < 0.5 {
        1
    } else if fraction < 0.9 {
        2
    } else if fraction < 0.99 {
        3
    } else {
        4
    }
}
impl Counts {
    fn add(&mut self, s: &verify::Request, a: &verify::Request, total: f64, crops: u32) {
        self.due_requests += 1;
        self.scheduled_complete += u64::from(s.complete);
        self.candidate_union_complete += u64::from(a.complete);
        self.no_candidate_crops += u64::from(crops == 0);
        self.incomplete_even_with_all_candidates += u64::from(!a.complete);
        self.candidate_union_complete_but_scheduled_miss += u64::from(a.complete && !s.complete);
        let sf = (s.area / total).clamp(0.0, 1.0);
        let af = (a.area / total).clamp(0.0, 1.0);
        self.scheduled_coverage_bins[bin(sf, s.complete)] += 1;
        self.candidate_coverage_bins[bin(af, a.complete)] += 1;
        self.scheduled_fraction_sum += sf;
        self.candidate_fraction_sum += af;
    }
}
#[derive(Deserialize)]
struct SavedSchedule {
    satellites: Vec<SavedPath>,
}
#[derive(Deserialize)]
struct SavedPath {
    satellite: usize,
    captures: Vec<SavedCapture>,
}
#[derive(Deserialize)]
struct SavedCapture {
    action: u32,
    tick: u32,
    mode: usize,
}

pub fn diagnose(config: &Config, schedule: &Path) -> Result<serde_json::Value> {
    if !config.horizon_s.is_finite()
        || config.horizon_s <= 0.0
        || !(1..=16).contains(&config.samples_per_axis)
        || !(1..=64).contains(&config.workers)
    {
        return Err("Invalid diagnostic configuration".into());
    }
    resources::set_worker_limit(config.workers);
    resources::background_thread();
    let start = Instant::now();
    let model = input::load(config)?;
    let saved: SavedSchedule =
        serde_json::from_reader(fs::File::open(schedule).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let mut paths = BTreeMap::new();
    for p in saved.satellites {
        if paths.insert(p.satellite, p.captures).is_some() {
            return Err("Duplicate satellite in saved schedule".into());
        }
    }
    if paths.len() != model.graphs.len() {
        return Err("Saved schedule has wrong number of satellites".into());
    }
    let mut plan = Vec::new();
    for graph in &model.graphs {
        let captures = paths
            .remove(&graph.satellite)
            .ok_or("Missing satellite in saved schedule")?;
        for c in &captures {
            if graph
                .actions
                .get(c.action as usize)
                .is_none_or(|a| a.tick != c.tick || a.mode != c.mode)
            {
                return Err("Saved capture differs from its graph".into());
            }
        }
        let selected: Vec<_> = captures.iter().map(|c| c.action).collect();
        verify::validate_path(graph, &selected)?;
        verify::validate_maneuvers(&model, graph, &selected)?;
        plan.push(selected);
    }
    let empty = || {
        model
            .demands
            .iter()
            .map(|d| {
                (0..d.slots)
                    .map(|_| verify::Request::default())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let mut available = empty();
    let mut scheduled = empty();
    let mut crops: Vec<Vec<u32>> = model.demands.iter().map(|d| vec![0; d.slots]).collect();
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
    for (index, g) in model.graphs.iter().enumerate() {
        let mut selected = vec![false; g.actions.len()];
        for &a in &plan[index] {
            selected[a as usize] = true;
        }
        let mut read = 0;
        for line in input::reader(
            &config
                .graph_directory
                .join(format!("satellite-{:03}.actions.jsonl.gz", g.satellite)),
        )?
        .lines()
        {
            if read == g.actions.len() {
                break;
            }
            let record: input::ActionRecord =
                serde_json::from_str(&line.map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            if record.id as usize != read
                || record.tick != g.actions[read].tick
                || record.mode != g.actions[read].mode
            {
                return Err("Action file changed during diagnosis".into());
            }
            let take = selected[read];
            read += 1;
            for c in record.crops {
                input::validate_crop(&c, &g.actions[record.id as usize], &model.jobs)?;
                if c.slot as usize >= model.demands[c.job].slots {
                    continue;
                }
                let slot = c.slot as usize;
                crops[c.job][slot] += 1;
                let a = &mut available[c.job][slot];
                let s = &mut scheduled[c.job][slot];
                if a.complete && (!take || s.complete) {
                    continue;
                }
                let original = originals
                    .get(&(c.job, c.patch))
                    .ok_or("Unknown original AOI chart")?;
                let shape = input::shape(c.polygons_xy_km);
                let clipped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    shape.intersection(*original)
                }))
                .map_err(|_| "Diagnostic polygon clipping failed".to_string())?;
                let total = model.atlas.job_areas_km2[c.job];
                a.add(c.patch, &clipped, total)?;
                if take {
                    s.add(c.patch, &clipped, total)?;
                }
            }
            if read % 128 == 0 {
                resources::check_memory()?;
            }
        }
        if read != g.actions.len() {
            return Err("Truncated action file during diagnosis".into());
        }
        eprintln!(
            "Candidate-union diagnostic: {}/{} satellites; {:.1}s",
            index + 1,
            model.graphs.len(),
            start.elapsed().as_secs_f64()
        );
    }
    let mut totals = Counts::default();
    let mut groups = BTreeMap::<String, Counts>::new();
    let mut by_priority = BTreeMap::<u16, Counts>::new();
    let mut inconsistent = 0;
    for (j, job) in model.jobs.iter().enumerate() {
        let group = format!(
            "{}h / {}",
            job.cadence_hours,
            match job.acquisition.fulfilment {
                crate::workload::Fulfilment::Mosaic => "mosaic",
                crate::workload::Fulfilment::Independent => "independent",
            }
        );
        for slot in 0..model.demands[j].slots {
            let s = &scheduled[j][slot];
            let a = &available[j][slot];
            let area = model.atlas.job_areas_km2[j];
            let n = crops[j][slot];
            inconsistent += u64::from(s.complete && !a.complete);
            totals.add(s, a, area, n);
            groups.entry(group.clone()).or_default().add(s, a, area, n);
            by_priority
                .entry(job.acquisition.priority)
                .or_default()
                .add(s, a, area, n);
        }
    }
    Ok(serde_json::json!({
        "schema_version":1,"horizon_s":config.horizon_s,"schedule":schedule,"elapsed_s":start.elapsed().as_secs_f64(),
        "preset_hash_fnv1a64":model.manifest.preset_hash_fnv1a64,
        "totals":totals,"by_cadence_and_fulfilment":groups,"by_priority":by_priority,
        "scheduled_complete_but_candidate_union_incomplete":inconsistent,
        "coverage_bin_labels":["zero","0_to_50_percent","50_to_90_percent","90_to_99_percent","99_percent_to_completion_threshold","complete"],
        "completion_relative_area_tolerance":1e-6,
        "interpretation":[
            "All eligible candidate crops are unioned, including mutually incompatible and source-unreachable choices. This is an optimistic geometric coverage bound, not a feasible schedule.",
            "An incomplete candidate union proves that request cannot be completed with these exported candidates, within the numerical geometry model and tolerance. It does not prove physical impossibility under finer time/pointing sampling.",
            "Complete candidate union does not establish a feasible path or constellation schedule. Remaining misses combine scheduling conflicts and heuristic limitations; this diagnostic does not separate them.",
            "Independent-crop jobs yield incremental data; they are still counted complete here only when the entire requested area is covered within the cadence window."
        ]
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnostic_categories_partition_missed_deadlines() {
        let r = |area, complete| {
            let mut r = verify::Request::default();
            r.area = area;
            r.complete = complete;
            r
        };
        let mut c = Counts::default();
        c.add(&r(1., true), &r(1., true), 1., 3);
        c.add(&r(0., false), &r(0., false), 1., 0);
        c.add(&r(0.5, false), &r(0.9, false), 1., 5);
        c.add(&r(0.995, false), &r(1., true), 1., 4);
        assert_eq!(
            c.scheduled_complete
                + c.incomplete_even_with_all_candidates
                + c.candidate_union_complete_but_scheduled_miss,
            c.due_requests
        );
        assert_eq!(c.scheduled_coverage_bins, [1, 0, 1, 0, 1, 1]);
        assert_eq!(c.no_candidate_crops, 1);
    }
}
