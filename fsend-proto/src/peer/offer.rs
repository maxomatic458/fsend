use std::collections::HashSet;

use serde::Serialize;
use thiserror::Error;

use super::{
    AcceptedFile, CheckedFile, Entry, Node, Offer,
    checksum::{Algorithm, Digest},
    frame,
    limits::{MAX_COMPONENT_BYTES, MAX_DEPTH, MAX_ENTRIES, MAX_PATH_BYTES, MAX_TOTAL_BYTES},
};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OfferError {
    #[error("the offer is empty")]
    Empty,
    #[error("the offer has more than {MAX_ENTRIES} entries")]
    TooManyEntries,
    #[error("the offer is larger than {MAX_TOTAL_BYTES} bytes")]
    TooLarge,
    #[error("the offer does not fit in a message")]
    TooLong,
    #[error("entry {id}: {reason}")]
    Entry { id: usize, reason: EntryError },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EntryError {
    #[error("the path is deeper than {MAX_DEPTH} components")]
    TooDeep,
    #[error("the path is longer than {MAX_PATH_BYTES} bytes")]
    TooLong,
    #[error("{0:?} is not a valid name")]
    Name(String),
    #[error("the name is taken in its directory")]
    Duplicate,
    #[error("its checksum does not fit the offer's checksum algorithm")]
    Checksum,
}

/// Checks the offer rules: safe names, unique within their directory, a
/// checksum of the offer's algorithm on every file if it has one, limits.
pub fn validate(offer: &Offer) -> Result<(), OfferError> {
    if offer.entries.is_empty() {
        return Err(OfferError::Empty);
    }
    let mut rules = Rules {
        digest_len: offer.checksums.map(Algorithm::digest_len),
        entries: 0,
        total: 0,
    };
    rules.check(&offer.entries, 0, 0)
}

/// Checks entries one by one, in offer order.
struct Rules {
    digest_len: Option<usize>,
    /// Entries checked so far.
    entries: usize,
    /// Bytes of the files checked so far.
    total: u64,
}

impl Rules {
    /// Checks the entries of a directory `depth` components deep, whose path
    /// is `path_bytes` long.
    fn check(&mut self, nodes: &[Node], depth: usize, path_bytes: usize) -> Result<(), OfferError> {
        let mut names = HashSet::with_capacity(nodes.len());
        for node in nodes {
            let id = self.entries;
            self.entries += 1;
            if self.entries > MAX_ENTRIES {
                return Err(OfferError::TooManyEntries);
            }
            let invalid = |reason| OfferError::Entry { id, reason };
            let name = node.name();
            if !valid_name(name) {
                return Err(invalid(EntryError::Name(name.chars().take(64).collect())));
            }
            if depth == MAX_DEPTH {
                return Err(invalid(EntryError::TooDeep));
            }
            // Joined with `/`.
            let path_bytes = path_bytes + usize::from(depth > 0) + name.len();
            if path_bytes > MAX_PATH_BYTES {
                return Err(invalid(EntryError::TooLong));
            }
            if !names.insert(name) {
                return Err(invalid(EntryError::Duplicate));
            }
            match node {
                Node::File { size, checksum, .. } => {
                    let len = checksum.as_ref().map(|digest| digest.as_bytes().len());
                    if len != self.digest_len {
                        return Err(invalid(EntryError::Checksum));
                    }
                    self.total = (self.total.checked_add(*size))
                        .filter(|total| *total <= MAX_TOTAL_BYTES)
                        .ok_or(OfferError::TooLarge)?;
                }
                Node::Dir { entries, .. } => self.check(entries, depth + 1, path_bytes)?,
            }
        }
        Ok(())
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_COMPONENT_BYTES
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|c| c == '/' || c == '\\' || c.is_control())
}

impl Offer {
    /// Every entry with its full path, each directory before what it holds;
    /// an entry's id is its index here.
    pub fn flatten(&self) -> Vec<Entry> {
        fn walk(nodes: &[Node], path: &mut Vec<String>, out: &mut Vec<Entry>) {
            for node in nodes {
                path.push(node.name().to_owned());
                match node {
                    Node::File { size, checksum, .. } => out.push(Entry::File {
                        path: path.clone(),
                        size: *size,
                        checksum: checksum.clone(),
                    }),
                    Node::Dir { entries, .. } => {
                        out.push(Entry::Dir { path: path.clone() });
                        walk(entries, path, out);
                    }
                }
                path.pop();
            }
        }
        let mut out = Vec::new();
        walk(&self.entries, &mut Vec::new(), &mut out);
        out
    }

    /// Checksums the files under `algorithm` with `digests`, one per file in
    /// offer order.
    pub fn set_checksums(
        &mut self,
        algorithm: Algorithm,
        digests: impl IntoIterator<Item = Digest>,
    ) {
        fn walk(nodes: &mut [Node], digests: &mut impl Iterator<Item = Digest>) {
            for node in nodes {
                match node {
                    Node::File { checksum, .. } => *checksum = digests.next(),
                    Node::Dir { entries, .. } => walk(entries, digests),
                }
            }
        }
        self.checksums = Some(algorithm);
        walk(&mut self.entries, &mut digests.into_iter());
    }
}

/// [`crate::pairing::Sealed::Offer`] as it goes on the wire, borrowing the offer.
#[derive(Serialize)]
#[serde(tag = "type", rename = "offer")]
struct OfferRef<'a> {
    #[serde(flatten)]
    offer: &'a Offer,
}

/// The plaintext of the sealed `offer`, once it passes [`validate`].
pub fn encode(offer: &Offer) -> Result<Vec<u8>, OfferError> {
    validate(offer)?;
    frame::encode_body(&OfferRef { offer }).map_err(|_| OfferError::TooLong)
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FileListError {
    #[error("entry {0} is not in the offer")]
    UnknownId(u32),
    #[error("entry {0} is a directory")]
    Directory(u32),
    #[error("entry {0} is listed out of order")]
    OutOfOrder(u32),
    #[error("entry {0} is listed past its end")]
    PastEnd(u32),
}

/// Checks an accept against the offer: known files, in offer order, offsets within them.
pub fn check_accept(entries: &[Entry], files: &[AcceptedFile]) -> Result<(), FileListError> {
    check_files(entries, files.iter().map(|file| (file.id, file.offset)))
}

/// Checks a `check` against the offer, like [`check_accept`].
pub fn check_lengths(entries: &[Entry], files: &[CheckedFile]) -> Result<(), FileListError> {
    check_files(entries, files.iter().map(|file| (file.id, file.len)))
}

fn check_files(
    entries: &[Entry],
    files: impl Iterator<Item = (u32, u64)>,
) -> Result<(), FileListError> {
    let mut next = 0;
    for (id, position) in files {
        let index = id as usize;
        match entries.get(index) {
            None => return Err(FileListError::UnknownId(id)),
            Some(_) if index < next => return Err(FileListError::OutOfOrder(id)),
            Some(Entry::Dir { .. }) => return Err(FileListError::Directory(id)),
            Some(Entry::File { size, .. }) if position > *size => {
                return Err(FileListError::PastEnd(id));
            }
            Some(Entry::File { .. }) => next = index + 1,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pairing::Sealed;

    fn file(name: &str, size: u64) -> Node {
        Node::File {
            name: name.into(),
            size,
            checksum: None,
        }
    }

    /// A file with a checksum of `len` bytes.
    fn checked(name: &str, len: usize) -> Node {
        Node::File {
            name: name.into(),
            size: 1,
            checksum: Some(Digest::from(&vec![7; len][..])),
        }
    }

    fn dir(name: &str, entries: Vec<Node>) -> Node {
        Node::Dir {
            name: name.into(),
            entries,
        }
    }

    fn plain(entries: Vec<Node>) -> Offer {
        Offer {
            checksums: None,
            entries,
        }
    }

    fn entry_error(entries: Vec<Node>) -> (usize, EntryError) {
        match validate(&plain(entries)) {
            Err(OfferError::Entry { id, reason }) => (id, reason),
            other => panic!("expected an entry error, got {other:?}"),
        }
    }

    /// `depth` directories, one inside the other, around `inner`.
    fn nested(depth: usize, inner: Vec<Node>) -> Node {
        (1..depth).fold(dir("d", inner), |inside, _| dir("d", vec![inside]))
    }

    #[test]
    fn a_tree_is_valid() {
        let offer = plain(vec![
            file("a.txt", 3),
            dir(
                "d",
                vec![
                    file("b", 0),
                    dir("e", vec![file("c", 9)]),
                    dir("empty", vec![]),
                ],
            ),
            // The same name in other directories.
            dir("e", vec![file("b", 1)]),
        ]);
        assert_eq!(validate(&offer), Ok(()));
    }

    #[test]
    fn bad_names_are_rejected() {
        let long = "x".repeat(MAX_COMPONENT_BYTES + 1);
        for name in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "a\nb",
            "\u{7f}",
            long.as_str(),
        ] {
            let (id, reason) = entry_error(vec![file(name, 1)]);
            assert_eq!(id, 0, "{name:?}");
            assert!(matches!(reason, EntryError::Name(_)), "{name:?}");
        }
        // Unicode and dots inside a name are fine.
        assert_eq!(validate(&plain(vec![file("über..txt", 1)])), Ok(()));
        let longest = "x".repeat(MAX_COMPONENT_BYTES);
        assert_eq!(validate(&plain(vec![file(&longest, 1)])), Ok(()));
    }

    #[test]
    fn names_are_unique_within_a_directory() {
        assert_eq!(
            entry_error(vec![dir("d", vec![]), file("d", 1)]),
            (1, EntryError::Duplicate)
        );
        // Ids count every entry before, in offer order.
        assert_eq!(
            entry_error(vec![dir(
                "d",
                vec![file("a", 1), dir("e", vec![]), file("a", 1)]
            )]),
            (3, EntryError::Duplicate)
        );
    }

    #[test]
    fn limits_hold() {
        assert_eq!(validate(&Offer::default()), Err(OfferError::Empty));

        assert_eq!(validate(&plain(vec![nested(MAX_DEPTH, vec![])])), Ok(()));
        assert_eq!(
            entry_error(vec![nested(MAX_DEPTH, vec![file("f", 1)])]),
            (MAX_DEPTH, EntryError::TooDeep)
        );

        // 20 components of 200 bytes and their separators fit, 21 do not.
        let name = "x".repeat(200);
        let path = |n| (1..n).fold(dir(&name, vec![]), |inside, _| dir(&name, vec![inside]));
        assert_eq!(validate(&plain(vec![path(20)])), Ok(()));
        assert_eq!(entry_error(vec![path(21)]), (20, EntryError::TooLong));

        assert_eq!(
            validate(&plain(vec![file("a", MAX_TOTAL_BYTES), file("b", 1)])),
            Err(OfferError::TooLarge)
        );
        assert_eq!(
            validate(&plain(vec![file("a", u64::MAX), file("b", u64::MAX)])),
            Err(OfferError::TooLarge)
        );
        let many = (0..=MAX_ENTRIES).map(|i| file(&i.to_string(), 0)).collect();
        assert_eq!(validate(&plain(many)), Err(OfferError::TooManyEntries));
    }

    #[test]
    fn every_file_has_a_checksum_of_the_offers_algorithm() {
        let sha256 = |entries| {
            validate(&Offer {
                checksums: Some(Algorithm::Sha256),
                entries,
            })
        };
        let error = |id| {
            Err(OfferError::Entry {
                id,
                reason: EntryError::Checksum,
            })
        };
        // Directories never have one.
        assert_eq!(sha256(vec![dir("d", vec![checked("a", 32)])]), Ok(()));
        assert_eq!(sha256(vec![checked("a", 32), file("b", 1)]), error(1));
        assert_eq!(sha256(vec![checked("a", 20)]), error(0));
        // None without an algorithm.
        assert_eq!(
            validate(&plain(vec![file("a", 1), checked("b", 32)])),
            error(1)
        );
    }

    #[test]
    fn flattening_lists_each_directory_before_what_it_holds() {
        let offer = plain(vec![
            file("a", 3),
            dir("d", vec![file("b", 5), dir("e", vec![file("c", 7)])]),
        ]);
        let path = |path: &[&str]| path.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            offer.flatten(),
            [
                Entry::File {
                    path: path(&["a"]),
                    size: 3,
                    checksum: None
                },
                Entry::Dir { path: path(&["d"]) },
                Entry::File {
                    path: path(&["d", "b"]),
                    size: 5,
                    checksum: None
                },
                Entry::Dir {
                    path: path(&["d", "e"])
                },
                Entry::File {
                    path: path(&["d", "e", "c"]),
                    size: 7,
                    checksum: None
                },
            ]
        );
    }

    #[test]
    fn checksums_go_to_the_files_in_order() {
        let mut offer = plain(vec![file("a", 1), dir("d", vec![file("b", 1)])]);
        let digest = |byte| Digest::from(&[byte; 32][..]);
        offer.set_checksums(Algorithm::Sha256, [digest(1), digest(2)]);
        assert_eq!(offer.checksums, Some(Algorithm::Sha256));
        let checksums: Vec<_> = offer
            .flatten()
            .iter()
            .map(|e| e.checksum().cloned())
            .collect();
        assert_eq!(checksums, [Some(digest(1)), None, Some(digest(2))]);
        assert_eq!(validate(&offer), Ok(()));
    }

    #[test]
    fn an_offer_is_encoded_once_valid() {
        let offer = Offer {
            checksums: Some(Algorithm::Sha256),
            entries: vec![dir("d", vec![checked("a", 32)])],
        };
        let body = encode(&offer).unwrap();
        assert_eq!(frame::decode_body(&body), Ok(Sealed::Offer(offer)));
        assert_eq!(encode(&Offer::default()), Err(OfferError::Empty));
    }

    #[test]
    fn a_tree_as_deep_as_allowed_survives_the_wire() {
        let offer = plain(vec![nested(MAX_DEPTH - 1, vec![file("f", 1)])]);
        let body = encode(&offer).unwrap();
        assert_eq!(frame::decode_body(&body), Ok(Sealed::Offer(offer)));

        // Far deeper, it is refused as it is parsed, before it could exhaust the stack.
        let deep = plain(vec![nested(1000, vec![])]);
        let body = frame::encode_body(&Sealed::Offer(deep)).unwrap();
        assert!(frame::decode_body::<Sealed>(&body).is_err());
    }

    #[test]
    fn accepts_and_checks_are_checked_against_the_offer() {
        let entries = plain(vec![file("a", 10), dir("d", vec![file("b", 5)])]).flatten();
        let accepted = |files: &[(u32, u64)]| {
            let files: Vec<_> = files
                .iter()
                .map(|&(id, offset)| AcceptedFile { id, offset })
                .collect();
            check_accept(&entries, &files)
        };
        assert_eq!(accepted(&[]), Ok(()));
        assert_eq!(accepted(&[(0, 0), (2, 5)]), Ok(()));
        assert_eq!(accepted(&[(3, 0)]), Err(FileListError::UnknownId(3)));
        assert_eq!(accepted(&[(1, 0)]), Err(FileListError::Directory(1)));
        assert_eq!(
            accepted(&[(2, 0), (0, 0)]),
            Err(FileListError::OutOfOrder(0))
        );
        assert_eq!(
            accepted(&[(0, 0), (0, 0)]),
            Err(FileListError::OutOfOrder(0))
        );
        assert_eq!(accepted(&[(0, 11)]), Err(FileListError::PastEnd(0)));

        let checked = [
            CheckedFile { id: 0, len: 10 },
            CheckedFile { id: 2, len: 6 },
        ];
        assert_eq!(check_lengths(&entries, &checked[..1]), Ok(()));
        assert_eq!(
            check_lengths(&entries, &checked),
            Err(FileListError::PastEnd(2))
        );
    }
}
