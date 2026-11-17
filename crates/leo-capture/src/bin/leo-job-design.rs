//! Derive a new workload from saved AOI polygons and a constellation service policy.
use leo_capture::{
    admission::{self, Policy},
    scheduling, storage, workload,
};
use serde::Deserialize;
use std::{fs, io::Write, path::PathBuf};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    preset: PathBuf,
    output: PathBuf,
    #[serde(default)]
    policy: Policy,
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 2 {
        return Err("Usage: leo-job-design CONFIG.json".into());
    }
    let path = fs::canonicalize(&args[1])?;
    let mut config: Config = serde_json::from_reader(fs::File::open(&path)?)?;
    let base = path.parent().unwrap();
    config.preset = base.join(config.preset);
    config.output = base.join(config.output);
    if config.output.exists() {
        return Err("Output must be a new directory".into());
    }
    config.policy.validate()?;
    let (jobs, regions, checksum) = scheduling::load_preset(&config.preset)?;
    let by_key: std::collections::BTreeMap<_, _> =
        regions.iter().map(|r| (&r.job_key, r)).collect();
    let mut mosaic_keys: Vec<_> = jobs
        .iter()
        .filter(|j| j.acquisition.fulfilment == workload::Fulfilment::Mosaic)
        .map(|j| j.aoi.key())
        .collect();
    mosaic_keys.sort_by_key(|key| (workload::hash(key.as_bytes()), key.clone()));
    let similar_count =
        (mosaic_keys.len() as f64 * config.policy.similar_solar_time_fraction).round() as usize;
    let similar: std::collections::BTreeSet<_> =
        mosaic_keys.into_iter().take(similar_count).collect();
    let mut designed = Vec::with_capacity(jobs.len());
    for (i, job) in jobs.iter().enumerate() {
        let region = by_key.get(&job.aoi.key()).ok_or("Missing AOI geometry")?;
        let mut assigned_policy = config.policy.clone();
        assigned_policy.similar_solar_time_fraction = if similar.contains(&job.aoi.key()) {
            1.0
        } else {
            0.0
        };
        designed.push(admission::redesign(job, region, &assigned_policy)?);
        if (i + 1) % 5000 == 0 {
            eprintln!("Designed {}/{} jobs", i + 1, jobs.len());
        }
    }
    let input = fs::read(&config.preset)?;
    if workload::hash(&input) != checksum {
        return Err("Preset changed during design".into());
    }
    let mut envelope: serde_json::Value = if input.starts_with(&[0x1f, 0x8b]) {
        serde_json::from_reader(flate2::read::GzDecoder::new(input.as_slice()))?
    } else {
        serde_json::from_slice(&input)?
    };
    let mut doc: storage::Document = serde_json::from_value(envelope["workload"].clone())?;
    doc.schema_version = 6;
    doc.jobs = designed;
    doc.generator.acquisition.similar_solar_time_fraction =
        config.policy.similar_solar_time_fraction;
    doc.generator.acquisition.solar_tolerance_hours =
        config.policy.usable_window_hours_per_day / 2.0;
    doc.generator.bounds.cadence_max_hours = doc
        .generator
        .bounds
        .cadence_max_hours
        .max(doc.jobs.iter().map(|j| j.cadence_hours).fold(0.0, f64::max));
    doc.generator.bounds.span_max_hours = doc
        .generator
        .bounds
        .span_max_hours
        .max(doc.jobs.iter().map(|j| j.span_hours()).fold(0.0, f64::max));
    doc.validate()?;
    let mut cadences = std::collections::BTreeMap::<String, usize>::new();
    let mut modes = std::collections::BTreeMap::<String, usize>::new();
    let mut strips = std::collections::BTreeMap::<u32, usize>::new();
    for job in &doc.jobs {
        *cadences
            .entry(format!("{} days", job.cadence_hours / 24.0))
            .or_default() += 1;
        *modes
            .entry(
                if job.acquisition.fulfilment == workload::Fulfilment::Independent {
                    "Independent crops".into()
                } else {
                    job.acquisition.mosaic_timing.label().into()
                },
            )
            .or_default() += 1;
        *strips
            .entry(
                job.acquisition
                    .admission
                    .as_ref()
                    .unwrap()
                    .planned_strips_with_overlap,
            )
            .or_default() += 1;
    }
    let report = serde_json::json!({"schema_version":1,"status":"complete","source_preset_hash_fnv1a64":checksum,
        "policy":config.policy,"jobs":doc.jobs.len(),"jobs_with_changed_cadence":jobs.iter().zip(&doc.jobs).filter(|(a,b)|a.cadence_hours!=b.cadence_hours).count(),
        "by_cadence":cadences,"by_timing_mode":modes,"by_strip_count":strips,
        "jobs_with_staggered_phase":doc.jobs.iter().filter(|j|j.acquisition.phase_hours>0.0).count(),
        "minimum_cadence_days":doc.jobs.iter().map(|j|j.cadence_hours/24.0).fold(f64::INFINITY,f64::min),
        "maximum_cadence_days":doc.jobs.iter().map(|j|j.cadence_hours/24.0).fold(0.0,f64::max),
        "interpretation":["Provisional service design under a parallel-strip reservation policy, not a proven feasible schedule or a physical lower bound for arbitrary agile trajectories.",
            "Perpendicular widths use locally projected AOI polygons and ascending/descending directions from the WorldView-60 design orbit. Disconnected projection gaps do not consume strips. One consistent direction and one strip per pass are assumed for each mosaic.",
            "Reserved lanes are an admission assumption derived from access width / strip spacing, not a measured clique of competing jobs. Daily opportunities use the configured nominal access interval and usable window; actual illumination, seasons and conflicts need scheduling verification.",
            "Delivery periods round up to whole days. Similar-solar-time mosaics may collect over the delivery period. Close-in-time mosaics use ceil(strips / daily pass budget) whole days immediately before the delivery deadline. Independent crops have no coherence constraint.",
            "Priorities, AOIs, pipelines, fulfilment and output densities are preserved. History samples are adjusted to retain at least the original history duration, with at least two samples for window jobs. Delivery phases are deterministically staggered across each cycle by default to avoid synchronizing short collection windows. No original assets are overwritten."]});
    envelope["workload"] = serde_json::to_value(&doc)?;
    envelope["service_design"] =
        serde_json::json!({"policy":config.policy,"source_preset_hash_fnv1a64":checksum});
    fs::create_dir_all(config.output.parent().ok_or("Missing output parent")?)?;
    fs::create_dir(&config.output)?;
    fs::write(
        config.output.join("INCOMPLETE"),
        b"Design export incomplete\n",
    )?;
    let output = config.output.join("preset.json.gz");
    let mut gzip =
        flate2::write::GzEncoder::new(fs::File::create(&output)?, flate2::Compression::fast());
    serde_json::to_writer(&mut gzip, &envelope)?;
    gzip.finish()?.flush()?;
    let (roundtrip, _, _) = scheduling::load_preset(&output)?;
    if roundtrip != doc.jobs {
        return Err("Designed workload failed round-trip verification".into());
    }
    fs::write(
        config.output.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    fs::remove_file(config.output.join("INCOMPLETE"))?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    println!("Preset: {}", output.display());
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("Job design failed: {e}");
        std::process::exit(1);
    }
}
