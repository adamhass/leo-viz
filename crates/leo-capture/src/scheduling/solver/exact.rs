//! Cached archived crop geometry and incremental, polygon-based plan evaluation.
//! Crop IDs follow archive order so unions agree with the independent verifier.
use super::*;
use crate::{geometry::Shape, parallel};
use geo::{Area, BooleanOps};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::BufRead;

pub(super) struct Crop {
    pub request: usize,
    pub patch: usize,
    pub shape: Shape,
}
pub(super) struct Geometry {
    pub crops: Vec<Crop>,
    pub actions: Vec<Vec<Vec<usize>>>,
    pub prepared_s: f64,
}
impl Geometry {
    pub fn crop_count(&self) -> usize {
        self.crops.len()
    }
    pub fn load(model: &Model, config: &Config) -> Result<Self> {
        let started = Instant::now();
        let originals: BTreeMap<_, _> = model
            .atlas
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
        let mut offset = 0;
        let request_offsets: Vec<_> = model
            .demands
            .iter()
            .map(|d| {
                let start = offset;
                offset += d.slots;
                start
            })
            .collect();
        let satellites = parallel::map(
            model.graphs.iter().collect(),
            config.workers,
            |g| {
                let mut actions = Vec::with_capacity(g.actions.len());
                for line in input::reader(
                    &config
                        .graph_directory
                        .join(format!("satellite-{:03}.actions.jsonl.gz", g.satellite)),
                )?
                .lines()
                .take(g.actions.len())
                {
                    let record: input::ActionRecord =
                        serde_json::from_str(&line.map_err(|e| e.to_string())?)
                            .map_err(|e| e.to_string())?;
                    let read = actions.len();
                    let action = record.action(model.manifest.config.options.step_s);
                    if record.id as usize != read
                        || record.tick != g.actions[read].tick
                        || record.mode != g.actions[read].mode
                        || action.start_s != g.actions[read].start_s
                        || action.end_s != g.actions[read].end_s
                    {
                        return Err("Action file changed while caching crop geometry".into());
                    }
                    let mut crops = Vec::new();
                    for crop in record.crops {
                        input::validate_crop(&crop, &action, &model.jobs)?;
                        if crop.slot as usize >= model.demands[crop.job].slots {
                            continue;
                        }
                        let original = originals
                            .get(&(crop.job, crop.patch))
                            .ok_or("Unknown crop chart while caching geometry")?;
                        let shape = input::shape(crop.polygons_xy_km);
                        let shape = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            shape.intersection(*original)
                        }))
                        .map_err(|_| {
                            "Polygon clipping failed while caching geometry".to_string()
                        })?;
                        if shape.unsigned_area() > 0.0 {
                            crops.push(Crop {
                                request: request_offsets[crop.job] + crop.slot as usize,
                                patch: crop.patch,
                                shape,
                            });
                        }
                    }
                    actions.push(crops);
                    if read % 128 == 0 {
                        resources::check_memory()?;
                    }
                }
                if actions.len() != g.actions.len() {
                    return Err("Action file truncated while caching geometry".into());
                }
                Ok(actions)
            },
            |n, total| {
                if n % 10 == 0 || n == total {
                    eprintln!("Caching polygons: {n}/{total} satellites");
                }
            },
        )?;
        let mut crops = Vec::new();
        let actions = satellites
            .into_iter()
            .map(|satellite| {
                satellite
                    .into_iter()
                    .map(|action| {
                        action
                            .into_iter()
                            .map(|crop| {
                                let id = crops.len();
                                crops.push(crop);
                                id
                            })
                            .collect()
                    })
                    .collect()
            })
            .collect();
        Ok(Self {
            crops,
            actions,
            prepared_s: started.elapsed().as_secs_f64(),
        })
    }
}

#[derive(Default, Serialize)]
pub(super) struct Stats {
    pub evaluation_s: f64,
    pub request_evaluations: usize,
    pub cache_hits: usize,
    pub polygon_unions: usize,
}
#[derive(Clone)]
pub(super) struct State {
    selected: Vec<BTreeSet<usize>>,
    rewards: Vec<f64>,
    pub score: f64,
}
pub(super) struct Replacement {
    requests: Vec<(usize, BTreeSet<usize>, f64)>,
    pub score: f64,
}
impl State {
    pub fn accept(&mut self, replacement: Replacement) {
        for (request, selected, reward) in replacement.requests {
            self.selected[request] = selected;
            self.rewards[request] = reward;
        }
        self.score = replacement.score;
    }
}

pub(super) struct Evaluator<'a> {
    model: &'a Model,
    geometry: &'a Geometry,
    reward: &'a reward::RewardModel,
    // Bounded per request. Values depend only on the selected crops, not prices.
    memo: Vec<HashMap<Vec<usize>, f64>>,
    pub stats: Stats,
}
impl<'a> Evaluator<'a> {
    pub fn new(model: &'a Model, geometry: &'a Geometry, reward: &'a reward::RewardModel) -> Self {
        Self {
            model,
            geometry,
            reward,
            memo: (0..model.requests.len()).map(|_| HashMap::new()).collect(),
            stats: Stats::default(),
        }
    }
    fn earned(&mut self, request: usize, selected: &BTreeSet<usize>) -> Result<f64> {
        let key: Vec<_> = selected.iter().copied().collect();
        if let Some(&value) = self.memo[request].get(&key) {
            self.stats.cache_hits += 1;
            return Ok(value);
        }
        self.stats.request_evaluations += 1;
        let job = self.model.requests[request].job;
        let total = self.model.atlas.job_areas_km2[job];
        let mut coverage = verify::Request::default();
        for &id in selected {
            let crop = &self.geometry.crops[id];
            coverage.add(crop.patch, &crop.shape, total)?;
            self.stats.polygon_unions += 1;
            if coverage.complete {
                break;
            }
        }
        let value = self.reward.earned(
            &self.model.jobs[job],
            total,
            coverage.area,
            coverage.complete,
        )?;
        if self.memo[request].len() >= 32 {
            self.memo[request].clear();
        }
        self.memo[request].insert(key, value);
        Ok(value)
    }
    pub fn state(&mut self, plan: &Plan) -> Result<State> {
        let started = Instant::now();
        let mut selected = vec![BTreeSet::new(); self.model.requests.len()];
        for (s, path) in plan.iter().enumerate() {
            for &a in path {
                for &id in &self.geometry.actions[s][a as usize] {
                    selected[self.geometry.crops[id].request].insert(id);
                }
            }
        }
        let rewards = selected
            .iter()
            .enumerate()
            .map(|(r, ids)| self.earned(r, ids))
            .collect::<Result<Vec<_>>>()?;
        let score = rewards.iter().sum();
        self.stats.evaluation_s += started.elapsed().as_secs_f64();
        Ok(State {
            selected,
            rewards,
            score,
        })
    }
    /// Rebuild only requests touched by either path. Rejection leaves state intact.
    pub fn replacement(
        &mut self,
        state: &State,
        satellite: usize,
        old: &[u32],
        new: &[u32],
    ) -> Result<Replacement> {
        let started = Instant::now();
        let mut affected = BTreeMap::new();
        for (path, add) in [(old, false), (new, true)] {
            for &a in path {
                for &id in &self.geometry.actions[satellite][a as usize] {
                    let request = self.geometry.crops[id].request;
                    let selected = affected
                        .entry(request)
                        .or_insert_with(|| state.selected[request].clone());
                    if add {
                        selected.insert(id);
                    } else {
                        selected.remove(&id);
                    }
                }
            }
        }
        let mut rewards = state.rewards.clone();
        let mut requests = Vec::with_capacity(affected.len());
        for (request, selected) in affected {
            let reward = self.earned(request, &selected)?;
            rewards[request] = reward;
            requests.push((request, selected, reward));
        }
        // Sum in request order, avoiding numerical drift across accepted moves.
        let score = rewards.iter().sum();
        self.stats.evaluation_s += started.elapsed().as_secs_f64();
        Ok(Replacement { requests, score })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::RewardMode;
    use geo::{Coord, MultiPolygon, Polygon, Rect};

    fn rectangle(x0: f64, x1: f64) -> Shape {
        MultiPolygon::new(vec![Rect::new(
            Coord { x: x0, y: 0.0 },
            Coord { x: x1, y: 1.0 },
        )
        .to_polygon()])
    }
    fn fixture(other_satellite_width: f64) -> (Model, Geometry) {
        let mut model = super::super::tests::tiny_model();
        model.atlas.job_areas_km2 = vec![1.0, 1.0];
        model.jobs[0].acquisition.reward_mode = Some(RewardMode::CompleteObservation);
        model.jobs[1].acquisition.reward_mode = Some(RewardMode::UniqueArea);
        let crops = vec![
            Crop {
                request: 0,
                patch: 0,
                shape: rectangle(0.0, 1.0),
            },
            Crop {
                request: 1,
                patch: 0,
                shape: rectangle(0.0, 1.0),
            },
            Crop {
                request: 0,
                patch: 0,
                shape: rectangle(0.0, other_satellite_width),
            },
        ];
        (
            model,
            Geometry {
                crops,
                actions: vec![vec![vec![0], vec![1]], vec![vec![2]]],
                prepared_s: 0.0,
            },
        )
    }

    #[test]
    fn exact_acceptance_rejects_a_sampled_gain_that_loses_a_completed_mosaic() {
        let (model, geometry) = fixture(0.5);
        for optimizer in [Optimizer::CoordinateImprovement, Optimizer::Lagrangian] {
            let mut config = Config {
                optimizer,
                workers: 1,
                iterations: 5,
                repair_sweeps: 2,
                ..Default::default()
            };
            let (exact, history, stats) =
                weighted::optimize(&model, &config, Some(&geometry)).unwrap();
            let mut evaluator = Evaluator::new(&model, &geometry, &config.reward);
            // Both halves look covered to the single witness, but only sat 0
            // actually completes A. Replacing it with B would reduce 32 to 16.
            assert_eq!(
                evaluator.state(&exact.last().unwrap().plan).unwrap().score,
                32.0
            );
            assert_eq!(stats["incumbent_reward"], 32.0);
            for h in history.as_array().unwrap() {
                assert!(
                    h["sampled_primal_reward"].as_f64().unwrap()
                        <= h["sampled_dual_upper"].as_f64().unwrap() + 1e-8
                );
            }
            config.acceptance = Acceptance::Sampled;
            let (sampled, _, _) = weighted::optimize(&model, &config, None).unwrap();
            assert_eq!(evaluator.state(&sampled[1].plan).unwrap().score, 16.0);
        }
    }

    #[test]
    fn exact_coordination_accepts_actual_gain_and_deduplicates_across_satellites() {
        let (model, geometry) = fixture(1.0);
        let config = Config {
            optimizer: Optimizer::CoordinateImprovement,
            workers: 1,
            repair_sweeps: 2,
            ..Default::default()
        };
        let (plans, _, report) = weighted::optimize(&model, &config, Some(&geometry)).unwrap();
        let mut evaluator = Evaluator::new(&model, &geometry, &config.reward);
        assert_eq!(evaluator.state(&plans[0].plan).unwrap().score, 32.0);
        assert_eq!(evaluator.state(&plans[1].plan).unwrap().score, 48.0);
        assert_eq!(report["accepted_paths"], 1);
        assert!(report["polygon_evaluation"]["cache_hits"].as_u64().unwrap() > 0);
    }

    #[test]
    fn polygon_reward_can_exceed_the_sampled_dual_without_corrupting_price_updates() {
        let (mut model, mut geometry) = fixture(0.5);
        model.jobs[0].acquisition.reward_mode = Some(RewardMode::UniqueArea);
        model.atlas.job_areas_km2[0] = 100.0;
        model.demands[0].points_per_slot = 2;
        model.demands[1].start = 2;
        (model.requests, model.point_request) = request_index(&model.demands);
        model.point_areas = vec![1.0, 99.0, 1.0];
        model.graphs.truncate(1);
        model.graphs[0].coverage = vec![vec![0], vec![2]];
        geometry.actions.truncate(1);
        geometry.crops[0].shape = rectangle(0.0, 90.0);
        let config = Config {
            workers: 1,
            iterations: 5,
            ..Default::default()
        };
        let (_, history, report) = weighted::optimize(&model, &config, Some(&geometry)).unwrap();
        assert_eq!(report["incumbent_reward"], 2880.0);
        for step in history.as_array().unwrap() {
            assert_eq!(step["sampled_primal_reward"], 32.0);
            assert_eq!(step["sampled_dual_upper"], 32.0);
            assert_eq!(step["acceptance_reward"], 2880.0);
        }
    }

    #[test]
    fn incremental_removal_preserves_holes_chart_pieces_and_other_deadlines() {
        let (mut model, mut geometry) = fixture(0.5);
        // Two deadlines for one partial-credit job. Its AOI consists of a square
        // with a hole on chart 0 (area .75) plus a .25 piece on chart 1.
        model.requests[1].job = 0;
        model.jobs[0].acquisition.reward_mode = Some(RewardMode::UniqueArea);
        let outer = Rect::new(Coord { x: 0., y: 0. }, Coord { x: 1., y: 1. })
            .to_polygon()
            .exterior()
            .clone();
        let hole = Rect::new(Coord { x: 0.25, y: 0.25 }, Coord { x: 0.75, y: 0.75 })
            .to_polygon()
            .exterior()
            .clone();
        geometry.crops[0].shape = MultiPolygon::new(vec![Polygon::new(outer, vec![hole])]);
        geometry.crops[2].shape = rectangle(0.0, 0.25);
        geometry.crops[2].patch = 1;
        let reward = reward::RewardModel::default();
        let mut evaluator = Evaluator::new(&model, &geometry, &reward);
        let mut state = evaluator.state(&vec![vec![0], vec![0]]).unwrap();
        assert_eq!(state.score, 32.0);
        let rejected = evaluator.replacement(&state, 0, &[0], &[]).unwrap();
        assert_eq!(rejected.score, 8.0);
        // Just evaluating a proposal cannot mutate the incumbent.
        assert_eq!(state.score, 32.0);
        let replacement = evaluator.replacement(&state, 0, &[0], &[1]).unwrap();
        assert_eq!(replacement.score, 40.0); // .25 of deadline 0 + all of deadline 1
        state.accept(replacement);
        let fresh = evaluator.state(&vec![vec![1], vec![0]]).unwrap();
        assert_eq!(state.score, fresh.score);
        assert_eq!(state.selected, fresh.selected);
        assert_eq!(state.rewards, fresh.rewards);
    }
}
