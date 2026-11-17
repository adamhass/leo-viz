//! Reconstruct per-deadline results from the selected archived crop polygons.
//! This does not run the scheduler or load the graph's millions of edges.
use super::{input, verify};
use crate::scheduling::{
    playback::{Assessment, Crop, DeadlineOutcome, Satellite},
    Summary,
};
use geo::BooleanOps;
use std::{collections::BTreeMap, io::BufRead, path::Path};

#[derive(Default)]
struct Completion {
    coverage: verify::Request,
    completed_at_s: Option<f64>,
}

/// A satellite's file order is not acquisition order for the constellation.
/// Union completed crop events globally in time order, retaining the first
/// completion per observation rather than the last (possibly redundant) crop.
fn accumulate(
    states: &mut [Vec<Completion>],
    mut crops: Vec<input::Crop>,
    originals: &BTreeMap<(usize, usize), &crate::geometry::Shape>,
    areas: &[f64],
) -> Result<(), String> {
    crops.sort_by(|a, b| a.end_s.total_cmp(&b.end_s));
    for crop in crops {
        let state = states
            .get_mut(crop.job)
            .and_then(|slots| slots.get_mut(crop.slot as usize))
            .ok_or("Unknown replay observation")?;
        if state.coverage.complete {
            continue;
        }
        let original = originals
            .get(&(crop.job, crop.patch))
            .ok_or("Unknown crop chart")?;
        let shape = input::shape(crop.polygons_xy_km);
        let clipped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            shape.intersection(*original)
        }))
        .map_err(|_| "Replay crop clipping failed")?;
        state.coverage.add(crop.patch, &clipped, areas[crop.job])?;
        if state.coverage.complete {
            state.completed_at_s = Some(crop.end_s);
        }
    }
    Ok(())
}

pub(in crate::scheduling) fn assess_selected(
    summary: &Summary,
    report: &serde_json::Value,
    preset: &Path,
    satellites: &mut [Satellite],
) -> Result<Assessment, String> {
    let config: super::Config =
        serde_json::from_value(report["config"].clone()).map_err(|e| e.to_string())?;
    let (jobs, regions, _) = crate::scheduling::load_preset(preset)?;
    let included = input::job_scope(&jobs, config.job_cadence_hours)?;
    eprintln!("Rebuilding charts for per-deadline replay results");
    let atlas = crate::geometry::Atlas::new(
        &jobs,
        &regions,
        summary.config.options.cell_deg,
        summary.config.options.edge_km,
        |_| true,
    )?;
    drop(regions);
    let charts: Vec<serde_json::Value> = serde_json::from_reader(
        std::fs::File::open(config.graph_directory.join("charts.json"))
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if charts.len() != atlas.patches.len()
        || charts
            .iter()
            .zip(&atlas.patches)
            .enumerate()
            .any(|(id, (c, p))| {
                c["patch"].as_u64() != Some(id as u64)
                    || (0..3).any(|i| {
                        c["center_unit"][i]
                            .as_f64()
                            .is_none_or(|x| (x - p.chart.center[i]).abs() > 1e-12)
                    })
            })
    {
        return Err("Replay chart identity differs from archived graph".into());
    }
    let originals: BTreeMap<_, _> = atlas
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
    let mut states: Vec<Vec<Completion>> = jobs
        .iter()
        .zip(included)
        .map(|(j, included)| {
            (0..if included {
                j.acquisition
                    .deadline_counts(j.cadence_hours, config.horizon_s)
                    .0
            } else {
                0
            })
                .map(|_| Completion::default())
                .collect()
        })
        .collect();
    // Each satellite archive is independent. Only selected crop polygons survive parsing.
    let records = crate::parallel::map(
        satellites.iter().collect(),
        crate::parallel::worker_count(satellites.len()),
        |sat| {
            let mut selected: BTreeMap<_, _> = sat.captures.iter().map(|c| (c.action, c)).collect();
            let mut out = Vec::new();
            for line in input::reader(
                &config
                    .graph_directory
                    .join(format!("satellite-{:03}.actions.jsonl.gz", sat.satellite)),
            )?
            .lines()
            {
                let line = line.map_err(|e| e.to_string())?;
                #[derive(serde::Deserialize)]
                struct Id {
                    id: u32,
                }
                let id: Id = serde_json::from_str(&line).map_err(|e| e.to_string())?;
                if let Some(c) = selected.remove(&id.id) {
                    let record: input::ActionRecord =
                        serde_json::from_str(&line).map_err(|e| e.to_string())?;
                    let action = record.action(summary.config.options.step_s);
                    if action.tick != c.tick
                        || action.mode != c.mode
                        || action.start_s != c.start_s
                        || action.end_s != c.end_s
                    {
                        return Err("Selected capture differs from archived graph".into());
                    }
                    out.push(record);
                }
            }
            if !selected.is_empty() {
                return Err("Selected capture is absent from archived graph".into());
            }
            Ok(out)
        },
        |done, total| eprintln!("Replay capture archives: {done}/{total}"),
    )?;
    let mut crops = Vec::new();
    for (sat, records) in satellites.iter_mut().zip(records) {
        let mut by_id: BTreeMap<_, _> = records.into_iter().map(|r| (r.id, r)).collect();
        for capture in &mut sat.captures {
            let record = by_id
                .remove(&capture.action)
                .ok_or("Missing selected action")?;
            let action = record.action(summary.config.options.step_s);
            capture.crops.clear();
            for c in record.crops {
                input::validate_crop(&c, &action, &jobs)?;
                capture.crops.push(Crop {
                    job: c.job,
                    slot: c.slot,
                    start_s: c.start_s,
                    end_s: c.end_s,
                });
                if states[c.job].get(c.slot as usize).is_some() {
                    crops.push(c);
                }
            }
        }
    }
    accumulate(&mut states, crops, &originals, &atlas.job_areas_km2)?;
    let requests: Vec<_> = states
        .iter()
        .enumerate()
        .flat_map(|(job, slots)| {
            slots.iter().enumerate().map({
                let jobs = &jobs;
                let areas = &atlas.job_areas_km2;
                move |(slot, r)| DeadlineOutcome {
                    job,
                    slot: slot as u64,
                    deadline_s: jobs[job]
                        .acquisition
                        .deadline_s(slot as u64, jobs[job].cadence_hours),
                    coverage_fraction: (r.coverage.area / areas[job]).clamp(0.0, 1.0),
                    complete: r.coverage.complete,
                    completed_at_s: r.completed_at_s,
                }
            })
        })
        .collect();
    let met = requests.iter().filter(|r| r.complete).count();
    let partial = requests
        .iter()
        .filter(|r| !r.complete && r.coverage_fraction > 0.0)
        .count();
    let v = &report["verification"];
    if v["due_acquisition_deadlines"].as_u64() != Some(requests.len() as u64)
        || v["planned_met_deadlines"].as_u64() != Some(met as u64)
        || v["missed_deadlines_with_partial_coverage"].as_u64() != Some(partial as u64)
    {
        return Err("Reconstructed deadline results disagree with the saved solver report".into());
    }
    eprintln!(
        "Verified {} due deadlines: {met} met, {} missed ({partial} partial)",
        requests.len(),
        requests.len() - met
    );
    Ok(Assessment {
        job_cadence_hours: config.job_cadence_hours,
        requests,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rectangle(west: f64, east: f64) -> crate::geometry::Polygons {
        vec![vec![vec![
            [west, 0.0],
            [east, 0.0],
            [east, 1.0],
            [west, 1.0],
            [west, 0.0],
        ]]]
    }
    fn crop(job: usize, slot: u64, patch: usize, end: f64, west: f64, east: f64) -> input::Crop {
        input::Crop {
            job,
            slot,
            patch,
            start_s: end - 1.0,
            end_s: end,
            polygons_xy_km: rectangle(west, east),
        }
    }

    #[test]
    fn completion_uses_chronological_unique_coverage_across_patches_and_cycles() {
        let full = input::shape(rectangle(0.0, 2.0));
        let unit = input::shape(rectangle(0.0, 1.0));
        let originals = BTreeMap::from([
            ((0, 0), &full),
            ((1, 0), &full),
            ((2, 0), &unit),
            ((2, 1), &unit),
        ]);
        for reverse in [false, true] {
            let mut states = vec![
                vec![Completion::default(), Completion::default()],
                vec![Completion::default()],
                vec![Completion::default()],
            ];
            // Different satellite/archive orders must produce the same times.
            let mut crops = vec![
                crop(0, 0, 0, 30.0, 0.0, 2.0),  // redundant full pass, after completion
                crop(0, 0, 0, 20.0, 1.0, 2.0),  // complementary half completes job 0
                crop(1, 0, 0, 25.0, 0.0, 1.0),  // duplicate half never completes job 1
                crop(2, 0, 1, 12.0, 0.0, 1.0),  // distinct chart completes job 2
                crop(0, 0, 0, 10.0, -1.0, 1.0), // outside-AOI area must be clipped
                crop(1, 0, 0, 5.0, 0.0, 1.0),
                crop(0, 1, 0, 40.0, 0.0, 2.0), // separate acquisition cycle
                crop(2, 0, 0, 8.0, 0.0, 1.0),
                crop(0, 0, 0, 15.0, -1.0, 1.0), // overlapping duplicate
            ];
            if reverse {
                crops.reverse();
            }
            accumulate(&mut states, crops, &originals, &[2.0, 2.0, 2.0]).unwrap();
            assert_eq!(states[0][0].completed_at_s, Some(20.0));
            assert_eq!(states[0][1].completed_at_s, Some(40.0));
            assert_eq!(states[1][0].completed_at_s, None);
            assert!(!states[1][0].coverage.complete);
            assert_eq!(states[1][0].coverage.area, 1.0);
            assert_eq!(states[2][0].completed_at_s, Some(12.0));
        }
    }
}
