//! Shared area/priority objective with three interchangeable path heuristics.
//! Witness scores guide search; polygon unions determine published reward.
use super::*;
use crate::{parallel, workload::RewardMode};
use std::ops::Range;

struct Goal {
    points: Range<usize>,
    reward: f64,
}
struct Objective {
    goals: Vec<Goal>,
    point_goal: Vec<usize>,
    base: Vec<f64>,
}
impl Objective {
    fn new(model: &Model, reward: &reward::RewardModel) -> Result<Self> {
        if model.point_areas.len() != model.point_request.len() {
            return Err("Witness areas do not match coverage indices".into());
        }
        let mut out = Self {
            goals: Vec::new(),
            point_goal: vec![0; model.point_areas.len()],
            base: vec![0.0; model.point_areas.len()],
        };
        let mut total_reward = 0.0;
        for request in &model.requests {
            let job = &model.jobs[request.job];
            let full = reward.full_reward(job, model.atlas.job_areas_km2[request.job])?;
            total_reward += full;
            let range = request.start..request.start + request.points;
            let area: f64 = model.point_areas[range.clone()].iter().sum();
            if !area.is_finite()
                || area <= 0.0
                || model.point_areas[range.clone()]
                    .iter()
                    .any(|a| !a.is_finite() || *a <= 0.0)
            {
                return Err("Invalid area represented by coverage witnesses".into());
            }
            for p in range.clone() {
                out.base[p] = full * model.point_areas[p] / area;
                out.point_goal[p] = out.goals.len();
                if job.acquisition.effective_reward_mode() == RewardMode::UniqueArea {
                    out.goals.push(Goal {
                        points: p..p + 1,
                        reward: out.base[p],
                    });
                }
            }
            if job.acquisition.effective_reward_mode() == RewardMode::CompleteObservation {
                out.goals.push(Goal {
                    points: range,
                    reward: full,
                });
            }
        }
        if !total_reward.is_finite() {
            return Err("Total sampled reward exceeds finite numeric range".into());
        }
        Ok(out)
    }
    fn score(&self, counts: &[u32]) -> f64 {
        self.goals
            .iter()
            .filter(|g| counts[g.points.clone()].iter().all(|n| *n > 0))
            .map(|g| g.reward)
            .sum()
    }
    fn plan_score(&self, model: &Model, plan: &Plan) -> f64 {
        self.score(&search::counts(model, plan))
    }
}

fn action_rewards(graph: &Graph, prices: &[f64]) -> Vec<f64> {
    graph
        .coverage
        .iter()
        .map(|points| points.iter().map(|p| prices[*p as usize]).sum())
        .collect()
}
fn paths(model: &Model, prices: &[f64], workers: usize) -> Result<(f64, Plan)> {
    let solved = parallel::map(
        model.graphs.iter().collect(),
        workers,
        |g| Ok(search::longest_path(g, &action_rewards(g, prices))),
        |_, _| {},
    )?;
    Ok((
        solved.iter().map(|(value, _)| value).sum(),
        solved.into_iter().map(|(_, plan)| plan).collect(),
    ))
}
fn adjust(model: &Model, satellite: usize, path: &[u32], count: &mut [u32], add: bool) {
    for &a in path {
        for &p in &model.graphs[satellite].coverage[a as usize] {
            if add {
                count[p as usize] += 1;
            } else {
                count[p as usize] -= 1;
            }
        }
    }
}
fn captures(plan: &Plan) -> usize {
    plan.iter().map(Vec::len).sum()
}
fn better(value: f64, plan: &Plan, best: f64, incumbent: &Plan) -> bool {
    value > best || (value == best && captures(plan) < captures(incumbent))
}

#[derive(Clone)]
struct State {
    score: f64,
    exact: Option<exact::State>,
}
struct AcceptanceEvaluator<'a> {
    model: &'a Model,
    objective: &'a Objective,
    exact: Option<exact::Evaluator<'a>>,
    proposals: usize,
    accepted: usize,
    replacement_indices: Vec<coverage_path::Index>,
    replacement_path_labels: usize,
    replacement_search_s: f64,
}
impl<'a> AcceptanceEvaluator<'a> {
    fn state(&mut self, plan: &Plan) -> Result<State> {
        let exact = self.exact.as_mut().map(|e| e.state(plan)).transpose()?;
        let score = exact
            .as_ref()
            .map_or_else(|| self.objective.plan_score(self.model, plan), |s| s.score);
        Ok(State { score, exact })
    }
    /// Prices generate a replacement; the configured deduplicated objective accepts it.
    fn improve(
        &mut self,
        plan: &mut Plan,
        state: &mut State,
        prices: &[f64],
        sweeps: usize,
        rotation: usize,
    ) -> Result<()> {
        let model = self.model;
        let mut count = search::counts(model, plan);
        for sweep in 0..sweeps {
            for step in 0..model.graphs.len() {
                let s = (step + rotation + sweep) % model.graphs.len();
                adjust(model, s, &plan[s], &mut count, false);
                let marginal: Vec<_> = count
                    .iter()
                    .enumerate()
                    .map(|(p, n)| {
                        if *n == 0 {
                            prices[p] + self.objective.base[p]
                        } else {
                            0.0
                        }
                    })
                    .collect();
                let started = Instant::now();
                let replacement = self.replacement_indices[s].search(
                    &model.graphs[s],
                    &marginal,
                    self.replacement_path_labels,
                )
                .1;
                self.replacement_search_s += started.elapsed().as_secs_f64();
                adjust(model, s, &replacement, &mut count, true);
                self.proposals += 1;
                let exact = match (&mut self.exact, &state.exact) {
                    (Some(evaluator), Some(current)) => {
                        Some(evaluator.replacement(current, s, &plan[s], &replacement)?)
                    }
                    _ => None,
                };
                let proposed = exact
                    .as_ref()
                    .map_or_else(|| self.objective.score(&count), |r| r.score);
                if proposed > state.score
                    || (proposed == state.score && replacement.len() < plan[s].len())
                {
                    plan[s] = replacement;
                    state.score = proposed;
                    if let Some(replacement) = exact {
                        state.exact.as_mut().unwrap().accept(replacement);
                    }
                    self.accepted += 1;
                } else {
                    adjust(model, s, &replacement, &mut count, false);
                    adjust(model, s, &plan[s], &mut count, true);
                }
            }
            resources::check_memory()?;
        }
        Ok(())
    }
    fn report(&self, mode: Acceptance, score: f64) -> serde_json::Value {
        serde_json::json!({"mode":mode,"proposed_paths":self.proposals,"accepted_paths":self.accepted,
            "replacement_search":{"method":"unique_coverage","labels_per_node":self.replacement_path_labels,"elapsed_s":self.replacement_search_s},
            "incumbent_reward":score,"polygon_evaluation":self.exact.as_ref().map(|e| &e.stats)})
    }
}

pub(super) fn optimize(
    model: &Model,
    config: &Config,
    geometry: Option<&exact::Geometry>,
) -> Result<(Vec<Candidate>, serde_json::Value, serde_json::Value)> {
    let objective = Objective::new(model, &config.reward)?;
    let (_, mut best) = paths(model, &objective.base, config.workers)?;
    let mut candidates = vec![Candidate {
        name: "independent_paths".into(),
        plan: best.clone(),
    }];
    if config.optimizer == Optimizer::IndependentPaths {
        return Ok((
            candidates,
            serde_json::json!([]),
            serde_json::json!({"mode":"none","proposed_paths":0,"accepted_paths":0}),
        ));
    }
    let exact = if config.acceptance == Acceptance::ExactPolygon {
        Some(exact::Evaluator::new(
            model,
            geometry.ok_or("Exact polygon acceptance requires cached geometry")?,
            &config.reward,
        ))
    } else {
        None
    };
    let mut evaluator = AcceptanceEvaluator {
        model,
        objective: &objective,
        exact,
        proposals: 0,
        accepted: 0,
        replacement_indices: model.graphs.iter().map(coverage_path::Index::new).collect(),
        replacement_path_labels: config.replacement_path_labels,
        replacement_search_s: 0.0,
    };
    let mut best_state = evaluator.state(&best)?;
    if let Some(plan) = incumbent::load(model, config)? {
        let state = evaluator.state(&plan)?;
        candidates.push(Candidate {
            name: "retained_incumbent".into(),
            plan: plan.clone(),
        });
        if better(state.score, &plan, best_state.score, &best) {
            best = plan;
            best_state = state;
        }
    }
    let mut sampled_lower = objective.plan_score(model, &best);
    evaluator.improve(
        &mut best,
        &mut best_state,
        &objective.base,
        config.repair_sweeps,
        0,
    )?;
    candidates.push(Candidate {
        name: "coordinate_improvement".into(),
        plan: best.clone(),
    });
    if config.optimizer == Optimizer::CoordinateImprovement {
        return Ok((
            candidates,
            serde_json::json!([]),
            evaluator.report(config.acceptance, best_state.score),
        ));
    }
    sampled_lower = sampled_lower.max(objective.plan_score(model, &best));
    let mut prices = objective.base.clone();
    let mut best_upper = f64::INFINITY;
    let mut history = Vec::new();
    for iteration in 0..config.iterations {
        let (path_value, raw) = paths(model, &prices, config.workers)?;
        let count = search::counts(model, &raw);
        let mut z = Vec::with_capacity(objective.goals.len());
        let mut constant = 0.0;
        for goal in &objective.goals {
            let reduced = goal.reward - prices[goal.points.clone()].iter().sum::<f64>();
            z.push(reduced >= 0.0);
            constant += reduced.max(0.0);
        }
        let upper = path_value + constant;
        best_upper = best_upper.min(upper);
        sampled_lower = sampled_lower.max(objective.score(&count));
        let mut repaired_state = evaluator.state(&raw)?;
        if better(repaired_state.score, &raw, best_state.score, &best) {
            best = raw.clone();
            best_state = repaired_state.clone();
        }
        let mut repaired = raw;
        evaluator.improve(
            &mut repaired,
            &mut repaired_state,
            &prices,
            config.repair_sweeps,
            iteration,
        )?;
        sampled_lower = sampled_lower.max(objective.plan_score(model, &repaired));
        if better(repaired_state.score, &repaired, best_state.score, &best) {
            best = repaired;
            best_state = repaired_state;
        }
        evaluator.improve(
            &mut best,
            &mut best_state,
            &prices,
            config.repair_sweeps,
            iteration,
        )?;
        sampled_lower = sampled_lower.max(objective.plan_score(model, &best));
        // Polygon rewards are a different objective. Never use them in sampled
        // bounds, stopping conditions or Polyak steps, even when they exceed a bound.
        if !upper.is_finite() || sampled_lower > best_upper + 1e-7 * best_upper.abs().max(1.0) {
            return Err("Invalid common-reward Lagrangian bound".into());
        }
        history.push(serde_json::json!({"iteration":iteration,"sampled_primal_reward":sampled_lower,"sampled_dual_upper":best_upper,"acceptance_reward":best_state.score}));
        if candidates.iter().all(|c| c.plan != best) {
            candidates.push(Candidate {
                name: format!("lagrangian_iteration_{}", iteration + 1),
                plan: best.clone(),
            });
        }
        let norm: f64 = count
            .iter()
            .enumerate()
            .map(|(p, n)| (f64::from(*n) - f64::from(z[objective.point_goal[p]])).powi(2))
            .sum();
        if norm <= 1e-20 || best_upper - sampled_lower <= 1e-8 * best_upper.abs().max(1.0) {
            break;
        }
        let step = 1.5 * (upper - sampled_lower).max(0.0) / norm;
        for (p, price) in prices.iter_mut().enumerate() {
            *price = (*price
                - step * (f64::from(count[p]) - f64::from(z[objective.point_goal[p]])))
            .max(0.0);
            if !price.is_finite() {
                return Err("Non-finite coverage price".into());
            }
        }
        resources::check_memory()?;
        eprintln!(
            "Common-reward LR {}/{}: accepted reward {:.3}, sampled reward {:.3}, upper {:.3}",
            iteration + 1,
            config.iterations,
            best_state.score,
            sampled_lower,
            best_upper
        );
    }
    Ok((
        candidates,
        serde_json::json!(history),
        evaluator.report(config.acceptance, best_state.score),
    ))
}

pub(super) fn sampled_reward(
    model: &Model,
    reward: &reward::RewardModel,
    plan: &Plan,
) -> Result<f64> {
    Ok(Objective::new(model, reward)?.plan_score(model, plan))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Model {
        let mut model = super::super::tests::tiny_model();
        model.atlas.job_areas_km2 = vec![1.0, 1.0];
        model
    }

    #[test]
    fn all_optimizers_share_one_objective_and_coordination_recovers_unique_work() {
        let model = fixture();
        let objective = Objective::new(&model, &Default::default()).unwrap();
        for optimizer in [
            Optimizer::IndependentPaths,
            Optimizer::CoordinateImprovement,
            Optimizer::Lagrangian,
        ] {
            let config = Config {
                optimizer,
                iterations: 8,
                repair_sweeps: 2,
                workers: 1,
                acceptance: Acceptance::Sampled,
                ..Default::default()
            };
            let (candidates, history, _) = optimize(&model, &config, None).unwrap();
            let best = candidates
                .iter()
                .map(|c| objective.plan_score(&model, &c.plan))
                .fold(0.0, f64::max);
            assert_eq!(
                best,
                if optimizer == Optimizer::IndependentPaths {
                    32.0
                } else {
                    48.0
                }
            );
            for c in &candidates {
                for (g, path) in model.graphs.iter().zip(&c.plan) {
                    verify::validate_path(g, path).unwrap();
                }
            }
            for step in history.as_array().unwrap() {
                assert!(step["sampled_primal_reward"].as_f64().unwrap() <= 48.0);
                assert!(step["sampled_dual_upper"].as_f64().unwrap() >= 48.0 - 1e-8);
            }
        }
    }

    #[test]
    fn replacement_favors_new_work_over_repetitions_and_keeps_valid_dual_bounds() {
        use super::super::tests::{action, graph};
        let mut model = fixture();
        model.graphs = vec![graph(
            (0..5).map(action).collect(),
            vec![vec![0], vec![0], vec![0], vec![1], vec![2]],
            &[(0, 1), (1, 2), (3, 4)],
            &[0, 3],
        )];
        model.atlas.job_areas_km2 = vec![10. / 32., 12. / 16.];
        model.jobs[1].acquisition.reward_mode = Some(RewardMode::CompleteObservation);
        model.demands[1].points_per_slot = 2;
        (model.requests, model.point_request) = request_index(&model.demands);
        model.point_areas = vec![1., 0.5, 0.5];
        let objective = Objective::new(&model, &Default::default()).unwrap();
        for optimizer in [Optimizer::CoordinateImprovement, Optimizer::Lagrangian] {
            let config = Config {
                optimizer,
                iterations: 5,
                repair_sweeps: 1,
                workers: 1,
                acceptance: Acceptance::Sampled,
                ..Default::default()
            };
            let (plans, history, report) = optimize(&model, &config, None).unwrap();
            assert_eq!(objective.plan_score(&model, &plans[0].plan), 10.);
            assert_eq!(objective.plan_score(&model, &plans[1].plan), 12.);
            assert_eq!(plans[1].plan, vec![vec![3, 4]]);
            assert_eq!(report["replacement_search"]["method"], "unique_coverage");
            for step in history.as_array().unwrap() {
                assert!(step["sampled_primal_reward"].as_f64().unwrap() <= 12.);
                assert!(step["sampled_dual_upper"].as_f64().unwrap() >= 12.);
            }
        }
    }

    #[test]
    fn witness_area_and_job_policy_control_reward_without_duplicate_credit() {
        let mut model = fixture();
        model.jobs.truncate(1);
        model.jobs[0].acquisition.priority = 2;
        model.atlas.job_areas_km2 = vec![100.0];
        model.requests = vec![AcquisitionRequest {
            job: 0,
            start: 0,
            points: 2,
        }];
        model.point_request = vec![0, 0];
        model.point_areas = vec![90.0, 10.0];
        let reward = reward::RewardModel::default();
        model.jobs[0].acquisition.reward_mode = Some(RewardMode::UniqueArea);
        let partial = Objective::new(&model, &reward).unwrap();
        assert_eq!(partial.score(&[1, 0]), 360.0);
        assert_eq!(partial.score(&[7, 0]), 360.0);
        assert_eq!(partial.score(&[1, 9]), 400.0);
        model.jobs[0].acquisition.reward_mode = Some(RewardMode::CompleteObservation);
        let full = Objective::new(&model, &reward).unwrap();
        assert_eq!(full.score(&[7, 0]), 0.0);
        assert_eq!(full.score(&[1, 9]), 400.0);
    }

    #[test]
    fn finite_priority_multiplier_can_trade_a_small_high_priority_job_for_more_area() {
        let mut model = fixture();
        model.graphs.truncate(1);
        model.atlas.job_areas_km2 = vec![1.0, 100.0];
        let config = Config {
            optimizer: Optimizer::IndependentPaths,
            workers: 1,
            acceptance: Acceptance::Sampled,
            ..Default::default()
        };
        let (candidates, _, _) = optimize(&model, &config, None).unwrap();
        assert_eq!(candidates[0].plan, vec![vec![1]]);
        assert_eq!(
            sampled_reward(&model, &config.reward, &candidates[0].plan).unwrap(),
            1600.0
        );
    }

    #[test]
    fn lagrangian_bounds_cover_the_known_mixed_policy_optimum() {
        let mut model = fixture();
        for job in &mut model.jobs {
            job.acquisition.priority = 1;
        }
        model.jobs[0].acquisition.reward_mode = Some(RewardMode::CompleteObservation);
        model.jobs[1].acquisition.reward_mode = Some(RewardMode::UniqueArea);
        model.atlas.job_areas_km2 = vec![1.0, 3.0];
        model.requests = vec![
            AcquisitionRequest {
                job: 0,
                start: 0,
                points: 2,
            },
            AcquisitionRequest {
                job: 1,
                start: 2,
                points: 1,
            },
        ];
        model.demands = vec![
            Demand {
                start: 0,
                points_per_slot: 2,
                slots: 1,
            },
            Demand {
                start: 2,
                points_per_slot: 1,
                slots: 1,
            },
        ];
        model.point_request = vec![0, 0, 1];
        model.point_areas = vec![0.5, 0.5, 3.0];
        model.graphs[0].coverage = vec![vec![0], vec![2]];
        model.graphs[1].coverage = vec![vec![1]];
        // Satellite 0 must choose: finish job A together with satellite 1 (2),
        // or cover the larger job B (6). A's remaining half earns no reward.
        let config = Config {
            iterations: 10,
            workers: 1,
            acceptance: Acceptance::Sampled,
            ..Default::default()
        };
        let (candidates, history, _) = optimize(&model, &config, None).unwrap();
        let best = candidates
            .iter()
            .map(|c| sampled_reward(&model, &config.reward, &c.plan).unwrap())
            .fold(0.0, f64::max);
        assert_eq!(best, 6.0);
        for step in history.as_array().unwrap() {
            assert!(step["sampled_primal_reward"].as_f64().unwrap() <= 6.0);
            assert!(step["sampled_dual_upper"].as_f64().unwrap() >= 6.0 - 1e-8);
        }
    }
}
