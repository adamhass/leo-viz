//! Standalone measurement/export of collection groups; no satellite DAGs needed.
use super::*;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub preset: PathBuf,
    pub output: PathBuf,
    #[serde(default)]
    pub context: Context,
    #[serde(default)]
    pub policy: Policy,
    #[serde(default = "cell")]
    pub cell_deg: f64,
    #[serde(default = "edge")]
    pub edge_km: f64,
    #[serde(default = "workers")]
    pub workers: usize,
}
fn cell() -> f64 {
    2.0
}
fn edge() -> f64 {
    2.0
}
fn workers() -> usize {
    crate::resources::worker_limit()
}

pub fn from_config(path: &Path) -> Result<serde_json::Value> {
    let path = fs::canonicalize(path).map_err(|e| e.to_string())?;
    let mut config: Config =
        serde_json::from_reader(fs::File::open(&path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let base = path.parent().unwrap();
    config.preset = base.join(config.preset);
    config.output = base.join(config.output);
    generate(&config)
}
pub fn generate(config: &Config) -> Result<serde_json::Value> {
    config.context.validate()?;
    config.policy.validate()?;
    if config.workers == 0 || config.output.exists() {
        return Err("Grouping requires positive workers and a new output directory".into());
    }
    crate::resources::set_worker_limit(config.workers);
    crate::resources::background_thread();
    let start = Instant::now();
    let (jobs, regions, checksum) = crate::scheduling::load_preset(&config.preset)?;
    let load_s = start.elapsed().as_secs_f64();
    let prepare = Instant::now();
    let atlas = Atlas::with_workers(
        &jobs,
        &regions,
        config.cell_deg,
        config.edge_km,
        config.workers,
        |_| true,
    )?;
    let geometry_s = prepare.elapsed().as_secs_f64();
    eprintln!(
        "Grouping {} jobs · {:.2}s geometry preparation",
        jobs.len(),
        geometry_s
    );
    let enriched = enrich(&jobs, &atlas, &config.context, &config.policy)?;
    let export_start = Instant::now();
    if let Some(parent) = config.output.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::create_dir(&config.output).map_err(|e| e.to_string())?;
    fs::write(
        config.output.join("INCOMPLETE"),
        b"Collection-group export incomplete\n",
    )
    .map_err(|e| e.to_string())?;
    let mut writer = flate2::write::GzEncoder::new(
        fs::File::create_new(config.output.join("groups.json.gz")).map_err(|e| e.to_string())?,
        flate2::Compression::fast(),
    );
    let charts: Vec<_> = atlas.patches.iter().map(|p| p.chart.center).collect();
    let keys: Vec<_> = enriched.jobs.iter().map(|j| j.aoi.key()).collect();
    serde_json::to_writer(
        &mut writer,
        &serde_json::json!({"schema_version":1,
        "source_preset_hash_fnv1a64":checksum,"context":config.context,"policy":config.policy,
        "original_job_keys":keys,"chart_centers_unit":charts,"groups":enriched.groups}),
    )
    .map_err(|e| e.to_string())?;
    writer
        .finish()
        .map_err(|e| e.to_string())?
        .flush()
        .map_err(|e| e.to_string())?;
    let report = serde_json::json!({"schema_version":1,"status":"complete","config":config,
        "source_preset_hash_fnv1a64":checksum,"statistics":enriched.report,
        "load_s":load_s,"geometry_s":geometry_s,"export_s":export_start.elapsed().as_secs_f64(),
        "total_s":start.elapsed().as_secs_f64(),
        "semantics":["Original jobs and rewards remain unchanged; group geometries are additive strip-generation inputs.",
            "Groups contain observation requests (job index, slot); common UTC windows are sampled at each original AOI anchor.",
            "Size is a nominal WorldView-60 pass/strip estimate, not a fleet capacity reservation or proof of schedulability.",
            "Short windows permit one possible access if capture plus maneuver fits; the graph must find an actual pass."]});
    fs::write(
        config.output.join("report.json"),
        serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    fs::remove_file(config.output.join("INCOMPLETE")).map_err(|e| e.to_string())?;
    Ok(report)
}
