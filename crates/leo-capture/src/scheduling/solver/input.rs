use super::*;
use crate::{
    geometry::{self, Atlas, Shape},
    parallel,
    workload::hash,
};
use flate2::read::GzDecoder;
use geo::{
    Area, BooleanOps, BoundingRect, Contains, InteriorPoint, LineString, MultiPolygon, Point,
    Polygon, Rect,
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, VecDeque},
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
};

#[derive(Deserialize)]
pub(super) struct Crop {
    pub job: usize,
    pub slot: u64,
    pub patch: usize,
    pub start_s: f64,
    pub end_s: f64,
    pub polygons_xy_km: geometry::Polygons,
}
#[derive(Deserialize)]
pub(super) struct ActionRecord {
    pub id: u32,
    pub tick: u32,
    pub mode: usize,
    #[serde(default)]
    pub start_s: Option<f64>,
    #[serde(default)]
    pub end_s: Option<f64>,
    pub crops: Vec<Crop>,
}
impl ActionRecord {
    pub(super) fn action(&self, step: f64) -> super::super::Action {
        super::super::Action {
            tick: self.tick,
            mode: self.mode,
            start_s: self.start_s.unwrap_or(f64::from(self.tick) * step),
            end_s: self.end_s.unwrap_or(f64::from(self.tick + 1) * step),
        }
    }
}
#[derive(Deserialize)]
struct Header {
    satellite: usize,
    action_count: usize,
    nodes: usize,
    source: usize,
    sink: usize,
    source_to: Vec<u32>,
}
#[derive(Deserialize)]
struct Node {
    action: Option<u32>,
    tick: Option<u32>,
    mode: Option<usize>,
    #[serde(default)]
    start_s: Option<f64>,
    #[serde(default)]
    end_s: Option<f64>,
    next_same: Option<u32>,
    #[serde(default)]
    maneuver_to: Vec<u32>,
    maneuver_hub: Option<usize>,
    hub: Option<usize>,
    #[serde(default)]
    targets: Vec<u32>,
}
pub(super) fn reader(path: &Path) -> Result<impl BufRead> {
    Ok(BufReader::new(GzDecoder::new(
        File::open(path).map_err(|e| format!("{}: {e}", path.display()))?,
    )))
}
pub(super) fn shape(polygons: geometry::Polygons) -> Shape {
    MultiPolygon::new(
        polygons
            .into_iter()
            .filter(|p| !p.is_empty())
            .map(|rings| {
                let mut rings = rings.into_iter().map(|r| {
                    LineString::from(r.into_iter().map(|p| (p[0], p[1])).collect::<Vec<_>>())
                });
                Polygon::new(rings.next().unwrap(), rings.collect())
            })
            .collect(),
    )
}
type Samples = BTreeMap<(usize, usize), Vec<(Point<f64>, usize)>>;

pub(super) fn job_scope(jobs: &[Job], cadence_hours: Option<f64>) -> Result<Vec<bool>> {
    if cadence_hours.is_some_and(|h| !h.is_finite() || h <= 0.0) {
        return Err("Job cadence filter must be finite and positive".into());
    }
    let included: Vec<_> = jobs
        .iter()
        .map(|job| cadence_hours.is_none_or(|h| job.cadence_hours == h))
        .collect();
    if cadence_hours.is_some() && !included.iter().any(|&yes| yes) {
        return Err("No archived jobs match the requested cadence".into());
    }
    Ok(included)
}

pub(super) fn load(config: &Config) -> Result<Model> {
    let root = &config.graph_directory;
    let manifest: super::super::Summary =
        serde_json::from_reader(File::open(root.join("manifest.json")).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if manifest.status != "complete" || root.join("INCOMPLETE").exists() {
        return Err("Graph generation is incomplete".into());
    }
    manifest.config.options.validate()?;
    let options = &manifest.config.options;
    if config.horizon_s > options.duration_s
        || (config.horizon_s / options.step_s - (config.horizon_s / options.step_s).round()).abs()
            > 1e-8
    {
        return Err(
            "Solver horizon must be a whole number of graph ticks within the saved horizon".into(),
        );
    }
    let preset = if root.join("preset.json.gz").exists() {
        root.join("preset.json.gz")
    } else {
        root.join("preset.json")
    };
    if hash(&fs::read(&preset).map_err(|e| e.to_string())?) != manifest.preset_hash_fnv1a64 {
        return Err("Archived preset checksum mismatch".into());
    }
    eprintln!("Loading archived jobs and rebuilding their original geographic charts");
    let (jobs, regions, _) = super::super::load_preset(&preset)?;
    if jobs.len() != manifest.job_count {
        return Err("Job count does not match graph manifest".into());
    }
    let included_jobs = job_scope(&jobs, config.job_cadence_hours)?;
    eprintln!(
        "Job scope: {} of {} archived jobs",
        included_jobs.iter().filter(|&&yes| yes).count(),
        jobs.len()
    );
    let slots: Vec<_> = jobs
        .iter()
        .zip(&included_jobs)
        .map(|(j, &included)| {
            if included {
                deadline_counts(j, config.horizon_s).0
            } else {
                0
            }
        })
        .collect();
    let atlas = Atlas::new(&jobs, &regions, options.cell_deg, options.edge_km, |_| true)?;
    drop(regions);
    let saved_charts: Vec<serde_json::Value> =
        serde_json::from_reader(File::open(root.join("charts.json")).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if saved_charts.len() != atlas.patches.len() {
        return Err("Rebuilt chart count differs from graph export".into());
    }
    for (id, patch) in atlas.patches.iter().enumerate() {
        let center = &saved_charts[id]["center_unit"];
        // The writer's chart identifiers and centres must match exactly.
        if saved_charts[id]["patch"].as_u64() != Some(id as u64)
            || (0..3).any(|i| {
                center[i]
                    .as_f64()
                    .is_none_or(|x| (x - patch.chart.center[i]).abs() > 1e-12)
            })
        {
            return Err(format!("Rebuilt chart {id} differs from graph export"));
        }
    }
    eprintln!(
        "Building spatial coverage witnesses ({} by {} per chart piece)",
        config.samples_per_axis, config.samples_per_axis
    );
    let mut samples = Samples::new();
    let mut counts = vec![0usize; jobs.len()];
    for (patch_id, patch) in atlas.patches.iter().enumerate() {
        for piece in &patch.pieces {
            if slots[piece.job] == 0 {
                continue;
            }
            let mut points = Vec::new();
            let bounds = piece.bounds;
            let n = config.samples_per_axis;
            for y in 0..n {
                for x in 0..n {
                    let lo = bounds.min();
                    let hi = bounds.max();
                    let rect = Rect::new(
                        (
                            lo.x + (hi.x - lo.x) * x as f64 / n as f64,
                            lo.y + (hi.y - lo.y) * y as f64 / n as f64,
                        ),
                        (
                            lo.x + (hi.x - lo.x) * (x + 1) as f64 / n as f64,
                            lo.y + (hi.y - lo.y) * (y + 1) as f64 / n as f64,
                        ),
                    );
                    let clipped = piece
                        .shape
                        .intersection(&MultiPolygon::new(vec![rect.to_polygon()]));
                    // Include every nonempty component, including narrow rivers and islands.
                    for polygon in clipped.0 {
                        if polygon.unsigned_area() > 1e-12 {
                            if let Some(point) = polygon.interior_point() {
                                points.push((point, counts[piece.job]));
                                counts[piece.job] += 1;
                            }
                        }
                    }
                }
            }
            samples.insert((piece.job, patch_id), points);
        }
        if patch_id % 128 == 0 {
            resources::check_memory()?;
        }
    }
    let mut point_count = 0;
    let demands: Vec<_> = counts
        .into_iter()
        .enumerate()
        .map(|(job, count)| {
            let d = Demand {
                start: point_count,
                points_per_slot: count,
                slots: slots[job],
            };
            point_count += d.len();
            d
        })
        .collect();
    if point_count > u32::MAX as usize {
        return Err("Too many coverage witnesses; reduce samples or horizon".into());
    }
    if demands
        .iter()
        .any(|d| d.slots > 0 && d.points_per_slot == 0)
    {
        return Err("A due job has no valid coverage witnesses".into());
    }
    let (requests, point_request) = request_index(&demands);
    let ticks = (config.horizon_s / options.step_s).round() as u32;
    let mut last = 0;
    let graphs = parallel::map(
        manifest.satellites.iter().map(|s| s.satellite).collect(),
        config.workers,
        |satellite| {
            load_graph(
                root,
                satellite,
                ticks,
                &samples,
                &demands,
                &jobs,
                options.step_s,
            )
        },
        |done, total| {
            if done != last {
                eprintln!("Indexed coverage for {done}/{total} satellites");
                last = done;
            }
        },
    )?;
    Ok(Model {
        jobs,
        included_jobs,
        demands,
        requests,
        point_request,
        graphs,
        atlas,
        manifest,
    })
}

fn load_graph(
    root: &Path,
    satellite: usize,
    ticks: u32,
    samples: &Samples,
    demands: &[Demand],
    jobs: &[Job],
    step: f64,
) -> Result<Graph> {
    let mut actions = Vec::new();
    let mut coverage = Vec::new();
    let mut reached_horizon = false;
    for line in reader(&root.join(format!("satellite-{satellite:03}.actions.jsonl.gz")))?.lines() {
        let record: ActionRecord =
            serde_json::from_str(&line.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let stamp = record.action(step);
        if !stamp.start_s.is_finite()
            || !stamp.end_s.is_finite()
            || stamp.start_s < 0.0
            || stamp.end_s <= stamp.start_s
        {
            return Err("Invalid acquisition timestamps".into());
        }
        if stamp.start_s >= f64::from(ticks) * step {
            if record.id as usize != actions.len() {
                return Err("Action prefix is not contiguous".into());
            }
            reached_horizon = true;
            break;
        }
        if record.id as usize != actions.len()
            || actions
                .last()
                .is_some_and(|a: &super::super::Action| a.start_s > stamp.start_s)
        {
            return Err("Action IDs or times are not ordered".into());
        }
        let mut credited = Vec::new();
        for crop in record.crops {
            validate_crop(&crop, &stamp, jobs)?;
            let d = &demands[crop.job];
            if crop.slot as usize >= d.slots {
                continue;
            }
            let points = samples
                .get(&(crop.job, crop.patch))
                .ok_or("Crop refers to an unknown job/chart piece")?;
            let polygon = shape(crop.polygons_xy_km);
            let bounds = polygon.bounding_rect().ok_or("Empty crop polygon")?;
            for &(point, local) in points {
                if bounds.contains(&point) && polygon.contains(&point) {
                    credited
                        .push((d.start + crop.slot as usize * d.points_per_slot + local) as u32);
                }
            }
        }
        credited.sort_unstable();
        credited.dedup();
        coverage.push(credited);
        if stamp.end_s > f64::from(ticks) * step + 1e-9 {
            return Err("Horizon cuts an acquisition interval".into());
        }
        actions.push(stamp);
        if actions.len() % 1024 == 0 {
            resources::check_memory()?;
        }
    }
    let mut lines = reader(&root.join(format!("satellite-{satellite:03}.graph.jsonl.gz")))?.lines();
    let h: Header = serde_json::from_str(
        &lines
            .next()
            .ok_or("Empty graph")?
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if h.satellite != satellite
        || h.source != 2 * h.action_count
        || h.sink != h.source + 1
        || h.nodes < h.sink + 1
        || actions.len() > h.action_count
    {
        return Err("Invalid graph header".into());
    }
    if !reached_horizon && actions.len() != h.action_count {
        return Err("Truncated action file".into());
    }
    let n = actions.len();
    let source = 2 * n;
    let sink = source + 1;
    let old_base = h.sink + 1;
    let mut edges = vec![Vec::new(); 2 * n + 2 + h.nodes - old_base];
    edges[source].push(sink as u32);
    for a in h.source_to {
        if a as usize >= h.action_count {
            return Err("Invalid source target".into());
        }
        if (a as usize) < n {
            edges[source].push(2 * a);
        }
    }
    let mut seen = vec![false; h.action_count];
    let mut seen_hub = vec![false; h.nodes - old_base];
    for line in lines {
        let r: Node =
            serde_json::from_str(&line.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        if let Some(a) = r.action {
            let a = a as usize;
            if a >= seen.len() || std::mem::replace(&mut seen[a], true) {
                return Err("Duplicate or invalid graph action".into());
            }
            if a >= n {
                continue;
            }
            if r.tick != Some(actions[a].tick)
                || r.mode != Some(actions[a].mode)
                || r.start_s.is_some_and(|t| t != actions[a].start_s)
                || r.end_s.is_some_and(|t| t != actions[a].end_s)
            {
                return Err("Action geometry and graph metadata disagree".into());
            }
            edges[2 * a].push((2 * a + 1) as u32); // capture or skip; reward selects which.
                                                   // Finishing a prefix is equivalent to skipping/waiting to the original sink.
            edges[2 * a + 1].push(sink as u32);
            for to in r.next_same.into_iter().chain(r.maneuver_to) {
                if to as usize >= h.action_count {
                    return Err("Invalid graph target".into());
                }
                if (to as usize) < n {
                    edges[2 * a + 1].push(2 * to);
                }
            }
            if let Some(hub) = r.maneuver_hub {
                if !(old_base..h.nodes).contains(&hub) {
                    return Err("Invalid maneuver hub".into());
                }
                edges[2 * a + 1].push((2 * n + 2 + hub - old_base) as u32);
            }
        } else if let Some(hub) = r.hub {
            if !(old_base..h.nodes).contains(&hub)
                || std::mem::replace(&mut seen_hub[hub - old_base], true)
            {
                return Err("Invalid hub record".into());
            }
            for to in r.targets {
                if to as usize >= h.action_count {
                    return Err("Invalid hub target".into());
                }
                if (to as usize) < n {
                    edges[2 * n + 2 + hub - old_base].push(2 * to);
                }
            }
        } else {
            return Err("Unknown graph record".into());
        }
    }
    if seen.contains(&false) || seen_hub.contains(&false) {
        return Err("Truncated graph".into());
    }
    let order = topological(&edges)?;
    Ok(Graph {
        satellite,
        actions,
        coverage,
        edges,
        order,
        source,
        sink,
    })
}
pub(super) fn topological(edges: &[Vec<u32>]) -> Result<Vec<usize>> {
    let mut degree = vec![0usize; edges.len()];
    for out in edges {
        for &to in out {
            if to as usize >= edges.len() {
                return Err("Invalid edge endpoint".into());
            }
            degree[to as usize] += 1;
        }
    }
    let mut ready: VecDeque<_> = degree
        .iter()
        .enumerate()
        .filter(|(_, n)| **n == 0)
        .map(|(i, _)| i)
        .collect();
    let mut order = Vec::with_capacity(edges.len());
    while let Some(n) = ready.pop_front() {
        order.push(n);
        for &to in &edges[n] {
            degree[to as usize] -= 1;
            if degree[to as usize] == 0 {
                ready.push_back(to as usize);
            }
        }
    }
    if order.len() != edges.len() {
        return Err("Scheduling graph contains a cycle".into());
    }
    Ok(order)
}
pub(super) fn validate_crop(c: &Crop, action: &super::super::Action, jobs: &[Job]) -> Result<()> {
    let j = jobs.get(c.job).ok_or("Crop job index out of range")?;
    let (lo, hi) = j.acquisition.collection_window_s(c.slot, j.cadence_hours);
    if !c.start_s.is_finite()
        || !c.end_s.is_finite()
        || c.start_s >= c.end_s
        || c.start_s < lo - 1e-6
        || c.end_s > hi + 1e-6
        || c.start_s < action.start_s - 1e-6
        || c.end_s > action.end_s + 1e-6
    {
        return Err("Crop time crosses its action or cadence window".into());
    }
    Ok(())
}
