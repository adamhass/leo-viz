use super::*;
use crate::{
    geometry::Atlas,
    workload::{Acquisition, AoiRef, Pipeline},
};

pub(super) fn graph(
    actions: Vec<super::super::Action>,
    coverage: Vec<Vec<u32>>,
    transitions: &[(usize, usize)],
    starts: &[usize],
) -> Graph {
    let n = actions.len();
    let source = 2 * n;
    let sink = source + 1;
    let mut edges = vec![vec![]; 2 * n + 2];
    edges[source].push(sink as u32);
    for &a in starts {
        edges[source].push((2 * a) as u32);
    }
    for a in 0..n {
        edges[2 * a].push((2 * a + 1) as u32);
        edges[2 * a + 1].push(sink as u32);
    }
    for &(a, b) in transitions {
        edges[2 * a + 1].push((2 * b) as u32);
    }
    let order = input::topological(&edges).unwrap();
    Graph {
        satellite: 0,
        actions,
        coverage,
        edges,
        order,
        source,
        sink,
    }
}
pub(super) fn action(tick: u32) -> super::super::Action {
    super::super::Action::at_tick(tick, 0, 1.0)
}

#[test]
fn dag_oracle_matches_exhaustive_paths_and_skips_negative_rewards() {
    let g = graph(
        vec![action(0), action(1), action(2), action(3)],
        vec![vec![]; 4],
        &[(0, 1), (0, 2), (1, 3), (2, 3)],
        &[0, 2],
    );
    for bits in 0..256 {
        let r: Vec<_> = (0..4)
            .map(|i| ((bits >> (2 * i)) & 3) as f64 - 1.0)
            .collect();
        let paths = [
            vec![],
            vec![0],
            vec![2],
            vec![0, 1],
            vec![0, 2],
            vec![2, 3],
            vec![0, 1, 3],
            vec![0, 2, 3],
        ];
        let exact = paths
            .iter()
            .map(|p| p.iter().map(|&a| r[a].max(0.)).sum::<f64>())
            .fold(0., f64::max);
        let (value, path) = search::longest_path(&g, &r);
        assert_eq!(value, exact);
        assert!(path.iter().all(|&a| r[a as usize] > 0.));
        verify::validate_path(&g, &path).unwrap();
    }
}
#[test]
fn unreachable_capture_and_incompatible_choices_are_rejected() {
    let g = graph(
        vec![action(0), action(0), action(2)],
        vec![vec![]; 3],
        &[(0, 2)],
        &[0],
    );
    assert_eq!(search::longest_path(&g, &[1., 100., 2.]), (3., vec![0, 2]));
    assert!(verify::validate_path(&g, &[1]).is_err());
    assert!(verify::validate_path(&g, &[0, 1]).is_err());
    assert!(verify::validate_path(&g, &[0, 0]).is_err());
}
#[test]
fn graph_cycles_are_rejected() {
    assert!(input::topological(&[vec![1], vec![0]]).is_err());
}
#[test]
fn priority_comparison_never_trades_one_high_deadline_for_many_low_deadlines() {
    let high = Score {
        complete_requests: vec![1, 0],
        fractional_requests: 1.,
    };
    let low = Score {
        complete_requests: vec![0, 100000],
        fractional_requests: 100000.,
    };
    assert!(high.better_than(&low));
    assert!(!low.better_than(&high));
}

fn job(id: &str, priority: u16) -> Job {
    Job {
        aoi: AoiRef {
            source: "test".into(),
            id: id.into(),
            area_km2: 1.,
            longitude: 0.,
            latitude: 0.,
            geometry_hash: 0,
        },
        pipeline: Pipeline::Map,
        acquisition: Acquisition {
            priority,
            ..Default::default()
        },
        cadence_hours: 4.,
        samples: 1,
        map_bytes_per_km2: 1.,
        window_bytes_per_km2: 1.,
    }
}
pub(super) fn tiny_model() -> Model {
    let jobs = vec![job("a", 5), job("b", 4)];
    let atlas = Atlas::new(&[], &[], 2., 2., |_| true).unwrap();
    let config = super::super::Config {
        preset: PathBuf::new(),
        output: PathBuf::new(),
        epoch_unix_s: 0.,
        options: Default::default(),
        workers: 1,
        first_satellite: 0,
        satellite_count: 2,
        only_actions_serving_due_deadlines: false,
            strips: None,
    };
    let manifest = super::super::Summary {
        schema_version: 2,
        status: "complete".into(),
        config,
        preset_hash_fnv1a64: 0,
        job_count: 2,
        jobs_with_due_deadlines: 2,
        due_observations: 2,
        jobs_with_no_due_deadline: 0,
        jobs_with_candidates: 2,
        cadence_job_counts: Default::default(),
        modes: vec![],
        slew_rate_deg_s: 1.,
        total_actions: 3,
        total_nodes: 0,
        total_edges: 0,
        total_crop_records: 0,
        graph_and_action_bytes: 0,
        observed_process_memory_bytes: 0,
        elapsed_s: 0.,
        satellites: vec![],
        logical_cpus: 1,
        effective_worker_limit: 1,
        duty_percent: 100,
        constellation: leo_sim::worldview::Fleet::WorldView60.shell(),
        assumptions: vec![],
    };
    Model {
        jobs,
        included_jobs: vec![true; 2],
        demands: vec![
            Demand {
                start: 0,
                points_per_slot: 1,
                slots: 1,
            },
            Demand {
                start: 1,
                points_per_slot: 1,
                slots: 1,
            },
        ],
        requests: vec![
            AcquisitionRequest {
                job: 0,
                start: 0,
                points: 1,
            },
            AcquisitionRequest {
                job: 1,
                start: 1,
                points: 1,
            },
        ],
        point_request: vec![0, 1],
        point_areas: vec![1.0, 1.0],
        graphs: vec![
            graph(
                vec![action(0), action(0)],
                vec![vec![0], vec![1]],
                &[],
                &[0, 1],
            ),
            graph(vec![action(1)], vec![vec![0]], &[], &[0]),
        ],
        atlas,
        manifest,
    }
}
#[test]
fn coordination_recovers_job_with_only_one_satellite_opportunity() {
    let model = tiny_model();
    let (candidates, history) = search::optimize(
        &model,
        &Config {
            iterations_per_priority: 3,
            workers: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(candidates.iter().any(
        |c| search::evaluate(&model, &search::counts(&model, &c.plan)).complete_requests == [1, 1]
    ));
    assert!(history
        .iter()
        .all(|i| i.sampled_primal_lower <= i.sampled_dual_upper + 1e-8));
}
#[test]
fn each_deadline_requires_its_own_coverage_and_earns_independent_credit() {
    let mut model = tiny_model();
    model.demands[0] = Demand {
        start: 0,
        points_per_slot: 2,
        slots: 2,
    };
    model.demands[1] = Demand {
        start: 4,
        points_per_slot: 1,
        slots: 1,
    };
    (model.requests, model.point_request) = request_index(&model.demands);
    assert_eq!(
        search::evaluate(&model, &[1, 1, 1, 0, 1]).complete_requests,
        [1, 1]
    );
    assert_eq!(
        search::evaluate(&model, &[1, 1, 1, 1, 1]).complete_requests,
        [2, 1]
    );
}

#[test]
fn an_impossible_deadline_does_not_suppress_later_acquisitions_for_the_same_job() {
    let mut model = tiny_model();
    model.jobs.truncate(1);
    model.included_jobs.truncate(1);
    model.demands = vec![Demand {
        start: 0,
        points_per_slot: 1,
        slots: 3,
    }];
    (model.requests, model.point_request) = request_index(&model.demands);
    // The first window has no opportunity; the next two can both be met.
    model.graphs = vec![graph(
        vec![action(1), action(2)],
        vec![vec![1], vec![2]],
        &[(0, 1)],
        &[0],
    )];
    let (candidates, history) = search::optimize(
        &model,
        &Config {
            workers: 1,
            iterations_per_priority: 3,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(candidates.iter().any(
        |c| search::evaluate(&model, &search::counts(&model, &c.plan)).complete_requests == [2]
    ));
    assert!(history.iter().any(|h| h.sampled_primal_lower == 2.0));
    assert_eq!(model.jobs.len(), 1); // The ongoing job was never removed.
}

#[test]
fn skipped_opportunities_and_repeated_captures_do_not_count_as_deadlines() {
    let mut model = tiny_model();
    model.jobs.truncate(1);
    model.included_jobs.truncate(1);
    model.demands.truncate(1);
    (model.requests, model.point_request) = request_index(&model.demands);
    model.graphs = vec![graph(
        vec![action(0), action(1)],
        vec![vec![0], vec![0]],
        &[(0, 1)],
        &[0, 1],
    )];
    assert_eq!(model.requests.len(), 1);
    assert_eq!(
        search::evaluate(&model, &search::counts(&model, &vec![vec![1]])).complete_requests,
        [1]
    );
    assert_eq!(
        search::evaluate(&model, &search::counts(&model, &vec![vec![0, 1]])).complete_requests,
        [1]
    );
}

#[test]
fn deadlines_follow_phase_and_cadence_and_future_windows_are_pending() {
    let mut j = job("timing", 1);
    assert_eq!(deadline_counts(&j, 14400.0 - 0.001), (0, true));
    assert_eq!(deadline_counts(&j, 14400.0), (1, false));
    assert_eq!(deadline_counts(&j, 86400.0), (6, false));
    j.acquisition.phase_hours = 2.0;
    assert_eq!(deadline_counts(&j, 3600.0), (0, false));
    assert_eq!(deadline_counts(&j, 4.0 * 3600.0), (0, true));
    assert_eq!(deadline_counts(&j, 6.0 * 3600.0), (1, false));
    assert_eq!(deadline_counts(&j, 24.0 * 3600.0), (5, true));
    j.acquisition.phase_hours = 0.0;
    for cadence in [168.0, 720.0] {
        j.cadence_hours = cadence;
        assert_eq!(deadline_counts(&j, 86400.0), (0, true));
        assert_eq!(deadline_counts(&j, cadence * 3600.0), (1, false));
    }
}

#[test]
fn a_horizon_without_due_deadlines_is_not_a_failure() {
    let mut model = tiny_model();
    for j in &mut model.jobs {
        j.cadence_hours = 720.0;
    }
    model.demands = vec![
        Demand {
            start: 0,
            points_per_slot: 0,
            slots: 0
        };
        2
    ];
    (model.requests, model.point_request) = request_index(&model.demands);
    for g in &mut model.graphs {
        for c in &mut g.coverage {
            c.clear();
        }
    }
    let (candidates, history) = search::optimize(
        &model,
        &Config {
            workers: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(history.is_empty());
    assert_eq!(candidates.len(), 1);
    assert!(candidates[0].plan.iter().all(Vec::is_empty));
    assert_eq!(miss_rate(0, 0), None);
    assert_eq!(miss_rate(6, 5), Some(100.0 / 6.0));
}

#[test]
fn cadence_filter_keeps_original_indices_and_excludes_other_deadlines() {
    let mut model = tiny_model();
    model.jobs = [4.0, 24.0, 168.0, 720.0]
        .into_iter()
        .map(|cadence| {
            let mut j = job("scope", 1);
            j.cadence_hours = cadence;
            j
        })
        .collect();
    assert_eq!(input::job_scope(&model.jobs, None).unwrap(), [true; 4]);
    model.included_jobs = input::job_scope(&model.jobs, Some(24.0)).unwrap();
    assert_eq!(model.included_jobs, [false, true, false, false]);
    let groups = verify::cadence_summary(&model, 86400.0);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["cadence_hours"], 24.0);
    assert_eq!(groups[0]["jobs"], 1);
    assert_eq!(groups[0]["due_acquisition_deadlines"], 1);
    assert_eq!(groups[0]["pending_acquisition_deadlines"], 0);
    // A matched future request remains pending; excluded requests are absent.
    model.included_jobs = input::job_scope(&model.jobs, Some(168.0)).unwrap();
    let groups = verify::cadence_summary(&model, 86400.0);
    assert_eq!(groups[0]["due_acquisition_deadlines"], 0);
    assert_eq!(groups[0]["pending_acquisition_deadlines"], 1);
    for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY, 12.0] {
        assert!(input::job_scope(&model.jobs, Some(invalid)).is_err());
    }
}

#[test]
fn standalone_baseline_is_the_same_candidate_used_by_priority_search() {
    let model = tiny_model();
    let config = Config {
        iterations_per_priority: 1,
        repair_sweeps: 0,
        ..Default::default()
    };
    let (candidates, _) = search::optimize(&model, &config).unwrap();
    assert_eq!(candidates[0].name, "independent_paths_baseline");
    assert_eq!(search::independent_baseline(&model), candidates[0].plan);
}
