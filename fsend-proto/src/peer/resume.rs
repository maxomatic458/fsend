use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use xxhash_rust::xxh3::xxh3_128;

use super::{AcceptedFile, CheckedFile, Entry};

/// Bytes sampled at each end of a fingerprinted range.
pub const SAMPLE_BYTES: u64 = 1024 * 1024;

/// xxh3-128 of the first and last [`SAMPLE_BYTES`] of a file's first `len` bytes.
/// On the wire, 32 lowercase hex digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(type = "string"))]
pub struct Fingerprint(pub u128);

impl Fingerprint {
    /// The `(start, end)` byte ranges of a file's first `len` bytes that are hashed, in order.
    pub fn ranges(len: u64) -> Vec<(u64, u64)> {
        if len <= 2 * SAMPLE_BYTES {
            vec![(0, len)]
        } else {
            vec![(0, SAMPLE_BYTES), (len - SAMPLE_BYTES, len)]
        }
    }

    /// The fingerprint of the bytes in [`Fingerprint::ranges`], concatenated.
    pub fn of(sampled: &[u8]) -> Self {
        Self(xxh3_128(sampled))
    }
}

impl Serialize for Fingerprint {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("{:032x}", self.0))
    }
}

impl<'de> Deserialize<'de> for Fingerprint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let hex = String::deserialize(deserializer)?;
        let valid = hex.len() == 32
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        valid
            .then(|| u128::from_str_radix(&hex, 16).ok())
            .flatten()
            .map(Self)
            .ok_or_else(|| de::Error::custom("a fingerprint is 32 lowercase hex digits"))
    }
}

/// What the receiver has at an entry's path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Local {
    Missing,
    File {
        size: u64,
    },
    Dir,
    /// Anything else, such as a socket.
    Other,
}

/// What to do with files that exist but differ from the offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    Skip,
    Replace,
}

/// Local files that may be a prefix of the offered ones, to fingerprint before planning.
pub fn to_check(entries: &[Entry], local: &[Local]) -> Vec<CheckedFile> {
    entries
        .iter()
        .zip(local)
        .enumerate()
        .filter_map(|(id, (entry, local))| match (entry, local) {
            (Entry::File { size, .. }, Local::File { size: len }) if (1..=*size).contains(len) => {
                Some(CheckedFile {
                    id: id as u32,
                    len: *len,
                })
            }
            _ => None,
        })
        .collect()
}

/// An entry whose local counterpart differs from the offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Conflict {
    pub id: u32,
    /// False when the local entry is of another kind; it, and all below it, are skipped.
    pub replaceable: bool,
}

/// What the receiver does with each entry of an offer.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Plan {
    /// Files to receive whatever the policy: new ones and resumed ones, in offer order.
    pub files: Vec<AcceptedFile>,
    /// Directories to create, in offer order.
    pub dirs: Vec<u32>,
    pub conflicts: Vec<Conflict>,
}

impl Plan {
    /// The files to accept under `policy`; a replaced file starts over.
    pub fn accept(&self, policy: ConflictPolicy) -> Vec<AcceptedFile> {
        let mut files = self.files.clone();
        if policy == ConflictPolicy::Replace {
            files.extend(
                self.conflicts
                    .iter()
                    .filter(|conflict| conflict.replaceable)
                    .map(|conflict| AcceptedFile {
                        id: conflict.id,
                        offset: 0,
                    }),
            );
            files.sort_by_key(|file| file.id);
        }
        files
    }
}

/// Plans the receive of a valid offer. `verified` holds the ids of the checked files whose
/// fingerprints matched: those resume, or are complete. Other existing files
/// conflict, and so does anything in the way of an offered entry.
pub fn plan(entries: &[Entry], local: &[Local], verified: &[u32]) -> Plan {
    let mut plan = Plan::default();
    let verified: HashSet<u32> = verified.iter().copied().collect();
    // Directories whose entries are skipped, since something else is in their place.
    let mut blocked = HashSet::<&[String]>::new();
    for (id, (entry, local)) in entries.iter().zip(local).enumerate() {
        let path = entry.path();
        if blocked.contains(&path[..path.len() - 1]) {
            if entry.is_dir() {
                blocked.insert(path);
            }
            continue;
        }
        let id = id as u32;
        let conflict = |replaceable| Conflict { id, replaceable };
        match (entry, *local) {
            (Entry::Dir { .. }, Local::Missing | Local::Dir) => plan.dirs.push(id),
            (Entry::Dir { path }, _) => {
                plan.conflicts.push(conflict(false));
                blocked.insert(path);
            }
            (Entry::File { .. }, Local::Missing) => plan.files.push(AcceptedFile { id, offset: 0 }),
            (Entry::File { size, .. }, Local::File { size: len }) => {
                let prefix = len == 0 || (len <= *size && verified.contains(&id));
                match prefix {
                    true if len == *size => {}
                    true => plan.files.push(AcceptedFile { id, offset: len }),
                    false => plan.conflicts.push(conflict(true)),
                }
            }
            (Entry::File { .. }, Local::Dir | Local::Other) => plan.conflicts.push(conflict(false)),
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &[&str], size: u64) -> Entry {
        Entry::File {
            path: path.iter().map(|s| s.to_string()).collect(),
            size,
        }
    }

    fn dir(path: &[&str]) -> Entry {
        Entry::Dir {
            path: path.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn at(id: u32, offset: u64) -> AcceptedFile {
        AcceptedFile { id, offset }
    }

    const MIB: u64 = SAMPLE_BYTES;

    #[test]
    fn small_ranges_are_hashed_whole_and_large_ones_at_both_ends() {
        let ranges = Fingerprint::ranges;
        assert_eq!(ranges(0), [(0, 0)]);
        assert_eq!(ranges(MIB), [(0, MIB)]);
        assert_eq!(ranges(2 * MIB), [(0, 2 * MIB)]);
        assert_eq!(ranges(2 * MIB + 1), [(0, MIB), (MIB + 1, 2 * MIB + 1)]);
    }

    #[test]
    fn fingerprints_keep_their_value_and_wire_format() {
        // Pins the hash, so both clients and future versions agree.
        let fingerprint = Fingerprint::of(b"fsend");
        let json = serde_json::to_string(&fingerprint).unwrap();
        assert_eq!(json, r#""e56767a1277b0357a6d715ea3ebead85""#);
        assert_eq!(
            serde_json::from_str::<Fingerprint>(&json).unwrap(),
            fingerprint
        );
        assert_eq!(
            serde_json::to_string(&Fingerprint(1)).unwrap(),
            r#""00000000000000000000000000000001""#
        );
        for bad in [r#""1""#, r#""+0000000000000000000000000000001""#, "1"] {
            assert!(serde_json::from_str::<Fingerprint>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn only_possible_prefixes_are_checked() {
        let entries = [
            file(&["missing"], 10),
            file(&["partial"], 10),
            file(&["whole"], 10),
            file(&["larger"], 10),
            file(&["empty"], 10),
            dir(&["d"]),
        ];
        let local = [
            Local::Missing,
            Local::File { size: 4 },
            Local::File { size: 10 },
            Local::File { size: 11 },
            Local::File { size: 0 },
            Local::File { size: 3 },
        ];
        assert_eq!(
            to_check(&entries, &local),
            [
                CheckedFile { id: 1, len: 4 },
                CheckedFile { id: 2, len: 10 }
            ]
        );
    }

    #[test]
    fn verified_prefixes_resume_and_the_rest_conflict() {
        let entries = [
            file(&["new"], 10),
            file(&["partial"], 10),
            file(&["whole"], 10),
            file(&["changed"], 10),
            file(&["larger"], 10),
            file(&["empty"], 10),
            file(&["was_empty"], 0),
        ];
        let local = [
            Local::Missing,
            Local::File { size: 4 },
            Local::File { size: 10 },
            Local::File { size: 4 },
            Local::File { size: 11 },
            Local::File { size: 0 },
            Local::File { size: 0 },
        ];
        let plan = plan(&entries, &local, &[1, 2]);
        assert_eq!(plan.files, [at(0, 0), at(1, 4), at(5, 0)]);
        assert_eq!(
            plan.conflicts,
            [
                Conflict {
                    id: 3,
                    replaceable: true
                },
                Conflict {
                    id: 4,
                    replaceable: true
                },
            ]
        );
        assert_eq!(plan.accept(ConflictPolicy::Skip), plan.files);
        assert_eq!(
            plan.accept(ConflictPolicy::Replace),
            [at(0, 0), at(1, 4), at(3, 0), at(4, 0), at(5, 0)]
        );
    }

    #[test]
    fn entries_of_another_kind_block_everything_below_them() {
        // Entries of different directories may interleave.
        let entries = [
            dir(&["d"]),
            file(&["d", "a"], 1),
            dir(&["dd"]),
            dir(&["d", "e"]),
            file(&["dd", "c"], 1),
            file(&["d", "e", "b"], 1),
            file(&["f"], 1),
            file(&["s"], 1),
        ];
        let local = [
            Local::File { size: 3 },
            Local::Missing,
            Local::Dir,
            Local::Missing,
            Local::Missing,
            Local::Missing,
            Local::Dir,
            Local::Other,
        ];
        let plan = plan(&entries, &local, &[]);
        assert_eq!(plan.dirs, [2]);
        assert_eq!(plan.files, [at(4, 0)]);
        let blocked = |id| Conflict {
            id,
            replaceable: false,
        };
        assert_eq!(plan.conflicts, [blocked(0), blocked(6), blocked(7)]);
        assert_eq!(plan.accept(ConflictPolicy::Replace), plan.files);
    }
}
