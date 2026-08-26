//! Native SVG layout via the pure-Rust `layout-rs` crate: no Node, no Chromium, no external
//! binary. Decoupled from the render types; callers pass plain labels and colours.

use layout::backends::svg::SVGWriter;
use layout::core::base::Orientation;
use layout::core::color::Color;
use layout::core::geometry::Point;
use layout::core::style::StyleAttr;
use layout::std_shapes::shapes::{Arrow, Element, ShapeKind};
use layout::topo::layout::VisualGraph;

/// One node to draw: its label and delta colours (stroke + fill as `#rrggbb` or a named colour).
pub struct SvgNode {
    /// Box label.
    pub label: String,
    /// Border colour.
    pub stroke: String,
    /// Fill colour.
    pub fill: String,
}

/// Lay out `nodes` and `edges` (index pairs into `nodes`) left-to-right and return an SVG document.
/// Deterministic in input order; layout-rs assigns coordinates.
pub fn layout_svg(nodes: &[SvgNode], edges: &[(usize, usize)]) -> String {
    if nodes.is_empty() {
        return String::from("<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>\n");
    }

    let mut vg = VisualGraph::new(Orientation::LeftToRight);
    let mut handles = Vec::with_capacity(nodes.len());
    for n in nodes {
        let shape = ShapeKind::new_box(&n.label);
        // width scales with the label so text fits; height fixed.
        let width = (n.label.chars().count() as f64 * 9.0 + 24.0).max(60.0);
        let look = StyleAttr::new(Color::fast(&n.stroke), 2, Some(Color::fast(&n.fill)), 0, 15);
        let el = Element::create(
            shape,
            look,
            Orientation::LeftToRight,
            Point::new(width, 42.0),
        );
        handles.push(vg.add_node(el));
    }
    for (from, to) in edges {
        if let (Some(&f), Some(&t)) = (handles.get(*from), handles.get(*to)) {
            vg.add_edge(Arrow::simple(""), f, t);
        }
    }

    let mut writer = SVGWriter::new();
    vg.do_it(false, false, false, &mut writer);
    writer.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(label: &str, fill: &str) -> SvgNode {
        SvgNode {
            label: label.to_string(),
            stroke: "#22c55e".to_string(),
            fill: fill.to_string(),
        }
    }

    #[test]
    fn renders_an_svg_document_with_labels() {
        let nodes = vec![node("alpha", "#f0fdf4"), node("beta", "#ffffff")];
        let out = layout_svg(&nodes, &[(0, 1)]);
        assert!(out.contains("<svg"), "expected an svg document:\n{out}");
        assert!(out.contains("alpha") && out.contains("beta"));
    }

    #[test]
    fn empty_is_a_valid_empty_svg() {
        let out = layout_svg(&[], &[]);
        assert!(out.contains("<svg"));
    }
}
