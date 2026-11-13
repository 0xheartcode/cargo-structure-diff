//! Native layered ("Sugiyama-lite") ASCII boxes-and-arrows layouter, pure Rust with no deps.
//!
//! Pipeline (all deterministic):
//! 1. Build directed adjacency; break cycles by a DFS from sorted roots. An edge to a node currently
//!    on the DFS stack is a *back-edge*: excluded from ranking, remembered for the legend.
//! 2. Rank by longest path over non-back edges: `rank(n) = 0` with no non-back incoming edge, else
//!    `max(rank(pred)) + 1`.
//! 3. Order nodes within a rank by id (the caller passes nodes in id order, so index order is id
//!    order).
//! 4. Lay out left-to-right: rank = column, nodes stacked vertically within a column, each drawn as a
//!    box. Box width per column is the widest label plus padding.
//! 5. Adjacent-rank edges are drawn as `--->` connectors in the gutter between columns. Edges
//!    spanning more than one rank, and back-edges, are collected into a legend under the diagram.
//!
//! v1 favours a clean, deterministic result on small graphs over crossing-minimization: adjacent
//! arrows plus a legend for the rest is the accepted design. Corners use the portable ASCII glyphs
//! `+ - | >`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// A node to lay out: its delta marker (`+`/`-`/`~`/space) and short display name.
pub(crate) struct BoxNode {
    /// Delta marker prefixed to the label: `+` added, `-` removed, `~` changed, space context.
    pub marker: char,
    /// Short display name (the caller supplies the last `::segment`, module-prefixed for overview).
    pub name: String,
}

impl BoxNode {
    /// The in-box label: the marker char followed by the name.
    fn label(&self) -> String {
        format!("{}{}", self.marker, self.name)
    }
}

/// Lay out `nodes` (in id order) connected by `edges` (index pairs into `nodes`) as an ASCII
/// boxes-and-arrows diagram. Returns the rendered text (trailing newline included). An empty node
/// list yields `(empty)`.
pub(crate) fn layout(nodes: &[BoxNode], edges: &[(usize, usize)]) -> String {
    let n = nodes.len();
    if n == 0 {
        return String::from("(empty)\n");
    }

    // Deduplicated, sorted edges give a stable adjacency (and a stable legend order later).
    let mut es: Vec<(usize, usize)> = edges.to_vec();
    es.sort_unstable();
    es.dedup();

    let mut succ: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for &(f, t) in &es {
        succ.entry(f).or_default().push(t);
    }

    let back = break_cycles(n, &succ, &es);
    let rank = rank_nodes(n, &es, &back);

    // Columns by rank; nodes within a column stay in id (index) order.
    let ncols = rank.iter().max().copied().unwrap_or(0) + 1;
    let mut cols: Vec<Vec<usize>> = vec![Vec::new(); ncols];
    for (i, r) in rank.iter().enumerate() {
        cols[*r].push(i);
    }
    let mut pos: Vec<(usize, usize)> = vec![(0, 0); n];
    for (c, col) in cols.iter().enumerate() {
        for (r, &i) in col.iter().enumerate() {
            pos[i] = (c, r);
        }
    }

    // Box geometry: inner width per column is the widest label; a box is `inner + 4` wide.
    let mut inner = vec![1usize; ncols];
    for (i, node) in nodes.iter().enumerate() {
        let len = node.label().chars().count();
        let c = rank[i];
        if len > inner[c] {
            inner[c] = len;
        }
    }
    let boxw: Vec<usize> = inner.iter().map(|w| w + 4).collect();

    // Classify edges: adjacent-rank drawn, everything else (skip / back) to the legend.
    let mut draw: Vec<(usize, usize)> = Vec::new();
    let mut legend: Vec<(usize, usize, &'static str)> = Vec::new();
    for &(f, t) in &es {
        if back.contains(&(f, t)) {
            legend.push((f, t, "back"));
        } else if rank[t] == rank[f] + 1 {
            draw.push((f, t));
        } else {
            // A non-back edge always points forward, so this spans more than one rank.
            legend.push((f, t, "skip"));
        }
    }

    // Gutter width per column boundary: room for one vertical channel per jog edge, min `--->`.
    let ngutters = ncols.saturating_sub(1);
    let mut gutterw = vec![0usize; ngutters];
    for (c, gw) in gutterw.iter_mut().enumerate() {
        let njog = draw
            .iter()
            .filter(|&&(f, t)| pos[f].0 == c && pos[f].1 != pos[t].1)
            .count();
        *gw = std::cmp::max(4, njog + 2);
    }

    // Column x-origins and total canvas size.
    let mut colx = vec![0usize; ncols];
    let mut x = 0usize;
    for c in 0..ncols {
        colx[c] = x;
        x += boxw[c];
        if c + 1 < ncols {
            x += gutterw[c];
        }
    }
    let width = x;
    let maxrows = cols.iter().map(Vec::len).max().unwrap_or(0);
    let height = if maxrows == 0 { 0 } else { maxrows * 4 - 1 };
    let mut canvas = vec![vec![' '; width]; height];

    // Paint boxes.
    for (i, node) in nodes.iter().enumerate() {
        let (c, r) = pos[i];
        let x0 = colx[c];
        let w = boxw[c];
        let (top, mid, bot) = (r * 4, r * 4 + 1, r * 4 + 2);
        canvas[top][x0] = '+';
        canvas[top][x0 + w - 1] = '+';
        canvas[bot][x0] = '+';
        canvas[bot][x0 + w - 1] = '+';
        canvas[top][x0 + 1..x0 + w - 1].fill('-');
        canvas[bot][x0 + 1..x0 + w - 1].fill('-');
        canvas[mid][x0] = '|';
        canvas[mid][x0 + w - 1] = '|';
        for (k, ch) in node.label().chars().enumerate() {
            canvas[mid][x0 + 2 + k] = ch;
        }
    }

    // Paint adjacent-rank arrows, gutter by gutter: straight (same-row) first, then jog edges each on
    // their own vertical channel so a later corner reads as a junction over a straight run.
    for (c, &gw) in gutterw.iter().enumerate() {
        let gstart = colx[c] + boxw[c];
        let lastx = gstart + gw - 1;

        for &(f, t) in &draw {
            if pos[f].0 != c || pos[f].1 != pos[t].1 {
                continue;
            }
            let line = pos[f].1 * 4 + 1;
            canvas[line][gstart..lastx].fill('-');
            canvas[line][lastx] = '>';
        }

        let mut jogs: Vec<(usize, usize)> = draw
            .iter()
            .copied()
            .filter(|&(f, t)| pos[f].0 == c && pos[f].1 != pos[t].1)
            .collect();
        jogs.sort_by_key(|&(f, t)| (pos[f].1, pos[t].1, f));
        for (k, &(f, t)) in jogs.iter().enumerate() {
            let cx = gstart + 1 + k;
            let sline = pos[f].1 * 4 + 1;
            let dline = pos[t].1 * 4 + 1;
            canvas[sline][gstart..cx].fill('-');
            canvas[sline][cx] = '+';
            let (lo, hi) = if sline < dline {
                (sline, dline)
            } else {
                (dline, sline)
            };
            for line in canvas.iter_mut().take(hi).skip(lo + 1) {
                if line[cx] == ' ' {
                    line[cx] = '|';
                }
            }
            canvas[dline][cx] = '+';
            canvas[dline][cx + 1..lastx].fill('-');
            canvas[dline][lastx] = '>';
        }
    }

    // Canvas to text, trimming trailing spaces on every line.
    let mut out = String::new();
    for row in &canvas {
        let line: String = row.iter().collect();
        out.push_str(line.trim_end());
        out.push('\n');
    }

    // Legend for skip and back edges, in (from, to) order.
    if !legend.is_empty() {
        legend.sort_by_key(|e| (e.0, e.1));
        out.push('\n');
        out.push_str("Legend:\n");
        for (f, t, kind) in legend {
            let _ = writeln!(
                out,
                "  {} ---> {}  ({})",
                nodes[f].name, nodes[t].name, kind
            );
        }
    }

    out
}

/// Find back-edges by a DFS from sorted roots (in-degree-zero nodes, else all nodes). An edge to a
/// node currently on the DFS stack (grey) is a back-edge. Any node left unvisited (a disjoint cyclic
/// component) is then DFS'd in id order, so every node is covered.
fn break_cycles(
    n: usize,
    succ: &BTreeMap<usize, Vec<usize>>,
    edges: &[(usize, usize)],
) -> BTreeSet<(usize, usize)> {
    let mut indeg = vec![0usize; n];
    for &(_, t) in edges {
        indeg[t] += 1;
    }
    let mut roots: Vec<usize> = (0..n).filter(|i| indeg[*i] == 0).collect();
    if roots.is_empty() {
        roots = (0..n).collect();
    }

    // 0 = white (unseen), 1 = grey (on stack), 2 = black (done).
    let mut color = vec![0u8; n];
    let mut back = BTreeSet::new();
    for r in roots {
        if color[r] == 0 {
            dfs(r, succ, &mut color, &mut back);
        }
    }
    for i in 0..n {
        if color[i] == 0 {
            dfs(i, succ, &mut color, &mut back);
        }
    }
    back
}

/// DFS helper for [`break_cycles`]: colour `u` grey, recurse into white successors, record an edge to
/// a grey successor as a back-edge, then colour `u` black.
fn dfs(
    u: usize,
    succ: &BTreeMap<usize, Vec<usize>>,
    color: &mut [u8],
    back: &mut BTreeSet<(usize, usize)>,
) {
    color[u] = 1;
    if let Some(vs) = succ.get(&u) {
        for &v in vs {
            match color[v] {
                1 => {
                    back.insert((u, v));
                }
                0 => dfs(v, succ, color, back),
                _ => {}
            }
        }
    }
    color[u] = 2;
}

/// Longest-path rank over non-back edges via Kahn's algorithm (a deterministic ready set). A node
/// with no non-back incoming edge has rank 0; otherwise `max(rank(pred)) + 1`.
fn rank_nodes(n: usize, edges: &[(usize, usize)], back: &BTreeSet<(usize, usize)>) -> Vec<usize> {
    let mut succ: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut indeg = vec![0usize; n];
    for &(f, t) in edges {
        if back.contains(&(f, t)) {
            continue;
        }
        succ.entry(f).or_default().push(t);
        indeg[t] += 1;
    }

    let mut rank = vec![0usize; n];
    let mut ready: BTreeSet<usize> = (0..n).filter(|i| indeg[*i] == 0).collect();
    while let Some(&u) = ready.iter().next() {
        ready.remove(&u);
        if let Some(vs) = succ.get(&u) {
            for &v in vs {
                if rank[u] + 1 > rank[v] {
                    rank[v] = rank[u] + 1;
                }
                indeg[v] -= 1;
                if indeg[v] == 0 {
                    ready.insert(v);
                }
            }
        }
    }
    rank
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(marker: char, name: &str) -> BoxNode {
        BoxNode {
            marker,
            name: name.to_string(),
        }
    }

    #[test]
    fn empty_graph_is_empty() {
        assert_eq!(layout(&[], &[]), "(empty)\n");
    }

    #[test]
    fn ranks_a_chain_by_longest_path() {
        // a -> b -> c: three ranks.
        let nodes = [node(' ', "a"), node(' ', "b"), node(' ', "c")];
        let back = BTreeSet::new();
        let rank = rank_nodes(3, &[(0, 1), (1, 2)], &back);
        assert_eq!(rank, vec![0, 1, 2]);
        let _ = &nodes;
    }

    #[test]
    fn diamond_ranks_the_join_last() {
        // a->b, a->c, b->d, c->d: a=0, b=c=1, d=2.
        let back = BTreeSet::new();
        let rank = rank_nodes(4, &[(0, 1), (0, 2), (1, 3), (2, 3)], &back);
        assert_eq!(rank, vec![0, 1, 1, 2]);
    }

    #[test]
    fn cycle_records_one_back_edge() {
        // a -> b -> a: the edge back to the on-stack root is the back-edge.
        let mut succ: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        succ.insert(0, vec![1]);
        succ.insert(1, vec![0]);
        let back = break_cycles(2, &succ, &[(0, 1), (1, 0)]);
        assert_eq!(back.len(), 1);
        assert!(back.contains(&(1, 0)));
    }
}
