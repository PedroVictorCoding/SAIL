//! Lesson plans: a dependency DAG from what the learner knows to their goal (#235).
use super::{Invalid, MAX_SHORT, bounded};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub const MAX_NODES: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanNode {
    pub id: String,
    pub title: String,
}

/// `edges` are `[prerequisite, dependent]` pairs. `goal` is the node everything leads to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LessonPlan {
    pub goal: String,
    pub nodes: Vec<PlanNode>,
    #[serde(default)]
    pub edges: Vec<[String; 2]>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

impl LessonPlan {
    /// Checks the plan and returns one error naming the problem, worded for the model.
    pub fn validate(&self) -> Result<(), Invalid> {
        if self.nodes.is_empty() || self.nodes.len() > MAX_NODES {
            return Err(Invalid::new(format!(
                "a plan needs 1 to {MAX_NODES} nodes, got {}",
                self.nodes.len()
            )));
        }
        let mut ids = BTreeSet::new();
        for node in &self.nodes {
            if !valid_id(&node.id) {
                return Err(Invalid::new(format!(
                    "node id `{}` must be 1-40 characters of letters, digits, `_` or `-`",
                    node.id
                )));
            }
            bounded("title", &node.title, MAX_SHORT)?;
            if !ids.insert(node.id.as_str()) {
                return Err(Invalid::new(format!("node id `{}` is used twice", node.id)));
            }
        }
        if !ids.contains(self.goal.as_str()) {
            return Err(Invalid::new(format!(
                "`goal` `{}` is not one of the node ids",
                self.goal
            )));
        }
        for [from, to] in &self.edges {
            for end in [from, to] {
                if !ids.contains(end.as_str()) {
                    return Err(Invalid::new(format!(
                        "edge [{from}, {to}] refers to unknown node `{end}`"
                    )));
                }
            }
            if from == to {
                return Err(Invalid::new(format!("edge [{from}, {to}] is a self-loop")));
            }
        }
        if self.order().len() != self.nodes.len() {
            return Err(Invalid::new(
                "the edges contain a cycle; a plan must be a DAG of prerequisites",
            ));
        }
        // Every node must lead to the goal: walk the edges backwards from it.
        let mut reaches = BTreeSet::from([self.goal.as_str()]);
        let mut queue = VecDeque::from([self.goal.as_str()]);
        while let Some(node) = queue.pop_front() {
            for [from, to] in &self.edges {
                if to == node && reaches.insert(from.as_str()) {
                    queue.push_back(from.as_str());
                }
            }
        }
        if let Some(stray) = self.nodes.iter().find(|n| !reaches.contains(n.id.as_str())) {
            return Err(Invalid::new(format!(
                "node `{}` does not lead to the goal `{}`; connect it or remove it",
                stray.id, self.goal
            )));
        }
        Ok(())
    }

    /// Teaching order: prerequisites first (Kahn's algorithm, ties broken by declaration
    /// order so the result is deterministic). Shorter than `nodes` when there is a cycle.
    pub fn order(&self) -> Vec<String> {
        let position: BTreeMap<&str, usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.as_str(), i))
            .collect();
        let mut indegree: BTreeMap<&str, usize> =
            self.nodes.iter().map(|n| (n.id.as_str(), 0)).collect();
        for [from, to] in &self.edges {
            if position.contains_key(from.as_str())
                && let Some(d) = indegree.get_mut(to.as_str())
            {
                *d += 1;
            }
        }
        let mut ready: BTreeSet<(usize, &str)> = indegree
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(id, _)| (position[id], *id))
            .collect();
        let mut out = Vec::new();
        while let Some(first) = ready.pop_first() {
            out.push(first.1.to_string());
            for [from, to] in &self.edges {
                if from == first.1
                    && let Some(d) = indegree.get_mut(to.as_str())
                {
                    *d -= 1;
                    if *d == 0 {
                        ready.insert((position[to.as_str()], to.as_str()));
                    }
                }
            }
        }
        out
    }

    pub fn contains(&self, id: &str) -> bool {
        self.nodes.iter().any(|n| n.id == id)
    }

    /// Mermaid flowchart source. Titles are reduced to a safe character set and quoted, so
    /// model text cannot inject Mermaid directives or HTML.
    pub fn to_mermaid(&self) -> String {
        let mut out = String::from("flowchart TD\n");
        for node in &self.nodes {
            let label: String = node
                .title
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || " .,:;?!()+-*/=_'".contains(c) {
                        c
                    } else {
                        ' '
                    }
                })
                .collect();
            out.push_str(&format!("  {}[\"{}\"]\n", node.id, label.trim()));
        }
        for [from, to] in &self.edges {
            out.push_str(&format!("  {from} --> {to}\n"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(nodes: &[&str], edges: &[(&str, &str)], goal: &str) -> LessonPlan {
        LessonPlan {
            goal: goal.into(),
            nodes: nodes
                .iter()
                .map(|id| PlanNode {
                    id: id.to_string(),
                    title: format!("Title {id}"),
                })
                .collect(),
            edges: edges
                .iter()
                .map(|(a, b)| [a.to_string(), b.to_string()])
                .collect(),
        }
    }

    #[test]
    fn valid_plan_orders_prerequisites_first() {
        let p = plan(&["c", "a", "b"], &[("a", "b"), ("b", "c")], "c");
        p.validate().unwrap();
        assert_eq!(p.order(), ["a", "b", "c"]);
    }

    #[test]
    fn every_validation_error_names_the_problem() {
        let cases: Vec<(LessonPlan, &str)> = vec![
            (plan(&[], &[], "a"), "1 to 40 nodes"),
            (plan(&["a b"], &[], "a b"), "must be 1-40 characters"),
            (plan(&["a", "a"], &[], "a"), "used twice"),
            (plan(&["a"], &[], "z"), "is not one of the node ids"),
            (plan(&["a", "b"], &[("a", "x")], "b"), "unknown node `x`"),
            (plan(&["a"], &[("a", "a")], "a"), "self-loop"),
            (
                plan(&["a", "b", "c"], &[("a", "b"), ("b", "a"), ("b", "c")], "c"),
                "cycle",
            ),
            (plan(&["a", "b"], &[], "b"), "does not lead to the goal"),
        ];
        for (p, expected) in cases {
            let error = p.validate().unwrap_err().0;
            assert!(
                error.contains(expected),
                "{error} should mention {expected}"
            );
        }
        let mut long = plan(&["a"], &[], "a");
        long.nodes[0].title = "x".repeat(MAX_SHORT + 1);
        assert!(long.validate().unwrap_err().0.contains("too long"));
    }

    #[test]
    fn mermaid_escapes_titles() {
        let mut p = plan(&["a", "b"], &[("a", "b")], "b");
        p.nodes[0].title = "x\"]\n%%{init}<script>".into();
        let m = p.to_mermaid();
        assert!(m.starts_with("flowchart TD\n"));
        assert!(!m.contains("<script>") && !m.contains("%%") && !m.contains("{init}"));
        assert!(m.contains("a --> b"));
        assert_eq!(m.lines().count(), 4);
    }
}
