//! Experimental Lagrangian coverage decomposition. See SCHEDULING-SOLVER.md.
mod coverage_path;
mod diagnose;
mod exact;
pub mod experiment;
mod incumbent;
mod input;
pub mod postprocess;
pub(super) mod replay;
pub mod reward;
mod search;
mod verify;
mod weighted;

pub use diagnose::diagnose;

use crate::{resources, workload::Job};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf, time::Instant};

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Optimizer {
    IndependentPaths,
    CoordinateImprovement,
    Lagrangian,
    /// Historical behavior for reproducing archived priority experiments only.
    LegacyPriority,
    LegacyIndependent,
}
impl Optimizer {
    pub fn id(self) -> &'static str {
        match self {
            Self::IndependentPaths => "independent_paths",
            Self::CoordinateImprovement => "coordinate_improvement",
            Self::Lagrangian => "lagrangian",
            Self::LegacyPriority => "legacy_priority",
            Self::LegacyIndependent => "legacy_independent",
        }
    }
    fn legacy(self) -> bool {
        matches!(self, Self::LegacyPriority | Self::LegacyIndependent)
    }
}

/// Controls acceptance only; all proposals still use sampled coverage prices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Acceptance {
    #[default]
    ExactPolygon,
    /// Reproduce the earlier heuristic for controlled cost/quality comparisons.
    Sampled,
}

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
    /// Iteration budget for the common-reward Lagrangian optimizer.
    pub iterations: usize,
    pub optimizer: Optimizer,
    pub acceptance: Acceptance,
    pub reward: reward::RewardModel,
    pub incumbent_schedule: Option<PathBuf>,
    pub samples_per_axis: usize,
    pub repair_sweeps: usize,
    /// Coverage histories retained per graph node during replacement search.
    /// Bounded heuristic: each history credits a witness only once along its path.
    pub replacement_path_labels: usize,
    /// Optional optimizer-independent polygon pruning and missed-request refill.
    pub postprocess: Option<postprocess::Options>,
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
            iterations: 30,
            optimizer: Optimizer::Lagrangian,
            acceptance: Acceptance::ExactPolygon,
            reward: Default::default(),
            incumbent_schedule: None,
            samples_per_axis: 3,
            repair_sweeps: 1,
            replacement_path_labels: 4,
            postprocess: None,
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
    /// Area represented by each witness, repeated independently for each deadline.
    point_areas: Vec<f64>,
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
    validate_config(config)?;
    resources::set_worker_limit(config.workers);
    resources::background_thread();
    let started = Instant::now();
    let model = input::load(config)?;
    let prepared_s = started.elapsed().as_secs_f64();
    let geometry = needs_exact_geometry(config)
        .then(|| exact::Geometry::load(&model, config))
        .transpose()?;
    run_loaded(
        config,
        &model,
        prepared_s,
        geometry.as_ref(),
        &Default::default(),
        None,
    )
}

/// Apply postprocessing to a saved schedule without rerunning its optimizer.
pub fn postprocess_saved(config: &Config, schedule: &std::path::Path) -> Result<serde_json::Value> {
    let mut config = config.clone();
    if config.postprocess.is_none() {
        return Err("Saved-schedule postprocessing requires config.postprocess".into());
    }
    config.incumbent_schedule = Some(schedule.to_path_buf());
    validate_config(&config)?;
    resources::set_worker_limit(config.workers);
    resources::background_thread();
    let started = Instant::now();
    let model = input::load(&config)?;
    let plan = incumbent::load(&model, &config)?.ok_or("Missing input schedule")?;
    let prepared_s = started.elapsed().as_secs_f64();
    let geometry = exact::Geometry::load(&model, &config)?;
    run_loaded(
        &config,
        &model,
        prepared_s,
        Some(&geometry),
        &Default::default(),
        Some(plan),
    )
}

/// Run independent satellite paths under the same reward model as other solvers.
pub fn solve_baseline(config: &Config) -> Result<serde_json::Value> {
    let mut config = config.clone();
    config.optimizer = if config.optimizer.legacy() {
        Optimizer::LegacyIndependent
    } else {
        Optimizer::IndependentPaths
    };
    solve(&config)
}

fn validate_config(config: &Config) -> Result<()> {
    if config.graph_directory.as_os_str().is_empty()
        || config.output.as_os_str().is_empty()
        || !config.horizon_s.is_finite()
        || config.horizon_s <= 0.0
        || !(1..=16).contains(&config.samples_per_axis)
        || !(1..=1000).contains(&config.iterations_per_priority)
        || !(1..=1000).contains(&config.iterations)
        || config.repair_sweeps > 10
        || !(1..=16).contains(&config.replacement_path_labels)
        || !(1..=64).contains(&config.workers)
    {
        return Err("Invalid scheduling solver configuration".into());
    }
    if config.output.exists() {
        return Err("Solver output already exists; choose a new directory".into());
    }
    config.reward.validate()?;
    if let Some(options) = &config.postprocess {
        options.validate()?;
    }
    Ok(())
}

fn needs_exact_geometry(config: &Config) -> bool {
    config.postprocess.is_some() || uses_exact_acceptance(config)
}

fn uses_exact_acceptance(config: &Config) -> bool {
    config.acceptance == Acceptance::ExactPolygon
        && matches!(
            config.optimizer,
            Optimizer::CoordinateImprovement | Optimizer::Lagrangian
        )
}

fn run_loaded(
    config: &Config,
    model: &Model,
    prepared_s: f64,
    geometry: Option<&exact::Geometry>,
    evaluation_models: &std::collections::BTreeMap<String, reward::RewardModel>,
    input_plan: Option<Plan>,
) -> Result<serde_json::Value> {
    validate_config(config)?;
    let started = Instant::now();
    eprintln!(
        "Prepared {} satellite graphs, {} actions, {} coverage witnesses in {:.1}s",
        model.graphs.len(),
        model.graphs.iter().map(|g| g.actions.len()).sum::<usize>(),
        model.point_request.len(),
        prepared_s
    );
    let saved_input = input_plan.is_some();
    let (mut candidates, history, acceptance_report) = if let Some(plan) = input_plan {
        (
            vec![Candidate {
                name: "input_schedule".into(),
                plan,
            }],
            serde_json::json!([]),
            serde_json::Value::Null,
        )
    } else {
        match config.optimizer {
            Optimizer::LegacyIndependent => (
                vec![Candidate {
                    name: "independent_paths_baseline".into(),
                    plan: search::independent_baseline(model),
                }],
                serde_json::json!([]),
                serde_json::Value::Null,
            ),
            Optimizer::LegacyPriority => {
                let (candidates, history) = search::optimize(model, config)?;
                (
                    candidates,
                    serde_json::json!(history),
                    serde_json::Value::Null,
                )
            }
            _ => weighted::optimize(model, config, geometry)?,
        }
    };
    let search_s = started.elapsed().as_secs_f64();
    let verification_started = Instant::now();
    let (mut best, mut reports) =
        verify::candidates_with_rewards(model, config, &candidates, evaluation_models)?;
    let mut verification_s = verification_started.elapsed().as_secs_f64();
    if uses_exact_acceptance(config) && !saved_input {
        let accepted = acceptance_report["incumbent_reward"]
            .as_f64()
            .ok_or("Missing polygon acceptance reward")?;
        let verified = reports
            .last()
            .and_then(|r| r["true_reward"].as_f64())
            .ok_or("Missing verified reward")?;
        if (accepted - verified).abs() > 1e-8 * accepted.abs().max(1.0) {
            return Err(format!(
                "Cached polygon acceptance ({accepted}) disagrees with independent verification ({verified})"
            ));
        }
    }

    let mut postprocess_report = serde_json::Value::Null;
    let mut postprocess_s = 0.0;
    if let Some(options) = &config.postprocess {
        let (plan, mut details) = postprocess::run(
            model,
            geometry.ok_or("Postprocessing requires cached polygons")?,
            &config.reward,
            &candidates[best].plan,
            options,
        )?;
        postprocess_s = details["elapsed_s"].as_f64().unwrap();
        let candidate = Candidate {
            name: format!("{}_postprocessed", candidates[best].name),
            plan,
        };
        let started_verification = Instant::now();
        let (_, mut after) = verify::candidates_with_rewards(
            model,
            config,
            std::slice::from_ref(&candidate),
            evaluation_models,
        )?;
        verification_s += started_verification.elapsed().as_secs_f64();
        let after = after.remove(0);
        let cached = details["cached_true_reward"].as_f64().unwrap();
        let verified = after["true_reward"].as_f64().unwrap();
        if (cached - verified).abs() > 1e-8 * verified.abs().max(1.0) {
            return Err(
                "Postprocessing polygon reward disagrees with independent verification".into(),
            );
        }
        let before = &reports[best];
        let baseline_reward = before["true_reward"].as_f64().unwrap();
        let accepted = after["true_reward"].as_f64().unwrap()
            + 1e-8 * baseline_reward.abs().max(1.0)
            >= baseline_reward
            && after["planned_met_deadlines"].as_u64() >= before["planned_met_deadlines"].as_u64();
        let metrics = |value: &serde_json::Value| {
            let fields = [
                "true_reward",
                "due_acquisition_deadlines",
                "planned_met_deadlines",
                "planned_met_rate_percent",
                "planned_missed_deadlines",
                "capture_count",
                "unique_job_area_km2",
                "repeated_job_area_km2",
            ];
            serde_json::Value::Object(
                fields
                    .into_iter()
                    .map(|k| (k.to_string(), value[k].clone()))
                    .collect(),
            )
        };
        details["before"] = metrics(before);
        details["after"] = metrics(&after);
        details["accepted"] = accepted.into();
        if accepted {
            best = candidates.len();
        }
        candidates.push(candidate);
        reports.push(after);
        postprocess_report = details;
    }

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
        "schema_version":3,"status":"complete","method":if saved_input {"postprocess_saved_schedule"} else {config.optimizer.id()},
        "objective":if config.optimizer.legacy() { "Historical objective; not a common-reward optimizer comparison." } else { "Maximize area_km2 * priority_base^priority with each job's partial-credit rule; deduplicate coverage per job and acquisition deadline." },
        "reward_model":config.reward,
        "evaluation_reward_models":evaluation_models,
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
        "acceptance":acceptance_report,
        "postprocess":postprocess_report,"postprocess_s":postprocess_s,
        "polygon_cache_prepared_s":geometry.map_or(0.0, |g| g.prepared_s),
        "prepared_s":prepared_s,"search_s":search_s,"verification_s":verification_s,"elapsed_s":prepared_s+geometry.map_or(0.0, |g| g.prepared_s)+started.elapsed().as_secs_f64(),
        "observed_process_memory_bytes":resources::process_memory(),
        "limitations":[
            "Search uses area-weighted spatial witnesses. Published true_reward uses polygon-union coverage, independently of additive rewards and Lagrangian prices.",
            "Each due job/deadline is a separate obligation. Repeat coverage within it earns nothing extra; one physical acquisition may legitimately serve different jobs.",
            "Only deadlines closing within the horizon are scored. Pending deadlines and unused opportunities are not failures.",
            "No pre-epoch imagery is credited. Timing feasibility follows the finite graph; clouds, downlink, storage, processing and energy are not modeled.",
            "Sampled dual bounds apply only to the witness approximation, not to polygon-verified reward. There is no original-problem optimality guarantee."
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
