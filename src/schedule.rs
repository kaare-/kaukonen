//! Evaluation order, and which cables genuinely need a delay.
//!
//! v1 read the previous step's outputs for every cable, so a chain of four
//! modules carried four steps of latency for no physical reason. Here the graph
//! is sorted so a feed-forward chain resolves inside one step, and only cables
//! that close a cycle fall back to the previous step's value — which is exactly
//! where a delay is unavoidable, since something has to break the loop.

use crate::module::AnalogModule;
use crate::network::Network;

/// One cable arriving at a module, resolved for fast summing.
#[derive(Clone, Copy, Debug)]
pub struct Arrival {
    pub from: usize,
    pub from_jack: usize,
    pub to_jack: usize,
    /// Read last step's value rather than this step's, because this cable
    /// closes a feedback loop.
    pub delayed: bool,
}

/// A network's fixed evaluation plan.
#[derive(Clone, Debug, Default)]
pub struct Schedule {
    /// Module indices in the order they should be stepped.
    pub order: Vec<usize>,
    /// Cables arriving at each module, indexed by module.
    pub incoming: Vec<Vec<Arrival>>,
    /// Indices into `Network::cables` of the cables that close a cycle.
    pub feedback: Vec<usize>,
}

/// DFS colours for cycle detection.
#[derive(Clone, Copy, PartialEq)]
enum Mark {
    Unseen,
    Open,
    Done,
}

impl Schedule {
    /// Plan a network. O(modules + cables).
    pub fn build(network: &Network) -> Self {
        let n = network.len();
        let mut out_edges: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (index, cable) in network.cables().iter().enumerate() {
            out_edges[cable.from.0].push(index);
        }

        let mut mark = vec![Mark::Unseen; n];
        let mut delayed = vec![false; network.cables().len()];
        let mut order = Vec::with_capacity(n);

        // Iterative depth-first search, so a deep chain of thousands of
        // modules cannot overflow the stack.
        let mut stack: Vec<(usize, usize)> = Vec::new();
        for root in 0..n {
            if mark[root] != Mark::Unseen {
                continue;
            }
            mark[root] = Mark::Open;
            stack.push((root, 0));
            while let Some((node, cursor)) = stack.pop() {
                if cursor < out_edges[node].len() {
                    stack.push((node, cursor + 1));
                    let cable_index = out_edges[node][cursor];
                    let next = network.cables()[cable_index].to.0;
                    match mark[next] {
                        // Reaches something still on the stack, so this cable
                        // closes a cycle and is the one that has to wait.
                        Mark::Open => delayed[cable_index] = true,
                        Mark::Unseen => {
                            mark[next] = Mark::Open;
                            stack.push((next, 0));
                        }
                        Mark::Done => {}
                    }
                } else {
                    mark[node] = Mark::Done;
                    order.push(node);
                }
            }
        }
        // Post-order visits a node after everything it feeds, so reversing it
        // puts producers before consumers.
        order.reverse();

        let mut incoming: Vec<Vec<Arrival>> = vec![Vec::new(); n];
        let mut feedback = Vec::new();
        for (index, cable) in network.cables().iter().enumerate() {
            incoming[cable.to.0].push(Arrival {
                from: cable.from.0,
                from_jack: cable.from_jack,
                to_jack: cable.to_jack,
                delayed: delayed[index],
            });
            if delayed[index] {
                feedback.push(index);
            }
        }

        Self {
            order,
            incoming,
            feedback,
        }
    }

    /// Describe each feedback cable in patch terms, for `inspect`.
    pub fn feedback_labels(&self, network: &Network) -> Vec<String> {
        self.feedback
            .iter()
            .map(|index| {
                let cable = network.cables()[*index];
                let from = network.get(cable.from);
                let to = network.get(cable.to);
                format!(
                    "{}.{} -> {}.{}",
                    network.label(cable.from),
                    from.out_jacks()
                        .get(cable.from_jack)
                        .copied()
                        .unwrap_or("?"),
                    network.label(cable.to),
                    to.in_jacks().get(cable.to_jack).copied().unwrap_or("?")
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch;

    fn schedule_of(text: &str) -> (Network, Schedule) {
        let built = patch::parse(text).unwrap().build().unwrap();
        let schedule = Schedule::build(&built.network);
        (built.network, schedule)
    }

    /// Position of a label in the evaluation order.
    fn slot(network: &Network, schedule: &Schedule, label: &str) -> usize {
        let id = (0..network.len())
            .find(|i| network.label(crate::network::ModuleId(*i)) == label)
            .unwrap();
        schedule.order.iter().position(|m| *m == id).unwrap()
    }

    #[test]
    fn a_feed_forward_chain_is_ordered_and_undelayed() {
        // Declared back to front, so a correct order cannot come from luck.
        let (net, s) = schedule_of(
            "module c neuron\nmodule b neuron\nmodule a source\n\
             patch b.out -> c.in0\npatch a.out -> b.in0\n",
        );
        assert!(slot(&net, &s, "a") < slot(&net, &s, "b"));
        assert!(slot(&net, &s, "b") < slot(&net, &s, "c"));
        assert!(s.feedback.is_empty(), "no cycle, so nothing should wait");
        assert_eq!(s.order.len(), 3);
    }

    #[test]
    fn a_loop_delays_exactly_one_cable() {
        let (net, s) = schedule_of(
            "module a neuron\nmodule b neuron\nmodule c neuron\n\
             patch a.out -> b.in0\npatch b.out -> c.in0\npatch c.out -> a.in0\n",
        );
        assert_eq!(s.feedback.len(), 1, "one delay is enough to break a loop");
        assert_eq!(s.feedback_labels(&net).len(), 1);
    }

    #[test]
    fn a_self_patch_is_its_own_loop() {
        let (_net, s) = schedule_of("module a neuron\npatch a.out -> a.in0\n");
        assert_eq!(s.feedback.len(), 1);
        assert!(s.incoming[0][0].delayed);
    }

    #[test]
    fn two_independent_loops_each_get_one_delay() {
        let (_net, s) = schedule_of(
            "module a neuron\nmodule b neuron\nmodule c neuron\nmodule d neuron\n\
             patch a.out -> b.in0\npatch b.out -> a.in0\n\
             patch c.out -> d.in0\npatch d.out -> c.in0\n",
        );
        assert_eq!(s.feedback.len(), 2);
    }

    #[test]
    fn every_module_is_scheduled_exactly_once() {
        let (net, s) = schedule_of(
            "module cells rc1[40]\nmodule brain tc3\n\
             patch cells[*].out -> brain.a\npatch brain.a -> cells[i].in\n",
        );
        assert_eq!(s.order.len(), net.len());
        let mut seen = s.order.clone();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), net.len());
    }

    #[test]
    fn a_deep_chain_does_not_overflow_the_stack() {
        let mut text = String::from("module chain neuron[2000]\n");
        for i in 0..1999 {
            text.push_str(&format!("patch chain[{i}].out -> chain[{}].in0\n", i + 1));
        }
        let (net, s) = schedule_of(&text);
        assert_eq!(s.order.len(), 2000);
        assert!(s.feedback.is_empty());
        assert!(slot(&net, &s, "chain[0]") < slot(&net, &s, "chain[1999]"));
    }
}
