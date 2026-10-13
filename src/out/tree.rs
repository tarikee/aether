//! The process tree: every single process under its parent, siblings in the order the view
//! chooses. A process whose parent is not in the list is a root, and a loop in the parent
//! links is walked once, so every process appears exactly once.
//!
//! The scratch vectors live in the `Tree` the live loop keeps between ticks, so they grow only
//! when the list does.

use aether::sys::Proc;
use std::cmp::Ordering;

/// The deepest level drawn; a process below it is drawn at this depth.
pub const MAX_DEPTH: u8 = 63;

const NONE: usize = usize::MAX;

/// One line of the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    /// Index into the `procs` slice that `Tree::build` was given.
    pub index: usize,
    /// Depth: a root is 0, and anything deeper than `MAX_DEPTH` is drawn at `MAX_DEPTH`.
    pub depth: u8,
    /// Whether a later sibling follows this process (its mark is `├─` rather than `└─`).
    pub more: bool,
    /// Bit k: the process at depth k+1 on the way to this one has a later sibling.
    lines: u64,
}

/// A walk in progress: the node, its next child to visit and the end of its children.
#[derive(Clone, Copy, Debug)]
struct Frame {
    next: usize,
    end: usize,
    depth: usize,
    lines: u64,
}

/// The tree and its scratch vectors. Cleared on every build, never freed.
#[derive(Debug, Default)]
pub struct Tree {
    rows: Vec<Row>,
    by_pid: Vec<(u32, usize)>,
    parent: Vec<usize>,
    order: Vec<usize>,
    first: Vec<usize>,
    cursor: Vec<usize>,
    kids: Vec<usize>,
    seen: Vec<bool>,
    stack: Vec<Frame>,
}

impl Tree {
    pub const fn new() -> Tree {
        Tree {
            rows: Vec::new(),
            by_pid: Vec::new(),
            parent: Vec::new(),
            order: Vec::new(),
            first: Vec::new(),
            cursor: Vec::new(),
            kids: Vec::new(),
            seen: Vec::new(),
            stack: Vec::new(),
        }
    }

    /// The rows of the last build, roots and their children in depth-first order.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Builds the tree of `procs`. `cmp` compares two indices into `procs` and sets the order
    /// of siblings (and of the roots).
    pub fn build(&mut self, procs: &[Proc], mut cmp: impl FnMut(usize, usize) -> Ordering) {
        let n = procs.len();
        self.rows.clear();
        self.by_pid.clear();
        self.by_pid
            .extend(procs.iter().enumerate().map(|(i, p)| (p.pid, i)));
        self.by_pid.sort_unstable();

        self.parent.clear();
        for (i, p) in procs.iter().enumerate() {
            let at = self.by_pid.partition_point(|e| e.0 < p.ppid);
            let found = self.by_pid.get(at).filter(|e| e.0 == p.ppid).map(|e| e.1);
            self.parent.push(match found {
                Some(j) if j != i => j,
                _ => NONE,
            });
        }

        self.order.clear();
        self.order.extend(0..n);
        self.order.sort_unstable_by(|&a, &b| cmp(a, b));

        self.first.clear();
        self.first.resize(n + 1, 0);
        for &p in &self.parent {
            if p != NONE {
                self.first[p + 1] += 1;
            }
        }
        for i in 0..n {
            self.first[i + 1] += self.first[i];
        }
        self.cursor.clear();
        self.cursor.extend_from_slice(&self.first[..n]);
        self.kids.clear();
        self.kids.resize(self.first[n], 0);
        for &i in &self.order {
            let p = self.parent[i];
            if p != NONE {
                self.kids[self.cursor[p]] = i;
                self.cursor[p] += 1;
            }
        }

        self.seen.clear();
        self.seen.resize(n, false);
        for k in 0..n {
            let i = self.order[k];
            if self.parent[i] == NONE {
                self.walk(i);
            }
        }
        // Whatever no root reaches sits in a loop of parent links: start it anywhere.
        for k in 0..n {
            let i = self.order[k];
            if !self.seen[i] {
                self.walk(i);
            }
        }
    }

    /// Writes the branch marks of `row` into `out`: one three-column step per level, then the
    /// mark of the row itself. A root has none.
    pub fn draw(&self, row: &Row, out: &mut String) {
        let d = row.depth as usize;
        if d == 0 {
            return;
        }
        for k in 0..d - 1 {
            out.push_str(if (row.lines >> k) & 1 == 1 {
                "│  "
            } else {
                "   "
            });
        }
        out.push_str(if row.more { "├─ " } else { "└─ " });
    }

    /// Walks the tree from `root`, adding each process not yet seen.
    fn walk(&mut self, root: usize) {
        self.seen[root] = true;
        self.rows.push(Row {
            index: root,
            depth: 0,
            more: false,
            lines: 0,
        });
        self.stack.clear();
        self.stack.push(Frame {
            next: self.first[root],
            end: self.first[root + 1],
            depth: 0,
            lines: 0,
        });
        while let Some(&Frame {
            next,
            end,
            depth,
            lines,
        }) = self.stack.last()
        {
            if next == end {
                self.stack.pop();
                continue;
            }
            if let Some(top) = self.stack.last_mut() {
                top.next += 1;
            }
            let child = self.kids[next];
            if self.seen[child] {
                continue;
            }
            self.seen[child] = true;
            let more = self.kids[next + 1..end].iter().any(|&k| !self.seen[k]);
            let d = depth + 1;
            let own = if d - 1 < 64 && more {
                1u64 << (d - 1)
            } else {
                0
            };
            self.rows.push(Row {
                index: child,
                depth: d.min(MAX_DEPTH as usize) as u8,
                more,
                lines,
            });
            self.stack.push(Frame {
                next: self.first[child],
                end: self.first[child + 1],
                depth: d,
                lines: lines | own,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, ppid: u32) -> Proc {
        Proc {
            pid,
            ppid,
            uid: 0,
            threads: 1,
            name: format!("p{pid}"),
            state: b'S',
            cpu_pct: None,
            gpu_pct: None,
            mem: 0,
            rss: 0,
            started_s: 0,
            vram: None,
            read_bps: None,
            write_bps: None,
        }
    }

    /// Builds the tree with siblings in pid order and returns (pid, drawn line) per row.
    fn lines_of(procs: &[Proc]) -> Vec<(u32, String)> {
        let mut t = Tree::new();
        t.build(procs, |a, b| procs[a].pid.cmp(&procs[b].pid));
        t.rows()
            .iter()
            .map(|r| {
                let mut s = String::new();
                t.draw(r, &mut s);
                (procs[r.index].pid, s)
            })
            .collect()
    }

    #[test]
    fn a_chain_nests_one_step_per_level() {
        let procs = [proc(1, 0), proc(2, 1), proc(3, 2)];
        let lines = lines_of(&procs);
        assert_eq!(
            lines,
            vec![
                (1, String::new()),
                (2, "└─ ".to_string()),
                (3, "   └─ ".to_string()),
            ]
        );
    }

    #[test]
    fn siblings_take_the_branch_marks_and_the_line_runs_on_under_the_first() {
        // 1 has children 2 and 4 in pid order; 2 has child 3.
        let procs = [proc(1, 0), proc(2, 1), proc(4, 1), proc(3, 2)];
        let lines = lines_of(&procs);
        assert_eq!(
            lines,
            vec![
                (1, String::new()),
                (2, "├─ ".to_string()),
                (3, "│  └─ ".to_string()),
                (4, "└─ ".to_string()),
            ]
        );
    }

    #[test]
    fn an_orphan_whose_parent_is_missing_is_a_root() {
        let procs = [proc(5, 99), proc(6, 5)];
        let mut t = Tree::new();
        t.build(&procs, |a, b| procs[a].pid.cmp(&procs[b].pid));
        let depths: Vec<(usize, u8)> = t.rows().iter().map(|r| (r.index, r.depth)).collect();
        assert_eq!(depths, vec![(0, 0), (1, 1)]);
    }

    #[test]
    fn a_loop_in_the_parent_links_is_walked_once_and_does_not_hang() {
        let procs = [proc(10, 11), proc(11, 10), proc(12, 0)];
        let mut t = Tree::new();
        t.build(&procs, |a, b| procs[a].pid.cmp(&procs[b].pid));
        let mut seen: Vec<usize> = t.rows().iter().map(|r| r.index).collect();
        seen.sort_unstable();
        assert_eq!(seen, vec![0, 1, 2]);
        assert!(t.rows().iter().all(|r| r.depth <= 1));
    }

    #[test]
    fn a_depth_past_63_is_drawn_as_63_and_the_walk_still_reaches_it() {
        let procs: Vec<Proc> = (1..=100).map(|pid| proc(pid, pid - 1)).collect();
        let mut t = Tree::new();
        t.build(&procs, |a, b| procs[a].pid.cmp(&procs[b].pid));
        assert_eq!(t.rows().len(), 100);
        let last = t.rows().last().copied().unwrap_or(Row {
            index: 0,
            depth: 0,
            more: false,
            lines: 0,
        });
        assert_eq!(last.index, 99);
        assert_eq!(last.depth, MAX_DEPTH);
        let mut s = String::new();
        t.draw(&last, &mut s);
        assert_eq!(s.chars().count(), 62 * 3 + 3);
        assert!(s.ends_with("└─ "));
    }

    #[test]
    fn an_empty_list_gives_no_rows() {
        let mut t = Tree::new();
        t.build(&[], |_, _| Ordering::Equal);
        assert!(t.rows().is_empty());
    }

    #[test]
    fn siblings_follow_the_given_order() {
        let procs = [proc(1, 0), proc(2, 1), proc(3, 1)];
        let mut t = Tree::new();
        t.build(&procs, |a, b| procs[b].pid.cmp(&procs[a].pid));
        let order: Vec<u32> = t.rows().iter().map(|r| procs[r.index].pid).collect();
        assert_eq!(order, vec![1, 3, 2]);
    }
}
