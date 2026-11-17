use super::*;
use crate::scheduling::solver::{
    search,
    tests::{action, graph},
    verify,
};

fn score(g: &Graph, path: &[u32], weights: &[f64]) -> f64 {
    let mut points = std::collections::BTreeSet::new();
    for &a in path {
        points.extend(g.coverage[a as usize].iter().copied());
    }
    points.into_iter().map(|p| weights[p as usize]).sum()
}

#[test]
fn fresh_coverage_beats_a_repeated_high_value_observation() {
    let g = graph(
        (0..5).map(action).collect(),
        vec![vec![0], vec![0], vec![0], vec![1], vec![2]],
        &[(0, 1), (1, 2), (3, 4)],
        &[0, 3],
    );
    let weights = [10.0, 6.0, 6.0];
    assert_eq!(
        search::longest_path(&g, &[10., 10., 10., 6., 6.]),
        (30., vec![0, 1, 2])
    );
    for limit in [1, 2, 4, 16] {
        let (value, path) = Index::new(&g).search(&g, &weights, limit);
        assert_eq!((value, &path), (12.0, &vec![3, 4]));
        verify::validate_path(&g, &path).unwrap();
    }
}

#[test]
fn partial_overlap_earns_only_new_coverage_and_full_repeats_are_skipped() {
    let g = graph(
        (0..4).map(action).collect(),
        vec![vec![0, 1], vec![1, 2], vec![0, 1], vec![2]],
        &[(0, 1), (1, 2), (2, 3)],
        &[0],
    );
    let (value, path) = Index::new(&g).search(&g, &[2., 3., 5.], 4);
    assert_eq!(value, 10.0);
    assert_eq!(path.len(), 2);
    assert_eq!(score(&g, &path, &[2., 3., 5.]), 10.0);
    verify::validate_path(&g, &path).unwrap();
}

#[test]
fn different_histories_survive_a_merge_to_collect_complementary_coverage() {
    // A single best prefix keeps witness 0 (10), missing the better prefix
    // witness 1 (9), followed by witness 0 (10) after the paths merge.
    let mut g = graph(
        (0..3).map(action).collect(),
        vec![vec![0], vec![1], vec![0]],
        &[],
        &[0, 1],
    );
    let hub = g.edges.len();
    g.edges.push(vec![4]);
    g.edges[1].push(hub as u32);
    g.edges[3].push(hub as u32);
    g.order = crate::scheduling::solver::input::topological(&g.edges).unwrap();
    let index = Index::new(&g);
    assert_eq!(index.search(&g, &[10., 9.], 1).0, 10.0);
    assert_eq!(index.search(&g, &[10., 9.], 4), (19.0, vec![1, 2]));
}

#[test]
fn later_superset_removes_redundant_earlier_capture_without_losing_coverage() {
    let g = graph(
        vec![action(0), action(1)],
        vec![vec![0], vec![0, 1]],
        &[(0, 1)],
        &[0],
    );
    for limit in [1, 4] {
        let (value, path) = Index::new(&g).search(&g, &[2., 3.], limit);
        assert_eq!((value, &path), (5.0, &vec![1]));
        verify::validate_path(&g, &path).unwrap();
    }
}

#[test]
fn zero_prices_other_deadlines_shared_jobs_and_unreachable_actions() {
    // IDs are independent obligations, even for the same physical location:
    // another job or deadline must still be credited. ID 1000 exercises compact
    // indexing; the high-valued unreachable action must never be selected.
    let g = graph(
        (0..5).map(action).collect(),
        vec![vec![1], vec![1, 1000], vec![2], vec![0], vec![3]],
        &[(0, 1), (1, 2), (2, 3)],
        &[0],
    );
    let mut weights = vec![0.; 1001];
    weights[1] = 4.;
    weights[1000] = 3.;
    weights[2] = 5.;
    weights[3] = 100.;
    let index = Index::new(&g);
    let (value, path) = index.search(&g, &weights, 4);
    assert_eq!((value, &path), (12., &vec![1, 2]));
    verify::validate_path(&g, &path).unwrap();
    assert_eq!(index.search(&g, &vec![0.; 1001], 4), (0., vec![]));
}

#[test]
fn matches_exhaustive_unique_coverage_on_a_small_dag() {
    let paths = [
        vec![],
        vec![0],
        vec![1],
        vec![0, 2],
        vec![1, 2],
        vec![0, 3],
        vec![1, 3],
        vec![0, 2, 4],
        vec![1, 2, 4],
        vec![0, 3, 4],
        vec![1, 3, 4],
    ];
    for bits in 0..256u32 {
        let weights: Vec<_> = (0..4).map(|i| ((bits >> (2 * i)) & 3) as f64).collect();
        let g = graph(
            (0..5).map(action).collect(),
            vec![vec![0], vec![1], vec![0, 2], vec![1, 3], vec![0, 3]],
            &[(0, 2), (1, 2), (0, 3), (1, 3), (2, 4), (3, 4)],
            &[0, 1],
        );
        let expected = paths
            .iter()
            .map(|p| score(&g, p, &weights))
            .fold(0., f64::max);
        let (value, path) = Index::new(&g).search(&g, &weights, 16);
        assert_eq!(value, expected);
        assert_eq!(value, score(&g, &path, &weights));
        verify::validate_path(&g, &path).unwrap();
        for omit in 0..path.len() {
            let mut reduced = path.clone();
            reduced.remove(omit);
            assert!(score(&g, &reduced, &weights) < value);
        }
    }
}

#[test]
fn frontier_limit_and_hash_collisions_preserve_distinct_coverage() {
    let mut frontier = Vec::new();
    for p in 0..8 {
        insert(
            &mut frontier,
            Label {
                value: p as f64,
                captures: 1,
                coverage: Rc::new(Coverage {
                    bits: vec![1 << p],
                    fingerprint: 0,
                }),
                trace: None,
            },
            4,
        );
        assert!(frontier.len() <= 4);
    }
    assert_eq!(
        frontier.iter().map(|l| l.value).collect::<Vec<_>>(),
        vec![7., 6., 5., 4.]
    );
}
