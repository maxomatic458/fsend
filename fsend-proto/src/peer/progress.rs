use super::{AcceptedFile, Entry};

/// Turns counts of data stream bytes into progress per accepted file.
#[derive(Debug, Clone)]
pub struct Progress {
    /// Each file's id and bytes to move, in stream order.
    files: Vec<(u32, u64)>,
    current: usize,
    used: u64,
}

impl Progress {
    pub fn new(entries: &[Entry], files: &[AcceptedFile]) -> Self {
        let files = files
            .iter()
            .map(|file| (file.id, entries[file.id as usize].size() - file.offset))
            .filter(|&(_, bytes)| bytes > 0)
            .collect();
        Self {
            files,
            current: 0,
            used: 0,
        }
    }

    /// The bytes of each file among the next `n`; nothing past the end.
    pub fn advance(&mut self, mut n: u64) -> Vec<(u32, u64)> {
        let mut steps = Vec::new();
        while n > 0
            && let Some(&(id, len)) = self.files.get(self.current)
        {
            let take = n.min(len - self.used);
            steps.push((id, take));
            n -= take;
            self.used += take;
            if self.used == len {
                self.current += 1;
                self.used = 0;
            }
        }
        steps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_walks_the_accepted_bytes() {
        let file = |size| Entry::File {
            path: vec!["f".into()],
            size,
            checksum: None,
        };
        let entries = [file(10), file(0), file(5), file(4)];
        let at = |id, offset| AcceptedFile { id, offset };
        let mut progress = Progress::new(&entries, &[at(0, 2), at(1, 0), at(2, 0), at(3, 0)]);
        assert_eq!(progress.advance(3), [(0, 3)]);
        // The empty file moves nothing.
        assert_eq!(progress.advance(7), [(0, 5), (2, 2)]);
        assert_eq!(progress.advance(0), []);
        assert_eq!(progress.advance(100), [(2, 3), (3, 4)]);
        assert_eq!(progress.advance(1), []);
    }
}
