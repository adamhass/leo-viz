//! Experimental Lagrangian coverage decomposition. See SCHEDULING-SOLVER.md.
mod diagnose;
mod input;
pub(super) mod replay;
mod search;
mod verify;

pub use diagnose::diagnose;

use crate::{resources, workload::Job};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf, time::Instant};

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub graph_directory: PathBuf,
    pub output: PathBuf,
    /// Prefix of the original graph, starting at its original epoch.
    pub horizon_s: f64,
    /// Restrict the objective and deadline assessment to this exact cadence.
    /// None includes every job. Original graph job indices are preserved.
    pub job_cadence_hours: Option<f64>,
    pub iterations_per_priority: usize,
    pub samples_per_axis: usize,
    pub repair_sweeps: usize,
    pub workers: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            graph_directory: PathBuf::new(),
            output: PathBuf::new(),
            horizon_s: 14400.0,
            job_cadence_hours: None,
            iterations_per_priority: 12,
            samples_per_axis: 3,
            repair_sweeps: 1,
            workers: 4,
        }
    }
}

#[derive(Clone)]
struct Graph {
    satellite: usize,
    actions: Vec<super::Action>,
    /// Witness IDs credited by each capture, deduplicated within that capture.
    coverage: Vec<Vec<u32>>,
    edges: Vec<Vec<u32>>,
    order: Vec<usize>,
    source: usize,
    sink: usize,
}
#[derive(Clone)]
struct Demand {
    start: usize,
    points_per_slot: usize,
    slots: usize,
}
impl Demand {
    fn len(&self) -> usize {
        self.points_per_slot * self.slots
    }
}
/// One recurring acquisition deadline, independent of the job's other windows.
struct AcquisitionRequest {
    job: usize,
    start: usize,
    points: usize,
}

fn request_index(demands: &[Demand]) -> (Vec<AcquisitionRequest>, Vec<usize>) {
    let mut requests = Vec::new();
    let mut point_request = Vec::new();
    for (job, demand) in demands.iter().enumerate() {
        for slot in 0..demand.slots {
            let start = demand.start + slot * demand.points_per_slot;
            assert_eq!(start, point_request.len());
            point_request.extend(std::iter::repeat_n(requests.len(), demand.points_per_slot));
            requests.push(AcquisitionRequest {
                job,
                start,
                points: demand.points_per_slot,
            });
        }
    }
    (requests, point_request)
}

/// Count only deadlines closing in this horizon, including a partial first cycle.
fn deadline_counts(job: &Job, horizon_s: f64) -> (usize, bool) {
    job.acquisition
        .deadline_counts(job.cadence_hours, horizon_s)
}

fn miss_rate(due: u64, met: u64) -> Option<f64> {
    (due > 0).then(|| 100.0 * (due - met) as f64 / due as f64)
}
struct Model {
    jobs: Vec<Job>,
    included_jobs: Vec<bool>,
    demands: Vec<Demand>,
    requests: Vec<AcquisitionRequest>,
    point_request: Vec<usize>,
    graphs: Vec<Graph>,
    atlas: crate::geometry::Atlas,
    manifest: super::Summary,
}
type Plan = Vec<Vec<u32>>;

/// Each met acquisition deadline earns credit independently of other windows.
/// Priorities are stored in descending order; no floating big-M objective here.
#[derive(Clone, Debug, Default, Serialize)]
struct Score {
    complete_requests: Vec<u64>,
    fractional_requests: f64,
}
impl Score {
    fn better_than(&self, other: &Self) -> bool {
        self.complete_requests
            .cmp(&other.complete_requests)
            .then(
                self.fractional_requests
                    .total_cmp(&other.fractional_requests),
            )
            .is_gt()
    }
}

#[derive(Serialize)]
struct Iteration {
    priority: u16,
    iteration: usize,
    /// Bound only for this stage's sampled objective, including protected-job weights.
    sampled_dual_upper: f64,
    sampled_primal_lower: f64,
    best_sampled_score: Score,
}
struct Candidate {
    name: String,
    plan: Plan,
}

pub fn solve(config: &Config) -> Result<serde_json::Value> {
    solve_selected(config, false)
}

/// Reproduce and save the same deterministic independent-path candidate that
/// is normally compared with the priority-stage candidates.
pub fn solve_baseline(config: &Config) -> Result<serde_json::Value> {
    solve_selected(config, true)
}

fn solve_selected(config: &Config, baseline_only: bool) -> Result<serde_json::Value> {
    if config.graph_directory.as_os_str().is_empty()
        || config.output.as_os_str().is_empty()
        || !config.horizon_s.is_finite()
        || config.horizon_s <= 0.0
        || !(1..=16).contains(&config.samples_per_axis)
        || !(1..=1000).contains(&config.iterations_per_priority)
        || config.repair_sweeps > 10
        || !(1..=64).contains(&config.workers)
    {
        return Err("Invalid scheduling solver configuration".into());
    }
    if config.output.exists() {
        return Err("Solver output already exists; choose a new directory".into());
    }
    resources::set_worker_limit(config.workers);
    resources::background_thread();
    let started = Instant::now();
    let model = input::load(config)?;
    let prepared_s = started.elapsed().as_secs_f64();
    eprintln!(
        "Prepared {} satellite graphs, {} actions, {} coverage witnesses in {:.1}s",
        model.graphs.len(),
        model.graphs.iter().map(|g| g.actions.len()).sum::<usize>(),
        model.point_request.len(),
        prepared_s
    );
    let (candidates, history) = if baseline_only {
        (
            vec![Candidate {
                name: "independent_paths_baseline".into(),
                plan: search::independent_baseline(&model),
            }],
            Vec::new(),
        )
    } else {
        search::optimize(&model, config)?
    };
    let search_s = started.elapsed().as_secs_f64() - prepared_s;
    let (best, reports) = verify::candidates(&model, config, &candidates)?;
    let winner = &candidates[best];
    let schedule: Vec<_> = model
        .graphs
        .iter()
        .zip(&winner.plan)
        .map(|(g, p)| {
            serde_json::json!({"satellite":g.satellite,"captures":p.iter().map(|&a| {
            let action=g.actions[a as usize];
            serde_json::json!({"action":a,"tick":action.tick,"mode":action.mode,"start_s":action.start_s,"end_s":action.end_s})
        }).collect::<Vec<_>>()})
        })
        .collect();
    let report = serde_json::json!({
        "schema_version":2,"status":"complete","method":if baseline_only { "independent_satellite_paths_with_polygon_verification" } else { "lagrangian_acquisition_deadlines_with_polygon_verification" },
        "objective":if baseline_only { "Use the deterministic independent-path baseline: maximize normalized sampled coverage independently for each satellite, with all due deadlines weighted equally." } else { "Maximize met acquisition deadlines by descending job priority, then partial coverage; each cadence window is independent." },
        "semantics":{
            "opportunity":"An optional graph capture; not selecting it is not a failure.",
            "acquisition_request":"Required coverage for one job within one cadence window, with its own deadline.",
            "missed_deadline":"A due acquisition request whose coverage is incomplete in the selected plan. A planning result, not an observed execution or weather failure.",
            "job":"An ongoing workload that remains active after any missed acquisition deadline.",
            "future_deadline":"Not due in this planning horizon; excluded from missed-deadline counts.",
            "excluded_job":"Outside the configured job scope; not optimized or assessed as missed or pending in this run. Captures may incidentally cover excluded jobs."
        },
        "reference":"https://doi.org/10.1016/j.ejor.2025.12.047",
        "config":config,"graph_preset_hash_fnv1a64":model.manifest.preset_hash_fnv1a64,
        "problem_size":{
            "satellites":model.graphs.len(),
            "actions":model.graphs.iter().map(|g|g.actions.len()).sum::<usize>(),
            "solver_nodes":model.graphs.iter().map(|g|g.edges.len()).sum::<usize>(),
            "solver_arcs":model.graphs.iter().flat_map(|g|g.edges.iter()).map(Vec::len).sum::<usize>(),
            "coverage_witnesses":model.point_request.len()
        },
        "epoch_unix_s":model.manifest.config.epoch_unix_s,
        "selected_candidate":winner.name,"verification":reports[best],
        "candidate_comparison":reports,"iterations":history,
        "prepared_s":prepared_s,"search_s":search_s,"elapsed_s":started.elapsed().as_secs_f64(),
        "observed_process_memory_bytes":resources::process_memory(),
        "limitations":[
            if baseline_only { "This run explicitly selects the independent-path candidate; priority-stage candidates were not generated." } else { "Adaptation of the publicly described Lagrangian decomposition; the paper's full heuristic details were not available. Not a reproduction of LRD-H." },
            "Search uses spatial witnesses. Only polygon-verified completion counts are reported as actual completions. No optimality guarantee or original-problem optimality gap.",
            if baseline_only { "The baseline weights all due requests equally and does not coordinate shared coverage between satellites. It is not a proof of globally optimal deadline fulfilment. Missed requests do not drop ongoing jobs." } else { "Incumbents are compared lexicographically by met acquisition deadlines at descending priority, then partial coverage. Missed requests neither fail nor drop ongoing jobs. This is a heuristic, not a proof of optimal deadline fulfilment." },
            "Only cadence windows whose deadlines close within the chosen prefix are optimized. Pending weekly/monthly work has no reward in this experiment.",
            "Explicit next deadlines can represent ongoing jobs, but no imagery from before the planning epoch is credited. The first cycle may have only a partial collection window available in this run.",
            "Feasibility is for the precomputed finite acquisition and maneuver model; storage, downlink, energy, processing and clouds remain unmodeled."
        ]
    });
    // Publish only after verification; never overwrite graph files or earlier runs.
    if let Some(parent) = config.output.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::create_dir(&config.output).map_err(|e| e.to_string())?;
    fs::write(
        config.output.join("INCOMPLETE"),
        b"Report is complete only after report.json exists.\n",
    )
    .map_err(|e| e.to_string())?;
    write_json(
        config.output.join("schedule.json"),
        &serde_json::json!({"satellites":schedule}),
    )?;
    write_json(config.output.join("report.json"), &report)?;
    fs::remove_file(config.output.join("INCOMPLETE")).map_err(|e| e.to_string())?;
    Ok(report)
}
fn write_json(path: PathBuf, value: &serde_json::Value) -> Result<()> {
    let part = path.with_extension("json.part");
    fs::write(
        &part,
        serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    fs::rename(part, path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;
