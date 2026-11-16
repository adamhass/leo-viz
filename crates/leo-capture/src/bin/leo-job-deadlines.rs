//! Assign randomized next deadlines while preserving the saved workload design.
use leo_capture::{deadlines::Policy, scheduling, storage, workload};
use serde::Deserialize;
use std::{collections::BTreeMap, fs, io::Write, path::PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    preset: PathBuf,
    output: PathBuf,
    policy: Policy,
    report_horizon_hours: f64,
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 2 {
        return Err("Usage: leo-job-deadlines CONFIG.json".into());
    }
    let path = fs::canonicalize(&args[1])?;
    let mut config: Config = serde_json::from_reader(fs::File::open(&path)?)?;
    let base = path.parent().unwrap();
    config.preset = base.join(config.preset);
    config.output = base.join(config.output);
    if config.output.exists() {
        return Err("Output must be a new directory".into());
    }
    if !config.report_horizon_hours.is_finite() || config.report_horizon_hours <= 0.0 {
        return Err("Invalid report horizon".into());
    }
    let (mut jobs, _, checksum) = scheduling::load_preset(&config.preset)?;
    config.policy.assign(&mut jobs)?;
    let input = fs::read(&config.preset)?;
    if workload::hash(&input) != checksum {
        return Err("Preset changed during deadline assignment".into());
    }
    let mut envelope: serde_json::Value = if input.starts_with(&[0x1f, 0x8b]) {
        serde_json::from_reader(flate2::read::GzDecoder::new(input.as_slice()))?
    } else {
        serde_json::from_slice(&input)?
    };
    let mut doc: storage::Document = serde_json::from_value(envelope["workload"].clone())?;
    doc.schema_version = 5;
    doc.jobs = jobs;
    doc.validate()?;
    let mut by_priority = BTreeMap::<u16, [usize; 2]>::new();
    let mut by_mode = BTreeMap::<String, [usize; 2]>::new();
    let mut by_cadence = BTreeMap::<String, [usize; 2]>::new();
    let mut normalized_bins = [0_usize; 10];
    let mut due_jobs = 0;
    let mut due_deadlines = 0;
    let mut min = f64::INFINITY;
    let mut max: f64 = 0.0;
    for j in &doc.jobs {
        let d = j.acquisition.first_deadline_hours.unwrap();
        let (count, _) = j
            .acquisition
            .deadline_counts(j.cadence_hours, config.report_horizon_hours * 3600.0);
        due_jobs += usize::from(count > 0);
        due_deadlines += count;
        min = min.min(d);
        max = max.max(d);
        normalized_bins[(((d - config.policy.minimum_deadline_hours)
            / (j.cadence_hours - config.policy.minimum_deadline_hours)
            * 10.0)
            .floor() as usize)
            .min(9)] += 1;
        let mode = if j.acquisition.fulfilment == workload::Fulfilment::Independent {
            "Independent crops"
        } else {
            j.acquisition.mosaic_timing.label()
        };
        for row in [
            by_priority.entry(j.acquisition.priority).or_default(),
            by_mode.entry(mode.to_owned()).or_default(),
            by_cadence
                .entry(format!("{} h", j.cadence_hours))
                .or_default(),
        ] {
            row[0] += 1;
            row[1] += usize::from(count > 0);
        }
    }
    let report = serde_json::json!({"schema_version":1,"status":"complete","source_preset_hash_fnv1a64":checksum,
        "policy":config.policy,"jobs":doc.jobs.len(),"report_horizon_hours":config.report_horizon_hours,
        "jobs_with_due_deadlines":due_jobs,"due_acquisition_deadlines":due_deadlines,"jobs_without_due_deadline":doc.jobs.len()-due_jobs,
        "minimum_next_deadline_hours":min,"maximum_next_deadline_hours":max,
        "group_columns":["jobs","jobs_with_due_deadlines"],"by_priority":by_priority,"by_timing_mode":by_mode,"by_cadence":by_cadence,
        "normalized_deadline_decile_counts":normalized_bins,
        "interpretation":["Per-job independent deterministic pseudorandom uniform draw on [minimum deadline, cadence). This is the stationary residual-deadline distribution conditioned on the minimum buffer, not a clamp or a cadence extension.",
            "Slot zero ends at the explicit next deadline; subsequent deadlines are exactly one cadence apart. Its collection window can start before planning time zero, but no prior imagery is credited by this experiment.",
            "The minimum deadline is an elapsed-time buffer, not a guarantee of two usable daylight or solar-window opportunities. All original AOIs, priorities, cadences, collection spans and job requirements are preserved."]});
    envelope["workload"] = serde_json::to_value(&doc)?;
    envelope["deadline_assignment"] = serde_json::json!({"policy":config.policy,"source_preset_hash_fnv1a64":checksum,"algorithm":"fnv1a64-key-xor-seed-splitmix64-top53-uniform-v1"});
    fs::create_dir_all(config.output.parent().ok_or("Missing output parent")?)?;
    fs::create_dir(&config.output)?;
    fs::write(
        config.output.join("INCOMPLETE"),
        b"Deadline export incomplete\n",
    )?;
    let output = config.output.join("preset.json.gz");
    let mut gzip =
        flate2::write::GzEncoder::new(fs::File::create(&output)?, flate2::Compression::fast());
    serde_json::to_writer(&mut gzip, &envelope)?;
    gzip.finish()?.flush()?;
    let (roundtrip, _, _) = scheduling::load_preset(&output)?;
    if roundtrip != doc.jobs {
        return Err("Assigned deadlines failed round-trip verification".into());
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
        eprintln!("Deadline assignment failed: {e}");
        std::process::exit(1);
    }
}
