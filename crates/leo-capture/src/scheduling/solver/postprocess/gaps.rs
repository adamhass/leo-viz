//! Linear graph passes locate insertions between consecutive retained captures.
//! Waiting hubs are ordinary graph nodes: no direct A->M or M->C edge is assumed.
use super::super::Graph;

pub(super) struct Gaps {
    /// Insertion position in the retained path, or None if no gap admits it.
    pub positions: Vec<Option<usize>>,
}

impl Gaps {
    pub fn new(graph: &Graph, retained: &[u32]) -> Self {
        let mut anchors = vec![0; graph.actions.len()];
        for (i, &a) in retained.iter().enumerate() {
            anchors[a as usize] = i + 1;
        }
        let mut forward = vec![0; graph.edges.len()];
        forward[graph.source] = 1;
        for &node in &graph.order {
            if node < 2 * anchors.len() && node % 2 == 1 && anchors[node / 2] > 0 {
                forward[node] = anchors[node / 2] + 1;
            }
            for &next in &graph.edges[node] {
                forward[next as usize] = forward[next as usize].max(forward[node]);
            }
        }
        let mut backward = vec![usize::MAX; graph.edges.len()];
        backward[graph.sink] = retained.len() + 1;
        for &node in graph.order.iter().rev() {
            for &next in &graph.edges[node] {
                backward[node] = backward[node].min(backward[next as usize]);
            }
            if node < 2 * anchors.len() && node % 2 == 0 && anchors[node / 2] > 0 {
                backward[node] = anchors[node / 2];
            }
        }
        let positions = (0..graph.actions.len())
            .map(|a| {
                (anchors[a] == 0 && forward[2 * a] > 0 && forward[2 * a] == backward[2 * a + 1])
                    .then(|| forward[2 * a] - 1)
            })
            .collect();
        Self { positions }
    }
}
