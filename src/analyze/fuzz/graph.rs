//! The call graph among indexed functions, from the index's resolved call
//! edges: bounded reach in both directions and which functions recurse.

use std::collections::{HashMap, VecDeque};

use crate::analyze::bugs::{FnSpan, Project};

/// Functions (dense indices) and their resolved calls.
pub struct CallGraph {
    pub functions: Vec<FnSpan>,
    index: HashMap<String, usize>,
    callees: Vec<Vec<usize>>,
    callers: Vec<Vec<usize>>,
    /// In a call cycle (including calling itself).
    pub recursive: Vec<bool>,
}

impl CallGraph {
    /// Add calls the index does not hold (a lazy result's `next`), and
    /// recompute which functions recurse.
    pub fn add_edges(&mut self, edges: &[(usize, usize)]) {
        for &(from, to) in edges {
            if !self.callees[from].contains(&to) {
                self.callees[from].push(to);
                self.callers[to].push(from);
            }
        }
        self.recursive = recursive_functions(&self.callees);
    }

    pub fn new(project: &Project) -> Self {
        let mut functions: Vec<FnSpan> = project
            .files()
            .iter()
            .flat_map(|file| project.functions_in(file).iter().cloned())
            .collect();
        functions
            .sort_by(|a, b| (&a.file, a.start_line, &a.id).cmp(&(&b.file, b.start_line, &b.id)));
        let index: HashMap<String, usize> = functions
            .iter()
            .enumerate()
            .map(|(i, span)| (span.id.clone(), i))
            .collect();
        let mut callees = vec![Vec::new(); functions.len()];
        let mut callers = vec![Vec::new(); functions.len()];
        for site in project.call_sites() {
            let (Some(&from), Some(&to)) = (index.get(&site.caller_id), index.get(&site.callee_id))
            else {
                continue;
            };
            callees[from].push(to);
            callers[to].push(from);
        }
        for list in callees.iter_mut().chain(callers.iter_mut()) {
            list.sort_unstable();
            list.dedup();
        }
        let recursive = recursive_functions(&callees);
        Self {
            functions,
            index,
            callees,
            callers,
            recursive,
        }
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.index.get(id).copied()
    }

    pub fn callers(&self, function: usize) -> &[usize] {
        &self.callers[function]
    }

    pub fn callees(&self, function: usize) -> &[usize] {
        &self.callees[function]
    }

    /// Functions reachable from `from` through calls (itself at depth 0),
    /// breadth first, at most `max_depth` hops and `cap` functions.
    pub fn reach(&self, from: usize, max_depth: u32, cap: usize) -> Vec<(usize, u32)> {
        bfs(&self.callees, from, max_depth, cap)
    }

    /// Functions that reach `to` through calls (itself at depth 0).
    pub fn reverse_reach(&self, to: usize, max_depth: u32, cap: usize) -> Vec<(usize, u32)> {
        bfs(&self.callers, to, max_depth, cap)
    }
}

fn bfs(adjacency: &[Vec<usize>], start: usize, max_depth: u32, cap: usize) -> Vec<(usize, u32)> {
    let mut seen = vec![false; adjacency.len()];
    let mut out = Vec::new();
    let mut queue = VecDeque::from([(start, 0u32)]);
    seen[start] = true;
    while let Some((node, depth)) = queue.pop_front() {
        out.push((node, depth));
        if out.len() >= cap {
            break;
        }
        if depth == max_depth {
            continue;
        }
        for &next in &adjacency[node] {
            if !seen[next] {
                seen[next] = true;
                queue.push_back((next, depth + 1));
            }
        }
    }
    out
}

/// Functions in a strongly connected component of more than one function,
/// or calling themselves (iterative Tarjan: the graph can be deep).
fn recursive_functions(callees: &[Vec<usize>]) -> Vec<bool> {
    let n = callees.len();
    let mut recursive = vec![false; n];
    let mut order = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack = Vec::new();
    let mut counter = 0;
    for root in 0..n {
        if order[root] != usize::MAX {
            continue;
        }
        // (node, next child position)
        let mut frames = vec![(root, 0usize)];
        order[root] = counter;
        low[root] = counter;
        counter += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(&mut (node, ref mut position)) = frames.last_mut() {
            if let Some(&child) = callees[node].get(*position) {
                *position += 1;
                if child == node {
                    recursive[node] = true;
                }
                if order[child] == usize::MAX {
                    order[child] = counter;
                    low[child] = counter;
                    counter += 1;
                    stack.push(child);
                    on_stack[child] = true;
                    frames.push((child, 0));
                } else if on_stack[child] {
                    low[node] = low[node].min(order[child]);
                }
                continue;
            }
            frames.pop();
            if let Some(&(parent, _)) = frames.last() {
                low[parent] = low[parent].min(low[node]);
            }
            if low[node] == order[node] {
                let mut component = Vec::new();
                while let Some(member) = stack.pop() {
                    on_stack[member] = false;
                    component.push(member);
                    if member == node {
                        break;
                    }
                }
                if component.len() > 1 {
                    for member in component {
                        recursive[member] = true;
                    }
                }
            }
        }
    }
    recursive
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycles_and_self_calls_are_recursive() {
        // 0 → 1 → 2 → 1, 3 → 3, 4 → 0
        let callees = vec![vec![1], vec![2], vec![1], vec![3], vec![0]];
        assert_eq!(
            recursive_functions(&callees),
            vec![false, true, true, true, false]
        );
    }

    #[test]
    fn bfs_is_bounded_by_depth_and_cap() {
        let chain = vec![vec![1], vec![2], vec![3], vec![]];
        assert_eq!(bfs(&chain, 0, 2, 100), vec![(0, 0), (1, 1), (2, 2)]);
        assert_eq!(bfs(&chain, 0, 10, 2), vec![(0, 0), (1, 1)]);
    }
}
