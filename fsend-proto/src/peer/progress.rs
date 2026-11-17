use std::collections::HashMap;

use serde::Serialize;

use super::{AcceptedFile, Entry};

/// A top-level entry of an offer, with the size of everything below it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TopEntry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
}

/// The top-level entries of a valid offer, and for each entry the index of the one it lies in.
pub fn top_entries(entries: &[Entry]) -> (Vec<TopEntry>, Vec<usize>) {
    let mut tops = Vec::new();
    let mut index = HashMap::<&str, usize>::new();
    let top_of = entries
        .iter()
        .map(|entry| {
            let name = &entry.path()[0];
            let top = *index.entry(name).or_insert_with(|| {
                tops.push(TopEntry {
                    name: name.clone(),
                    dir: entry.is_dir(),
                    size: 0,
                });
                tops.len() - 1
            });
            tops[top].size += entry.size();
            top
        })
        .collect();
    (tops, top_of)
}

/// Bytes of each top-level entry already present once `files` are accepted.
pub fn present(entries: &[Entry], files: &[AcceptedFile]) -> Vec<u64> {
    let (tops, top_of) = top_entries(entries);
    let mut present: Vec<u64> = tops.iter().map(|top| top.size).collect();
    for file in files {
        let id = file.id as usize;
        present[top_of[id]] -= entries[id].size() - file.offset;
    }
    present
}

/// Turns counts of data stream bytes into progress per top-level entry, in stream order.
#[derive(Debug, Clone)]
pub struct Progress {
    /// The top-level entry of each run of accepted bytes, and its length.
    runs: Vec<(usize, u64)>,
    run: usize,
    /// Bytes of the current run already counted.
    used: u64,
}

impl Progress {
    /// For `files` accepted of `entries`, with `top_of` from [`top_entries`].
    pub fn new(entries: &[Entry], files: &[AcceptedFile], top_of: &[usize]) -> Self {
        let mut runs = Vec::<(usize, u64)>::new();
        for file in files {
            let id = file.id as usize;
            let bytes = entries[id].size() - file.offset;
            match runs.last_mut() {
                Some((top, run)) if *top == top_of[id] => *run += bytes,
                _ if bytes > 0 => runs.push((top_of[id], bytes)),
                _ => {}
            }
        }
        Self {
            runs,
            run: 0,
            used: 0,
        }
    }

    /// The bytes of each top-level entry among the next `n`; nothing past the end.
    pub fn advance(&mut self, mut n: u64) -> Vec<(usize, u64)> {
        let mut steps = Vec::new();
        while n > 0
            && let Some(&(top, len)) = self.runs.get(self.run)
        {
            let take = n.min(len - self.used);
            steps.push((top, take));
            n -= take;
            self.used += take;
            if self.used == len {
                self.run += 1;
                self.used = 0;
            }
        }
        steps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &[&str], size: u64) -> Entry {
        Entry::File {
            path: path.iter().map(|p| p.to_string()).collect(),
            size,
        }
    }

    fn dir(path: &[&str]) -> Entry {
        Entry::Dir {
            path: path.iter().map(|p| p.to_string()).collect(),
        }
    }

    #[test]
    fn progress_counts_per_top_level_entry() {
        let entries = [
            file(&["a"], 10),
            dir(&["d"]),
            dir(&["e"]),
            file(&["e", "x"], 3),
            file(&["d", "b"], 5),
            dir(&["d", "f"]),
            file(&["d", "f", "c"], 7),
        ];
        let (tops, top_of) = top_entries(&entries);
        let top = |name: &str, dir, size| TopEntry {
            name: name.into(),
            dir,
            size,
        };
        assert_eq!(
            tops,
            [top("a", false, 10), top("d", true, 12), top("e", true, 3)]
        );
        // Entries of different top-level entries may interleave.
        assert_eq!(top_of, [0, 1, 2, 2, 1, 1, 1]);

        let files = [
            AcceptedFile { id: 0, offset: 4 },
            AcceptedFile { id: 6, offset: 0 },
        ];
        assert_eq!(present(&entries, &files), [4, 5, 3]);
        assert_eq!(present(&entries, &[]), [10, 12, 3]);
    }

    #[test]
    fn progress_walks_the_accepted_bytes() {
        let file = |path: &str, size| Entry::File {
            path: vec![path.into()],
            size,
        };
        let entries = [file("a", 10), file("b", 0), file("c", 5), file("d", 4)];
        let files = [
            AcceptedFile { id: 0, offset: 2 },
            AcceptedFile { id: 1, offset: 0 },
            AcceptedFile { id: 2, offset: 0 },
            AcceptedFile { id: 3, offset: 0 },
        ];
        // a and b share a top-level entry, c and d do not.
        let mut progress = Progress::new(&entries, &files, &[0, 0, 1, 2]);
        assert_eq!(progress.advance(3), [(0, 3)]);
        assert_eq!(progress.advance(7), [(0, 5), (1, 2)]);
        assert_eq!(progress.advance(0), []);
        assert_eq!(progress.advance(100), [(1, 3), (2, 4)]);
        assert_eq!(progress.advance(1), []);
    }
}
