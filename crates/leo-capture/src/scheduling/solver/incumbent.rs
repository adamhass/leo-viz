//! Transfer a feasible plan across candidate refinement by physical identity.
//! Action numbers and satellite-local mode numbers may change on insertion.
use super::*;
use std::collections::BTreeMap;
use std::io::BufReader;

pub(super) fn load(model: &Model, config: &Config) -> Result<Option<Plan>> {
    let Some(path) = &config.incumbent_schedule else {
        return Ok(None);
    };
    let read = |path: &std::path::Path| -> Result<serde_json::Value> {
        serde_json::from_reader(BufReader::new(
            std::fs::File::open(path).map_err(|e| e.to_string())?,
        ))
        .map_err(|e| e.to_string())
    };
    let root = path.parent().ok_or("Incumbent has no parent directory")?;
    let report = read(&root.join("report.json"))?;
    if report["status"] != "complete" || root.join("INCOMPLETE").exists() {
        return Err("Incomplete incumbent schedule".into());
    }
    let previous_config: Config =
        serde_json::from_value(report["config"].clone()).map_err(|e| e.to_string())?;
    let previous: super::super::Summary = serde_json::from_value(read(
        &previous_config.graph_directory.join("manifest.json"),
    )?)
    .map_err(|e| e.to_string())?;
    if previous.preset_hash_fnv1a64 != model.manifest.preset_hash_fnv1a64
        || previous_config.horizon_s != config.horizon_s
        || previous.config.epoch_unix_s != model.manifest.config.epoch_unix_s
        || serde_json::to_value(&previous.config.options).unwrap()
            != serde_json::to_value(&model.manifest.config.options).unwrap()
        || serde_json::to_value(&previous.constellation).unwrap()
            != serde_json::to_value(&model.manifest.constellation).unwrap()
    {
        return Err(
            "Incumbent must share the saved jobs, epoch, horizon and observation model".into(),
        );
    }
    let value = read(path)?;
    let satellites = value["satellites"]
        .as_array()
        .ok_or("Invalid incumbent satellites")?;
    if satellites.len() != model.graphs.len() {
        return Err("Incumbent satellite count differs".into());
    }
    let key = |start: f64, end: f64, mode: &super::super::Mode| {
        (
            start.to_bits(),
            end.to_bits(),
            mode.along_km.to_bits(),
            mode.cross_km.to_bits(),
        )
    };
    let mut plan = Vec::new();
    for graph in &model.graphs {
        let old = satellites
            .iter()
            .find(|s| s["satellite"].as_u64() == Some(graph.satellite as u64))
            .ok_or("Missing incumbent satellite")?;
        let modes = model.manifest.modes_for(graph.satellite);
        let index: BTreeMap<_, _> = graph
            .actions
            .iter()
            .enumerate()
            .map(|(i, a)| {
                Ok((
                    key(
                        a.start_s,
                        a.end_s,
                        modes.get(a.mode).ok_or("Invalid graph mode")?,
                    ),
                    i as u32,
                ))
            })
            .collect::<Result<_>>()?;
        let old_modes = previous.modes_for(graph.satellite);
        let mut selected = Vec::new();
        for a in old["captures"]
            .as_array()
            .ok_or("Invalid incumbent captures")?
        {
            let start = a["start_s"].as_f64().ok_or("Missing incumbent start")?;
            let end = a["end_s"].as_f64().ok_or("Missing incumbent end")?;
            let mode = old_modes
                .get(a["mode"].as_u64().ok_or("Missing incumbent mode")? as usize)
                .ok_or("Invalid incumbent mode")?;
            selected.push(
                *index
                    .get(&key(start, end, mode))
                    .ok_or("Refined graph lost an incumbent capture")?,
            );
        }
        verify::validate_path(graph, &selected)?;
        verify::validate_maneuvers(model, graph, &selected)?;
        plan.push(selected);
    }
    eprintln!("Retaining feasible incumbent from {}", path.display());
    Ok(Some(plan))
}
