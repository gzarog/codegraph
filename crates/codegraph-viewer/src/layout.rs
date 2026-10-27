//! Layered layout for the module map: modules that depend on others sit
//! above what they depend on. Cycles are broken by DFS back edges, layers by
//! longest path, and order within a layer by a few barycenter sweeps.

use std::collections::HashMap;

pub const NODE_H: f64 = 44.0;
const GAP_X: f64 = 28.0;
const GAP_Y: f64 = 70.0;
const CHAR_W: f64 = 7.2;
const PAD_X: f64 = 26.0;

#[derive(Debug, Clone)]
pub struct Placed {
    pub x: f64,
    pub y: f64,
    pub w: f64,
}

#[derive(Debug, Clone)]
pub struct Layout {
    pub nodes: Vec<Placed>,
    pub width: f64,
    pub height: f64,
}

pub fn node_width(label: &str) -> f64 {
    (label.chars().count() as f64 * CHAR_W + PAD_X).clamp(70.0, 260.0)
}

/// `edges` are `(from, to)` indices: `from` depends on `to`.
pub fn layered(labels: &[String], edges: &[(usize, usize)]) -> Layout {
    let n = labels.len();
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(a, b) in edges {
        if a != b && a < n && b < n && !out[a].contains(&b) {
            out[a].push(b);
        }
    }
    // Break cycles: drop DFS back edges.
    let mut state = vec![0u8; n];
    let mut dag: Vec<Vec<usize>> = vec![Vec::new(); n];
    fn dfs(v: usize, out: &[Vec<usize>], state: &mut [u8], dag: &mut [Vec<usize>]) {
        state[v] = 1;
        for &w in &out[v] {
            match state[w] {
                0 => {
                    dag[v].push(w);
                    dfs(w, out, state, dag);
                }
                2 => dag[v].push(w),
                _ => {} // back edge
            }
        }
        state[v] = 2;
    }
    for v in 0..n {
        if state[v] == 0 {
            dfs(v, &out, &mut state, &mut dag);
        }
    }
    // Longest path from the sources gives each node's layer.
    let mut indeg = vec![0usize; n];
    for &w in dag.iter().flatten() {
        indeg[w] += 1;
    }
    let mut rank = vec![0usize; n];
    let mut queue: Vec<usize> = (0..n).filter(|&v| indeg[v] == 0).collect();
    let mut i = 0;
    while i < queue.len() {
        let v = queue[i];
        i += 1;
        for &w in &dag[v] {
            rank[w] = rank[w].max(rank[v] + 1);
            indeg[w] -= 1;
            if indeg[w] == 0 {
                queue.push(w);
            }
        }
    }
    let layers_n = rank.iter().copied().max().map_or(0, |m| m + 1);
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); layers_n];
    for v in 0..n {
        layers[rank[v]].push(v);
    }
    for l in &mut layers {
        l.sort_by(|a, b| labels[*a].cmp(&labels[*b]));
    }
    // Barycenter sweeps, down then up.
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (v, outs) in dag.iter().enumerate() {
        for &w in outs {
            preds[w].push(v);
        }
    }
    let position = |layers: &[Vec<usize>]| -> HashMap<usize, f64> {
        let mut pos = HashMap::new();
        for l in layers {
            for (i, &v) in l.iter().enumerate() {
                pos.insert(v, i as f64);
            }
        }
        pos
    };
    for sweep in 0..4 {
        let pos = position(&layers);
        let down = sweep % 2 == 0;
        for layer in layers.iter_mut() {
            let neighbors = |v: usize| if down { &preds[v] } else { &dag[v] };
            let mut keyed: Vec<(f64, usize)> = layer
                .iter()
                .map(|&v| {
                    let ns = neighbors(v);
                    let key = if ns.is_empty() {
                        pos[&v]
                    } else {
                        ns.iter().map(|w| pos[w]).sum::<f64>() / ns.len() as f64
                    };
                    (key, v)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            *layer = keyed.into_iter().map(|(_, v)| v).collect();
        }
    }
    // Coordinates: layers centered on the widest.
    let widths: Vec<f64> = labels.iter().map(|l| node_width(l)).collect();
    let row_width = |l: &Vec<usize>| -> f64 {
        l.iter().map(|&v| widths[v]).sum::<f64>() + GAP_X * (l.len().saturating_sub(1)) as f64
    };
    let width = layers.iter().map(row_width).fold(0.0, f64::max) + 2.0 * GAP_X;
    let mut nodes = vec![
        Placed {
            x: 0.0,
            y: 0.0,
            w: 0.0
        };
        n
    ];
    for (li, l) in layers.iter().enumerate() {
        let mut x = (width - row_width(l)) / 2.0;
        for &v in l {
            nodes[v] = Placed {
                x,
                y: GAP_X + li as f64 * (NODE_H + GAP_Y),
                w: widths[v],
            };
            x += widths[v] + GAP_X;
        }
    }
    let height = 2.0 * GAP_X + layers_n as f64 * (NODE_H + GAP_Y) - GAP_Y;
    Layout {
        nodes,
        width: width.max(200.0),
        height: height.max(NODE_H + 2.0 * GAP_X),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependents_above_dependencies_even_with_cycles() {
        let labels: Vec<String> = ["app", "core", "util", "cli"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // app → core → util, cli → core, util → core (cycle)
        let l = layered(&labels, &[(0, 1), (1, 2), (3, 1), (2, 1)]);
        assert!(l.nodes[0].y < l.nodes[1].y);
        assert!(l.nodes[3].y < l.nodes[1].y);
        assert!(l.nodes[1].y < l.nodes[2].y);
        assert_eq!(l.nodes[0].y, l.nodes[3].y);
        assert!(l.nodes.iter().all(|p| p.x >= 0.0 && p.x + p.w <= l.width));
    }
}
