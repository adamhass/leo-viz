//! Assemble a portable replay from a verified schedule and its source graph.
use super::io::{read_json, write_new_gzip};
use super::*;
use crate::{scheduling::Summary, workload::hash};
use std::path::Path;

/// Package the selected schedule, archived workload and exact candidate identity.
pub fn export(schedule: &Path, output: &Path) -> Result<()> {
    if output.exists() {
        return Err("Replay output already exists".into());
    }
    let root = schedule
        .parent()
        .ok_or("Schedule has no parent directory")?;
    let report = read_json(&root.join("report.json"))?;
    if report["status"] != "complete" || root.join("INCOMPLETE").exists() {
        return Err("Schedule run is incomplete".into());
    }
    let graph = Path::new(
        report["config"]["graph_directory"]
            .as_str()
            .ok_or("Missing graph directory")?,
    );
    let summary: Summary = serde_json::from_value(read_json(&graph.join("manifest.json"))?)
        .map_err(|e| e.to_string())?;
    if summary.status != "complete" || graph.join("INCOMPLETE").exists() {
        return Err("Graph run is incomplete".into());
    }
    let preset_path = graph.join(if graph.join("preset.json.gz").exists() {
        "preset.json.gz"
    } else {
        "preset.json"
    });
    let bytes = std::fs::read(&preset_path).map_err(|e| e.to_string())?;
    if hash(&bytes) != summary.preset_hash_fnv1a64
        || report["graph_preset_hash_fnv1a64"].as_u64() != Some(summary.preset_hash_fnv1a64)
    {
        return Err("Schedule/preset checksum mismatch".into());
    }
    let saved = read_json(schedule)?;
    let mut satellites: Vec<Satellite> =
        serde_json::from_value(saved["satellites"].clone()).map_err(|e| e.to_string())?;
    let expected_ids: BTreeSet<_> = summary.satellites.iter().map(|s| s.satellite).collect();
    if satellites
        .iter()
        .map(|s| s.satellite)
        .collect::<BTreeSet<_>>()
        != expected_ids
    {
        return Err("Schedule satellite set differs from graph".into());
    }
    let assessment = crate::scheduling::solver::replay::assess_selected(
        &summary,
        &report,
        &preset_path,
        &mut satellites,
    )?;
    let replay = Replay::new(Document {
        assessment: Some(assessment),
        schema_version: 1,
        epoch_unix_s: summary.config.epoch_unix_s,
        horizon_s: report["config"]["horizon_s"]
            .as_f64()
            .ok_or("Missing horizon")?,
        options: summary.config.options,
        constellation: summary.constellation,
        modes: summary.modes,
        preset: read_json(&preset_path)?,
        satellites,
        selected_candidate: report["selected_candidate"]
            .as_str()
            .unwrap_or("selected")
            .into(),
        source_preset_hash_fnv1a64: summary.preset_hash_fnv1a64,
    })?;
    write_new_gzip(output, &replay.document)
}
