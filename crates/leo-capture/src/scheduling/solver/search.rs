use super::*;
use crate::parallel;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn priorities(model: &Model) -> Vec<u16> {
    model
        .jobs
        .iter()
        .zip(&model.demands)
        .filter(|(_, d)| d.slots > 0)
        .map(|(j, _)| j.acquisition.priority)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// Exact additive maximum-reward path; capture and skip remain distinct choices.
pub(super) fn longest_path(g: &Graph, reward: &[f64]) -> (f64, Vec<u32>) {
    assert_eq!(reward.len(), g.actions.len());
    let mut value = vec![f64::NEG_INFINITY; g.edges.len()];
    let mut count = vec![usize::MAX; g.edges.len()];
    let mut previous = vec![usize::MAX; g.edges.len()];
    value[g.source] = 0.0;
    count[g.source] = 0;
    for &from in &g.order {
        if !value[from].is_finite() {
            continue;
        }
        let capture = from < 2 * g.actions.len() && from % 2 == 0;
        let gain = if capture {
            reward[from / 2].max(0.0)
        } else {
            0.0
        };
        for &to in &g.edges[from] {
            let to = to as usize;
            let proposed = value[from] + gain;
            let captures = count[from] + usize::from(gain > 0.0);
            if proposed > value[to] || (proposed == value[to] && captures < count[to]) {
                value[to] = proposed;
                count[to] = captures;
                previous[to] = from;
            }
        }
    }
    let mut selected = Vec::new();
    let mut node = g.sink;
    while previous[node] != usize::MAX {
        let from = previous[node];
        if from < 2 * g.actions.len() && from % 2 == 0 && node == from + 1 && reward[from / 2] > 0.0
        {
            selected.push((from / 2) as u32);
        }
        node = from;
    }
    selected.reverse();
    (value[g.sink], selected)
}
fn rewards(g: &Graph, prices: &[f64]) -> Vec<f64> {
    g.coverage
        .iter()
        .map(|c| c.iter().map(|&p| prices[p as usize]).sum())
        .collect()
}
pub(super) fn counts(model: &Model, plan: &Plan) -> Vec<u32> {
    let mut count = vec![0; model.point_request.len()];
    for (g, path) in model.graphs.iter().zip(plan) {
        adjust(&mut count, g, path, true);
    }
    count
}
fn adjust(count: &mut [u32], g: &Graph, path: &[u32], add: bool) {
    for &a in path {
        for &p in &g.coverage[a as usize] {
            if add {
                count[p as usize] += 1;
            } else {
                count[p as usize] -= 1;
            }
        }
    }
}
pub(super) fn evaluate(model: &Model, count: &[u32]) -> Score {
    let priorities = priorities(model);
    let index: BTreeMap<_, _> = priorities
        .iter()
        .enumerate()
        .map(|(i, &p)| (p, i))
        .collect();
    let mut score = Score {
        complete_requests: vec![0; priorities.len()],
        fractional_requests: 0.0,
    };
    for (j, d) in model.demands.iter().enumerate() {
        if d.slots == 0 {
            continue;
        }
        let level = index[&model.jobs[j].acquisition.priority];
        let mut complete = 0;
        for points in count[d.start..d.start + d.len()].chunks(d.points_per_slot) {
            let n = points.iter().filter(|&&n| n > 0).count();
            complete += usize::from(n == points.len());
            score.fractional_requests += n as f64 / points.len() as f64;
        }
        score.complete_requests[level] += complete as u64;
    }
    score
}
fn complete(model: &Model, count: &[u32], request: usize) -> bool {
    let r = &model.requests[request];
    r.points > 0 && count[r.start..r.start + r.points].iter().all(|&n| n > 0)
}
fn objective(model: &Model, count: &[u32], weights: &[f64]) -> f64 {
    weights
        .iter()
        .enumerate()
        .filter(|(j, _)| complete(model, count, *j))
        .map(|(_, w)| w)
        .sum()
}
/// Remove a satellite's old coverage before pricing its replacement. Accept only
/// actual (deduplicated) improvement, never the additive path score alone.
fn repair(
    model: &Model,
    plan: &mut Plan,
    prices: &[f64],
    base: &[f64],
    sweeps: usize,
    rotation: usize,
) -> Score {
    let mut count = counts(model, plan);
    let mut score = evaluate(model, &count);
    for sweep in 0..sweeps {
        for i in 0..model.graphs.len() {
            let s = (i + rotation + sweep) % model.graphs.len();
            let g = &model.graphs[s];
            adjust(&mut count, g, &plan[s], false);
            let reward: Vec<_> = g
                .coverage
                .iter()
                .map(|c| {
                    c.iter()
                        .map(|&p| {
                            let p = p as usize;
                            if count[p] == 0 {
                                prices[p] + base[p]
                            } else {
                                0.0
                            }
                        })
                        .sum()
                })
                .collect();
            let (_, replacement) = longest_path(g, &reward);
            adjust(&mut count, g, &replacement, true);
            let proposed = evaluate(model, &count);
            if proposed.better_than(&score) {
                plan[s] = replacement;
                score = proposed;
            } else {
                adjust(&mut count, g, &replacement, false);
                adjust(&mut count, g, &plan[s], true);
            }
        }
    }
    score
}

pub(super) fn independent_baseline(model: &Model) -> Plan {
    // A reproducible uncoordinated baseline exposes the effect of de-duplication.
    let baseline_prices: Vec<_> = model
        .point_request
        .iter()
        .map(|&r| 1.0 / model.requests[r].points as f64)
        .collect();
    model
        .graphs
        .iter()
        .map(|g| longest_path(g, &rewards(g, &baseline_prices)).1)
        .collect()
}

pub(super) fn optimize(model: &Model, config: &Config) -> Result<(Vec<Candidate>, Vec<Iteration>)> {
    let priorities = priorities(model);
    let mut best: Plan = model.graphs.iter().map(|_| Vec::new()).collect();
    let mut best_score = evaluate(model, &vec![0; model.point_request.len()]);
    let baseline = independent_baseline(model);
    let baseline_score = evaluate(model, &counts(model, &baseline));
    let mut candidates = vec![Candidate {
        name: "independent_paths_baseline".into(),
        plan: baseline.clone(),
    }];
    if baseline_score.better_than(&best_score) {
        best = baseline;
        best_score = baseline_score;
    }
    let mut history = Vec::new();
    for &priority in &priorities {
        let current = counts(model, &best);
        // Freeze the choice of protected higher-tier acquisition requests during this stage.
        // A scalar stage can then use modest, numerically safe penalty weights.
        let weights: Vec<_> = model
            .requests
            .iter()
            .enumerate()
            .map(|(r, request)| {
                let job_priority = model.jobs[request.job].acquisition.priority;
                if job_priority > priority && complete(model, &current, r) {
                    model.requests.len() as f64 + 1.0
                } else if job_priority == priority {
                    1.0
                } else {
                    0.0
                }
            })
            .collect();
        let base: Vec<_> = model
            .point_request
            .iter()
            .map(|&r| weights[r] / model.requests[r].points as f64)
            .collect();
        let mut prices = base.clone();
        let mut best_lower = objective(model, &current, &weights);
        let mut best_upper = f64::INFINITY;
        for iteration in 0..config.iterations_per_priority {
            let started = Instant::now();
            resources::check_memory()?;
            let solutions = parallel::map(
                model.graphs.iter().collect(),
                config.workers,
                |g| Ok(longest_path(g, &rewards(g, &prices))),
                |_, _| {},
            )?;
            let path_value: f64 = solutions.iter().map(|s| s.0).sum();
            let raw: Plan = solutions.into_iter().map(|s| s.1).collect();
            let count = counts(model, &raw);
            let mut z = vec![false; model.requests.len()];
            let mut constant = 0.0;
            for (r, request) in model.requests.iter().enumerate() {
                let reduced = weights[r]
                    - prices[request.start..request.start + request.points]
                        .iter()
                        .sum::<f64>();
                z[r] = weights[r] > 0.0 && reduced >= 0.0;
                constant += reduced.max(0.0);
            }
            let upper = path_value + constant;
            best_upper = best_upper.min(upper);
            best_lower = best_lower.max(objective(model, &count, &weights));
            let raw_score = evaluate(model, &count);
            if raw_score.better_than(&best_score) {
                best = raw.clone();
                best_score = raw_score;
            }
            let mut repaired = raw;
            let repaired_score = repair(
                model,
                &mut repaired,
                &prices,
                &base,
                config.repair_sweeps,
                iteration,
            );
            best_lower = best_lower.max(objective(model, &counts(model, &repaired), &weights));
            if repaired_score.better_than(&best_score) {
                best = repaired;
            }
            // Also improve the incumbent, whose higher-priority coverage may be
            // absent in the current relaxed paths.
            best_score = repair(
                model,
                &mut best,
                &prices,
                &base,
                config.repair_sweeps,
                iteration,
            );
            best_lower = best_lower.max(objective(model, &counts(model, &best), &weights));
            if best_lower > best_upper + 1e-7 * best_upper.abs().max(1.0) {
                return Err("Sampled dual bound is below a feasible sampled solution".into());
            }
            history.push(Iteration {
                priority,
                iteration,
                sampled_dual_upper: best_upper,
                sampled_primal_lower: best_lower,
                best_sampled_score: best_score.clone(),
            });
            eprintln!("Priority {priority}, iteration {}: sampled met deadlines {:?}; stage bound {:.3} / {:.3}",iteration+1,best_score.complete_requests,best_lower,best_upper);
            let norm: f64 = count
                .iter()
                .enumerate()
                .map(|(p, &n)| {
                    let g = n as f64 - f64::from(z[model.point_request[p]]);
                    g * g
                })
                .sum();
            if norm <= 1e-20 || best_upper - best_lower <= 1e-8 * best_upper.abs().max(1.0) {
                break;
            }
            let step = 1.5 * (upper - best_lower).max(0.0) / norm;
            for (p, price) in prices.iter_mut().enumerate() {
                let gradient = count[p] as f64 - f64::from(z[model.point_request[p]]);
                *price = (*price - step * gradient).max(0.0);
                if !price.is_finite() {
                    return Err("Non-finite dual multiplier".into());
                }
            }
            // Honor the shared cooperative CPU-duty setting between iterations.
            let duty = resources::duty_percent() as f64 / 100.0;
            std::thread::sleep(std::time::Duration::from_secs_f64(
                (started.elapsed().as_secs_f64() * (1.0 - duty) / duty).min(2.0),
            ));
        }
        candidates.push(Candidate {
            name: format!("after_priority_{priority}"),
            plan: best.clone(),
        });
    }
    Ok((candidates, history))
}
