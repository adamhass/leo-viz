//! Export a new combined preset; never edit the source workload or saved runs.
use super::*;
use crate::{scheduling, storage};
use std::{
    fs,
    io::{BufReader, Write},
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub preset: PathBuf,
    pub output: PathBuf,
    #[serde(default)]
    pub policy: Policy,
}
pub fn from_config(path: &Path) -> Result<serde_json::Value> {
    let path = fs::canonicalize(path).map_err(|e| e.to_string())?;
    let mut config: Config = serde_json::from_reader(BufReader::new(
        fs::File::open(&path).map_err(|e| e.to_string())?,
    ))
    .map_err(|e| e.to_string())?;
    let base = path.parent().unwrap();
    config.preset = base.join(config.preset);
    config.output = base.join(config.output);
    generate(&config)
}
fn gzip(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut writer = flate2::write::GzEncoder::new(
        fs::File::create_new(path).map_err(|e| e.to_string())?,
        flate2::Compression::fast(),
    );
    serde_json::to_writer(&mut writer, value).map_err(|e| e.to_string())?;
    writer
        .finish()
        .map_err(|e| e.to_string())?
        .flush()
        .map_err(|e| e.to_string())
}
pub fn generate(config: &Config) -> Result<serde_json::Value> {
    config.policy.validate()?;
    if config.output.exists() {
        return Err("Alignment output must be a new directory".into());
    }
    let (jobs, regions, checksum) = scheduling::load_preset(&config.preset)?;
    let result = align(&jobs, &regions, &config.policy)?;
    verify_permutation(&jobs, &result.jobs)?;
    let input = fs::read(&config.preset).map_err(|e| e.to_string())?;
    if hash(&input) != checksum {
        return Err("Source preset changed during alignment".into());
    }
    let mut envelope: serde_json::Value = if input.starts_with(&[0x1f, 0x8b]) {
        serde_json::from_reader(flate2::read::GzDecoder::new(input.as_slice()))
    } else {
        serde_json::from_slice(&input)
    }
    .map_err(|e| e.to_string())?;
    let mut doc: storage::Document =
        serde_json::from_value(envelope["workload"].clone()).map_err(|e| e.to_string())?;
    doc.schema_version = 6;
    doc.jobs = result.jobs;
    doc.validate()?;
    let previous_assignment = envelope.get("deadline_assignment").cloned();
    envelope["workload"] = serde_json::to_value(&doc).map_err(|e| e.to_string())?;
    envelope["cadence_alignment"] = serde_json::json!({"algorithm":"cadence_deadline_permutation_v1",
        "policy":config.policy,"source_preset_hash_fnv1a64":checksum,"source_deadline_assignment":previous_assignment});
    envelope["deadline_assignment"] = envelope["cadence_alignment"].clone();
    fs::create_dir_all(
        config
            .output
            .parent()
            .ok_or("Missing alignment output parent")?,
    )
    .map_err(|e| e.to_string())?;
    fs::create_dir(&config.output).map_err(|e| e.to_string())?;
    fs::write(
        config.output.join("INCOMPLETE"),
        b"Cadence alignment export incomplete\n",
    )
    .map_err(|e| e.to_string())?;
    let preset = config.output.join("preset.json.gz");
    gzip(&preset, &envelope)?;
    gzip(
        &config.output.join("assignments.json.gz"),
        &result.assignments,
    )?;
    let (roundtrip, roundtrip_regions, aligned_checksum) = scheduling::load_preset(&preset)?;
    if roundtrip != doc.jobs
        || serde_json::to_value(&roundtrip_regions).unwrap()
            != serde_json::to_value(&regions).unwrap()
    {
        return Err("Aligned preset failed job/geometry round-trip verification".into());
    }
    verify_permutation(&jobs, &roundtrip)?;
    let mut report = result.report;
    report["schema_version"] = 1.into();
    report["status"] = "complete".into();
    report["source_preset_hash_fnv1a64"] = checksum.into();
    report["aligned_preset_hash_fnv1a64"] = aligned_checksum.into();
    report["config"] = serde_json::to_value(config).map_err(|e| e.to_string())?;
    fs::write(
        config.output.join("report.json"),
        serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    fs::remove_file(config.output.join("INCOMPLETE")).map_err(|e| e.to_string())?;
    Ok(report)
}

pub(super) fn verify_permutation(before: &[Job], after: &[Job]) -> Result<()> {
    if before.len() != after.len() {
        return Err("Alignment changed the job count".into());
    }
    let mut original = BTreeMap::<u64, Vec<u64>>::new();
    let mut updated = BTreeMap::<u64, Vec<u64>>::new();
    for (a, b) in before.iter().zip(after) {
        let mut expected = a.clone();
        expected.acquisition.first_deadline_hours = b.acquisition.first_deadline_hours;
        if expected != *b {
            return Err("Alignment changed a job field other than its next deadline".into());
        }
        original
            .entry(a.cadence_hours.to_bits())
            .or_default()
            .push(deadline(a).to_bits());
        updated
            .entry(b.cadence_hours.to_bits())
            .or_default()
            .push(deadline(b).to_bits());
    }
    for values in original.values_mut().chain(updated.values_mut()) {
        values.sort_unstable();
    }
    if original != updated {
        return Err("Alignment changed the per-cadence deadline multiset".into());
    }
    Ok(())
}
