//! A map of a patch: where each module sits, and how the cables run.
//!
//! Used by the scope page as an SVG and by the live control page as JSON,
//! so both drawings come from the same layout.

use crate::module::AnalogModule;
use crate::network::{ModuleId, Network};
use crate::schedule::Schedule;
use std::fmt::Write as _;

/// Column and row of each module, from the evaluation order.
///
/// Producers sit to the left of the modules they feed. Feedback cables are
/// ignored when placing, so a loop does not stretch the page.
pub fn layout(network: &Network, schedule: &Schedule) -> Vec<(usize, usize)> {
    let mut column = vec![0usize; network.len()];
    for index in &schedule.order {
        for arrival in &schedule.incoming[*index] {
            if !arrival.delayed {
                column[*index] = column[*index].max(column[arrival.from] + 1);
            }
        }
    }
    let mut next_row = vec![0usize; column.iter().copied().max().unwrap_or(0) + 1];
    let mut placed = vec![(0usize, 0usize); network.len()];
    for index in &schedule.order {
        let col = column[*index];
        placed[*index] = (col, next_row[col]);
        next_row[col] += 1;
    }
    placed
}

/// One module on the map.
#[derive(Clone, Debug)]
pub struct Node {
    pub index: usize,
    pub name: String,
    pub bank: String,
    pub kind: String,
    pub col: usize,
    pub row: usize,
}

/// One cable on the map.
#[derive(Clone, Debug)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub from_jack: String,
    pub to_jack: String,
    pub feedback: bool,
}

/// The whole patch, ready to draw.
#[derive(Clone, Debug, Default)]
pub struct Map {
    pub nodes: Vec<Node>,
    pub cables: Vec<Edge>,
}

impl Map {
    pub fn of(network: &Network, schedule: &Schedule) -> Self {
        let placed = layout(network, schedule);
        let nodes = (0..network.len())
            .map(|index| {
                let label = network.label(ModuleId(index));
                Node {
                    index,
                    name: label.to_string(),
                    bank: label.split('[').next().unwrap_or(label).to_string(),
                    kind: network.get(ModuleId(index)).kind_name().to_string(),
                    col: placed[index].0,
                    row: placed[index].1,
                }
            })
            .collect();
        let cables = network
            .cables()
            .iter()
            .enumerate()
            .map(|(index, cable)| {
                let from = network.get(cable.from);
                let to = network.get(cable.to);
                Edge {
                    from: cable.from.0,
                    to: cable.to.0,
                    from_jack: from
                        .out_jacks()
                        .get(cable.from_jack)
                        .copied()
                        .unwrap_or("?")
                        .to_string(),
                    to_jack: to
                        .in_jacks()
                        .get(cable.to_jack)
                        .copied()
                        .unwrap_or("?")
                        .to_string(),
                    feedback: schedule.feedback.contains(&index),
                }
            })
            .collect();
        Self { nodes, cables }
    }

    pub fn to_json(&self) -> String {
        let mut out = String::from("{\"nodes\":[");
        for (i, node) in self.nodes.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('{');
            let _ = write!(
                out,
                "\"index\":{},\"col\":{},\"row\":{}",
                node.index, node.col, node.row
            );
            out.push(',');
            push_json_str(&mut out, "name", &node.name);
            out.push(',');
            push_json_str(&mut out, "bank", &node.bank);
            out.push(',');
            push_json_str(&mut out, "kind", &node.kind);
            out.push('}');
        }
        out.push_str("],\"cables\":[");
        for (i, cable) in self.cables.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('{');
            let _ = write!(
                out,
                "\"from\":{},\"to\":{},\"feedback\":{}",
                cable.from, cable.to, cable.feedback
            );
            out.push(',');
            push_json_str(&mut out, "from_jack", &cable.from_jack);
            out.push(',');
            push_json_str(&mut out, "to_jack", &cable.to_jack);
            out.push('}');
        }
        out.push_str("]}");
        out
    }
}

/// The patch as an SVG diagram, for the offline scope page.
pub fn svg(network: &Network, schedule: &Schedule) -> String {
    const BOX_W: f32 = 132.0;
    const BOX_H: f32 = 36.0;
    const COL: f32 = 186.0;
    const ROW: f32 = 52.0;
    const PAD: f32 = 16.0;

    let placed = layout(network, schedule);
    let columns = placed.iter().map(|(c, _)| *c).max().unwrap_or(0) + 1;
    let rows = placed.iter().map(|(_, r)| *r).max().unwrap_or(0) + 1;
    let width = PAD * 2.0 + columns as f32 * COL;
    let height = PAD * 2.0 + rows as f32 * ROW;
    let at = |index: usize| {
        let (c, r) = placed[index];
        (PAD + c as f32 * COL, PAD + r as f32 * ROW)
    };

    let mut svg = String::new();
    let _ = writeln!(
        svg,
        "<svg class=\"patch\" viewBox=\"0 0 {width:.0} {height:.0}\" \
         xmlns=\"http://www.w3.org/2000/svg\">"
    );
    svg.push_str(
        "<defs><marker id=\"tip\" viewBox=\"0 0 8 8\" refX=\"7\" refY=\"4\" \
         markerWidth=\"7\" markerHeight=\"7\" orient=\"auto\">\
         <path d=\"M0,0 L8,4 L0,8 z\" fill=\"#6b7280\"/></marker></defs>\n",
    );

    for (index, cable) in network.cables().iter().enumerate() {
        let (x0, y0) = at(cable.from.0);
        let (x1, y1) = at(cable.to.0);
        let (sx, sy) = (x0 + BOX_W, y0 + BOX_H / 2.0);
        let (ex, ey) = (x1, y1 + BOX_H / 2.0);
        let delayed = schedule.feedback.contains(&index);
        let bow = if delayed { -28.0 } else { 0.0 };
        let curve = ((ex - sx).abs() * 0.4).max(30.0);
        let _ = writeln!(
            svg,
            "<path d=\"M{sx:.0},{sy:.0} C{:.0},{:.0} {:.0},{:.0} {ex:.0},{ey:.0}\" \
             class=\"{}\" marker-end=\"url(#tip)\"/>",
            sx + curve,
            sy + bow,
            ex - curve,
            ey + bow,
            if delayed { "cable feedback" } else { "cable" }
        );
    }

    for index in 0..network.len() {
        let (x, y) = at(index);
        let module = network.get(ModuleId(index));
        let _ = writeln!(
            svg,
            "<rect x=\"{x:.0}\" y=\"{y:.0}\" width=\"{BOX_W:.0}\" height=\"{BOX_H:.0}\" \
             rx=\"6\" class=\"module\"/>\
             <text x=\"{:.0}\" y=\"{:.0}\" class=\"label\">{}</text>\
             <text x=\"{:.0}\" y=\"{:.0}\" class=\"kind\">{}</text>",
            x + 8.0,
            y + 15.0,
            escape(network.label(ModuleId(index))),
            x + 8.0,
            y + 29.0,
            escape(module.kind_name())
        );
    }
    svg.push_str("</svg>\n");
    svg
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn push_json_str(out: &mut String, key: &str, value: &str) {
    out.push('"');
    out.push_str(key);
    out.push_str("\":\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch;

    fn built(text: &str) -> (crate::Network, Schedule) {
        let built = patch::parse(text).unwrap().build().unwrap();
        let schedule = Schedule::build(&built.network);
        (built.network, schedule)
    }

    #[test]
    fn layout_puts_producers_left_of_consumers() {
        let (network, schedule) = built(
            "module c neuron\nmodule b neuron\nmodule a source\n\
             patch a.out -> b.in0\npatch b.out -> c.in0\n",
        );
        let placed = layout(&network, &schedule);
        let column = |label: &str| {
            let index = (0..network.len())
                .find(|i| network.label(ModuleId(*i)) == label)
                .unwrap();
            placed[index].0
        };
        assert!(column("a") < column("b"));
        assert!(column("b") < column("c"));
    }

    #[test]
    fn a_map_names_jacks_and_marks_feedback() {
        let (network, schedule) = built(
            "module a neuron\nmodule b neuron\n\
             patch a.out -> b.in0\npatch b.out -> a.in0\n",
        );
        let map = Map::of(&network, &schedule);
        assert_eq!(map.nodes.len(), 2);
        assert_eq!(map.cables.len(), 2);
        assert!(map.cables.iter().any(|c| c.feedback));
        assert!(map.cables.iter().any(|c| !c.feedback));
        assert!(map
            .cables
            .iter()
            .any(|c| c.from_jack == "out" && c.to_jack == "in0"));
        let json = map.to_json();
        assert!(json.contains("\"bank\":\"a\""));
        assert!(json.contains("\"feedback\":true"));
        assert!(!json.contains("NaN"));
    }
}
