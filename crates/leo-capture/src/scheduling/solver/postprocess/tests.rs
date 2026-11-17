use super::super::{
    exact::{Crop, Geometry},
    tests::{action, graph, tiny_model},
};
use super::*;
use crate::{geometry::Shape, workload::RewardMode};
use geo::{MultiPolygon, Rect};

fn rect(left: f64, right: f64) -> Shape {
    MultiPolygon::new(vec![Rect::new((left, 0.), (right, 1.)).to_polygon()])
}

fn fixture() -> (Model, Geometry, Plan) {
    let mut model = tiny_model();
    model.atlas.job_areas_km2 = vec![1., 1.];
    for job in &mut model.jobs {
        job.acquisition.reward_mode = Some(RewardMode::CompleteObservation);
    }
    // A -> redundant R -> C; missed mosaic can use A -> M -> N -> C.
    model.graphs = vec![graph(
        vec![action(0), action(1), action(1), action(2), action(3)],
        vec![vec![0], vec![0], vec![1], vec![1], vec![0]],
        &[(0, 1), (1, 4), (0, 2), (2, 3), (3, 4)],
        &[0],
    )];
    let crops = vec![
        Crop {
            request: 0,
            patch: 0,
            shape: rect(0., 0.5),
        },
        Crop {
            request: 0,
            patch: 0,
            shape: rect(0., 0.5),
        },
        Crop {
            request: 1,
            patch: 0,
            shape: rect(0., 0.5),
        },
        Crop {
            request: 1,
            patch: 0,
            shape: rect(0.5, 1.),
        },
        Crop {
            request: 0,
            patch: 0,
            shape: rect(0.5, 1.),
        },
    ];
    (
        model,
        Geometry {
            crops,
            actions: vec![vec![vec![0], vec![1], vec![2], vec![3], vec![4]]],
            prepared_s: 0.,
        },
        vec![vec![0, 1, 4]],
    )
}

#[test]
fn removes_a_repeat_and_refills_a_chain_to_complete_a_missed_mosaic() {
    let (model, geometry, original) = fixture();
    let (plan, report) = run(
        &model,
        &geometry,
        &Default::default(),
        &original,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(plan, vec![vec![0, 2, 3, 4]]);
    assert_eq!(report["pruned_before_refill"], 1);
    assert_eq!(report["inserted_captures"], 2);
    assert_eq!(
        report["target_requests"],
        serde_json::json!([{"job":1,"slot":0}])
    );
    let reward = reward::RewardModel::default();
    let mut exact = exact::Evaluator::new(&model, &geometry, &reward);
    // The first inserted half has zero true reward; the pair earns full reward.
    assert_eq!(exact.state(&original).unwrap().score, 32.);
    assert_eq!(exact.state(&plan).unwrap().score, 48.);
}

#[test]
fn insertion_limit_keeps_partial_progress_and_reports_the_limit() {
    let (model, geometry, original) = fixture();
    let (plan, report) = run(
        &model,
        &geometry,
        &Default::default(),
        &original,
        &Options { max_insertions: 1 },
    )
    .unwrap();
    assert_eq!(report["inserted_captures"], 1);
    assert_eq!(report["stop_reason"], "insertion_limit");
    assert_eq!(plan[0].len(), 3);
    let reward = reward::RewardModel::default();
    let ledger = coverage::Ledger::new(&model, &geometry, &reward, &plan).unwrap();
    assert_eq!(ledger.requests[1].area, 0.5);
    assert!(!ledger.requests[1].complete);
}

#[test]
fn globally_prunes_one_duplicate_but_preserves_zero_reward_partial_coverage() {
    let (mut model, mut geometry, _) = fixture();
    model.graphs = vec![
        graph(
            vec![action(0), action(1)],
            vec![vec![0], vec![1]],
            &[(0, 1)],
            &[0],
        ),
        graph(vec![action(2)], vec![vec![0]], &[], &[0]),
    ];
    geometry.crops = vec![
        Crop {
            request: 0,
            patch: 0,
            shape: rect(0., 1.),
        },
        Crop {
            request: 1,
            patch: 0,
            shape: rect(0., 0.5),
        },
        Crop {
            request: 0,
            patch: 0,
            shape: rect(0., 1.),
        },
    ];
    geometry.actions = vec![vec![vec![0], vec![1]], vec![vec![2]]];
    let (plan, report) = run(
        &model,
        &geometry,
        &Default::default(),
        &vec![vec![0, 1], vec![0]],
        &Default::default(),
    )
    .unwrap();
    assert_eq!(plan, vec![vec![0, 1], vec![]]);
    assert_eq!(report["pruned_before_refill"], 1);
    assert_eq!(report["inserted_captures"], 0);
}

#[test]
fn gap_labels_match_exhaustive_insertion_feasibility_including_wait_hubs() {
    let mut g = graph(
        (0..5).map(action).collect(),
        vec![vec![]; 5],
        &[(0, 2), (1, 2), (0, 3), (1, 3), (2, 4)],
        &[0, 1],
    );
    let hub = g.edges.len();
    g.edges.push(vec![8]);
    g.edges[7].push(hub as u32);
    g.order = input::topological(&g.edges).unwrap();
    for mask in 0..32 {
        let path: Vec<_> = (0..5u32).filter(|a| mask & (1 << a) != 0).collect();
        if verify::validate_path(&g, &path).is_err() {
            continue;
        }
        let gaps = gaps::Gaps::new(&g, &path);
        for a in 0..5u32 {
            let expected = (0..=path.len()).find(|&i| {
                let mut candidate = path.clone();
                candidate.insert(i, a);
                verify::validate_path(&g, &candidate).is_ok()
            });
            assert_eq!(
                gaps.positions[a as usize], expected,
                "path {path:?}, action {a}"
            );
        }
    }
}

#[test]
fn refills_polygon_residual_even_when_all_sampling_witnesses_are_already_covered() {
    let (mut model, mut geometry, _) = fixture();
    model.graphs = vec![graph(
        vec![action(0), action(1), action(2)],
        vec![vec![0], vec![0], vec![1]],
        &[(0, 1), (1, 2)],
        &[0],
    )];
    geometry.crops = vec![
        Crop {
            request: 0,
            patch: 0,
            shape: rect(0., 0.5),
        },
        Crop {
            request: 0,
            patch: 0,
            shape: rect(0.5, 1.),
        },
        Crop {
            request: 1,
            patch: 0,
            shape: rect(0., 1.),
        },
    ];
    geometry.actions = vec![vec![vec![0], vec![1], vec![2]]];
    let (plan, report) = run(
        &model,
        &geometry,
        &Default::default(),
        &vec![vec![0, 2]],
        &Default::default(),
    )
    .unwrap();
    assert_eq!(plan, vec![vec![0, 1, 2]]);
    assert_eq!(
        report["target_requests"],
        serde_json::json!([{"job":0,"slot":0}])
    );
    assert_eq!(report["cached_true_reward"], 48.);
}

#[test]
fn separate_deadlines_and_distinct_chart_patches_are_not_duplicates() {
    let (mut model, mut geometry, _) = fixture();
    model.requests[1].job = 0;
    model.demands[0].slots = 2;
    model.requests[1].start = 1;
    model.graphs = vec![graph(
        vec![action(0), action(1), action(2)],
        vec![vec![0], vec![0], vec![1]],
        &[(0, 1), (1, 2)],
        &[0],
    )];
    geometry.crops = vec![
        Crop {
            request: 0,
            patch: 0,
            shape: rect(0., 0.5),
        },
        Crop {
            request: 0,
            patch: 1,
            shape: rect(0., 0.5),
        },
        Crop {
            request: 1,
            patch: 0,
            shape: rect(0., 1.),
        },
    ];
    geometry.actions = vec![vec![vec![0], vec![1], vec![2]]];
    let original = vec![vec![0, 1, 2]];
    let (plan, report) = run(
        &model,
        &geometry,
        &Default::default(),
        &original,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(plan, original);
    assert_eq!(report["target_requests"], serde_json::json!([]));
}

#[test]
fn postprocessing_is_opt_in_and_validates_its_budget() {
    let c: Config = serde_json::from_str("{}").unwrap();
    assert!(c.postprocess.is_none());
    let c: Config = serde_json::from_str(r#"{"postprocess":{}}"#).unwrap();
    assert_eq!(c.postprocess.unwrap().max_insertions, 10000);
    assert!(Options { max_insertions: 0 }.validate().is_err());
}

#[test]
fn refill_reprices_coverage_globally_and_does_not_insert_a_second_satellite_repeat() {
    let (mut model, mut geometry, _) = fixture();
    model.graphs = vec![
        graph(vec![action(0)], vec![vec![1]], &[], &[0]),
        graph(vec![action(1)], vec![vec![1]], &[], &[0]),
    ];
    geometry.crops = vec![
        Crop {
            request: 1,
            patch: 0,
            shape: rect(0., 1.),
        },
        Crop {
            request: 1,
            patch: 0,
            shape: rect(0., 1.),
        },
    ];
    geometry.actions = vec![vec![vec![0]], vec![vec![1]]];
    let (plan, report) = run(
        &model,
        &geometry,
        &Default::default(),
        &vec![vec![], vec![]],
        &Default::default(),
    )
    .unwrap();
    assert_eq!(plan, vec![vec![0], vec![]]);
    assert_eq!(report["inserted_captures"], 1);
    assert_eq!(report["cached_true_reward"], 16.);
}
