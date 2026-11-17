//! Workload variant × reward model × optimizer comparisons.
//! Timing design is an upstream input: each variant supplies its own saved graph.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkloadVariant {
    pub id: String,
    /// Provenance label, not a command to alter jobs (e.g. randomized_deadlines).
    pub timing_policy: String,
    pub graph_directory: PathBuf,
    /// Seed coordinated runs from this earlier graph variant's matching result.
    #[serde(default)]
    pub incumbent_from: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Experiment {
    pub output: PathBuf,
    pub workloads: Vec<WorkloadVariant>,
    pub reward_models: BTreeMap<String, reward::RewardModel>,
    pub optimizers: Vec<Optimizer>,
    /// Graph path/output/optimizer/reward are supplied by the matrix axes.
    #[serde(default)]
    pub solver: Config,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn validate(experiment: &Experiment) -> Result<()> {
    if experiment.output.as_os_str().is_empty()
        || experiment.output.exists()
        || experiment.workloads.is_empty()
        || experiment.reward_models.is_empty()
        || experiment.optimizers.is_empty()
    {
        return Err(
            "Comparison needs workload/reward/optimizer axes and a new output directory".into(),
        );
    }
    let mut ids = BTreeSet::new();
    for variant in &experiment.workloads {
        if variant
            .incumbent_from
            .as_ref()
            .is_some_and(|id| !ids.contains(id))
        {
            return Err("Incumbent variant must precede its refinement".into());
        }
        if !valid_id(&variant.id)
            || !ids.insert(&variant.id)
            || variant.timing_policy.trim().is_empty()
            || variant.graph_directory.as_os_str().is_empty()
        {
            return Err("Invalid or duplicate workload variant".into());
        }
    }
    for (id, reward) in &experiment.reward_models {
        if !valid_id(id) {
            return Err("Invalid reward-model identifier".into());
        }
        reward.validate()?;
    }
    let mut optimizers = BTreeSet::new();
    for optimizer in &experiment.optimizers {
        if optimizer.legacy() || !optimizers.insert(optimizer.id()) {
            return Err(
                "Comparison optimizers must be distinct and use the common reward objective".into(),
            );
        }
    }
    Ok(())
}

pub fn run(experiment: &Experiment) -> Result<serde_json::Value> {
    validate(experiment)?;
    resources::set_worker_limit(experiment.solver.workers);
    resources::background_thread();
    let started = Instant::now();
    let mut rows = Vec::new();
    let mut workload_records = Vec::new();
    let mut common_geometry = None;
    fs::create_dir_all(
        experiment
            .output
            .parent()
            .ok_or("Missing comparison output parent")?,
    )
    .map_err(|e| e.to_string())?;
    fs::create_dir(&experiment.output).map_err(|e| e.to_string())?;
    fs::write(
        experiment.output.join("INCOMPLETE"),
        b"Comparison is not complete\n",
    )
    .map_err(|e| e.to_string())?;
    for variant in &experiment.workloads {
        let mut config = experiment.solver.clone();
        config.graph_directory = variant.graph_directory.clone();
        config.output = experiment.output.join(&variant.id);
        validate_config(&config)?;
        let preparing = Instant::now();
        let model = input::load(&config)?;
        let prepared_s = preparing.elapsed().as_secs_f64();
        let geometry_cache = (config.postprocess.is_some() || (config.acceptance == Acceptance::ExactPolygon
            && experiment
                .optimizers
                .iter()
                .any(|o| matches!(o, Optimizer::CoordinateImprovement | Optimizer::Lagrangian))))
        .then(|| exact::Geometry::load(&model, &config))
        .transpose()?;
        let geometry = serde_json::json!({"epoch":model.manifest.config.epoch_unix_s,"options":model.manifest.config.options,"constellation":model.manifest.constellation});
        if common_geometry
            .as_ref()
            .is_some_and(|original| original != &geometry)
        {
            return Err(
                "Workload variants must share the epoch, constellation and observation model"
                    .into(),
            );
        }
        common_geometry = Some(geometry);
        let mut priority_counts = BTreeMap::<u16, usize>::new();
        let mut reward_modes = BTreeMap::<String, usize>::new();
        for job in &model.jobs {
            *priority_counts.entry(job.acquisition.priority).or_default() += 1;
            *reward_modes
                .entry(
                    serde_json::to_value(job.acquisition.effective_reward_mode())
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .into(),
                )
                .or_default() += 1;
        }
        workload_records.push(serde_json::json!({
            "id":variant.id,"timing_policy":variant.timing_policy,"graph_directory":variant.graph_directory,
            "preset_hash_fnv1a64":model.manifest.preset_hash_fnv1a64,
            "manifest_hash_fnv1a64":crate::workload::hash(&fs::read(variant.graph_directory.join("manifest.json")).map_err(|e|e.to_string())?),
            "epoch_unix_s":model.manifest.config.epoch_unix_s,"horizon_s":config.horizon_s,
            "jobs":model.jobs.len(),"priorities":priority_counts,"job_reward_modes":reward_modes,
            "prepared_once_s":prepared_s,
            "polygon_cache_prepared_once_s":geometry_cache.as_ref().map_or(0.0, |g| g.prepared_s),
            "cached_polygons":geometry_cache.as_ref().map_or(0, |g| g.crop_count()),
            "candidate_builder":if model.manifest.config.strips.is_some() {"aoi_strips"} else {"fixed_grid"},
            "strip_options":model.manifest.config.strips,
            "graph_actions":model.manifest.total_actions,"graph_edges":model.manifest.total_edges,
            "graph_generation_s":model.manifest.elapsed_s,"incumbent_from":variant.incumbent_from
        }));
        for (reward_id, reward) in &experiment.reward_models {
            for &optimizer in &experiment.optimizers {
                config.reward = reward.clone();
                config.optimizer = optimizer;
                config.incumbent_schedule = if optimizer == Optimizer::IndependentPaths {
                    None
                } else {
                    variant.incumbent_from.as_ref().map(|id| {
                        experiment
                            .output
                            .join(id)
                            .join(reward_id)
                            .join(optimizer.id())
                            .join("schedule.json")
                    })
                };
                let relative = PathBuf::from(&variant.id)
                    .join(reward_id)
                    .join(optimizer.id());
                config.output = experiment.output.join(&relative);
                eprintln!(
                    "Comparing {} / {} / {}",
                    variant.id,
                    reward_id,
                    optimizer.id()
                );
                let report = run_loaded(
                    &config,
                    &model,
                    prepared_s,
                    geometry_cache.as_ref(),
                    &experiment.reward_models,
                    None,
                )?;
                let v = &report["verification"];
                rows.push(serde_json::json!({
                    "workload":variant.id,"timing_policy":variant.timing_policy,"optimized_for":reward_id,"optimizer":optimizer,
                    "selected_candidate":report["selected_candidate"],"schedule":relative.join("schedule.json"),"report":relative.join("report.json"),
                    "due_deadlines":v["due_acquisition_deadlines"],"met_deadlines":v["planned_met_deadlines"],
                    "met_percent":v["planned_met_rate_percent"],"missed_deadlines":v["planned_missed_deadlines"],
                    "missed_percent":v["planned_miss_rate_percent"],"capture_count":v["capture_count"],
                    "true_reward":v["true_reward"],"rewards_by_model":v["rewards_by_model"],
                    "unique_job_area_km2":v["unique_job_area_km2"],"repeated_job_area_km2":v["repeated_job_area_km2"],
                    "acceptance":report["acceptance"],
                    "postprocess":report["postprocess"],"postprocess_s":report["postprocess_s"],
                    "search_s":report["search_s"],"verification_s":report["verification_s"]
                }));
            }
        }
    }
    let report = serde_json::json!({
        "schema_version":1,"status":"complete","config":experiment,"workloads":workload_records,"results":rows,
        "elapsed_s":started.elapsed().as_secs_f64(),
        "semantics":{
            "reward":"Geometric area in km² × c^priority, with the job's unique-area or completed-observation policy.",
            "duplicates":"Polygon union per job/deadline across all satellites; repeat work earns no additional reward. Distinct jobs can share one useful capture.",
            "cross_evaluation":"Each selected plan is evaluated under every reward model, independently of which model generated it.",
            "timing_variants":"Supplied upstream as separate graphs; this workflow does not alter deadlines or implement alignment.",
            "comparability":"Optimizer comparisons within a workload/reward cell share the same graph and objective. When cadence/timing changes demand within the horizon, compare due counts and available reward too.",
            "cost":"Graph/witness preparation and the optional polygon cache are shared per workload and reported separately. Acceptance evaluation is included in search time. Each optimizer pays for its own search and final verification."
        }
    });
    write_json(experiment.output.join("comparison.json"), &report)?;
    fs::remove_file(experiment.output.join("INCOMPLETE")).map_err(|e| e.to_string())?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn variant_names_cannot_escape_the_output_and_legacy_objectives_are_excluded() {
        for id in ["", "..", "a/b", "/tmp/x"] {
            assert!(!valid_id(id));
        }
        assert!(valid_id("randomized-20261113"));
        let mut experiment = Experiment {
            output: PathBuf::from("not-created-comparison"),
            workloads: vec![WorkloadVariant {
                id: "randomized".into(),
                timing_policy: "randomized_deadlines".into(),
                graph_directory: PathBuf::from("graphs"),
                incumbent_from: None,
            }],
            reward_models: [("area".into(), reward::RewardModel::default())].into(),
            optimizers: vec![Optimizer::LegacyPriority],
            solver: Config::default(),
        };
        assert!(validate(&experiment).is_err());
        experiment.optimizers = vec![Optimizer::IndependentPaths, Optimizer::Lagrangian];
        assert!(validate(&experiment).is_ok());
        experiment.workloads.push(experiment.workloads[0].clone());
        assert!(validate(&experiment).is_err());
    }
}
