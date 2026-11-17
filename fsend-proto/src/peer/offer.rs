use std::collections::HashMap;

use serde::Serialize;
use thiserror::Error;

use super::{
    AcceptedFile, CheckedFile, Entry, frame,
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
    #[error("the path is empty")]
    EmptyPath,
    #[error("the path is deeper than {MAX_DEPTH} components")]
    TooDeep,
    #[error("the path is longer than {MAX_PATH_BYTES} bytes")]
    TooLong,
    #[error("{0:?} is not a valid name")]
    Name(String),
    #[error("its parent directory is not offered before it")]
    NoParent,
    #[error("the path is offered twice")]
    Duplicate,
}

/// Checks the offer rules: safe names, parents before children, unique paths, limits.
pub fn validate(entries: &[Entry]) -> Result<(), OfferError> {
    if entries.is_empty() {
        return Err(OfferError::Empty);
    }
    if entries.len() > MAX_ENTRIES {
        return Err(OfferError::TooManyEntries);
    }
    // Path -> whether it is a directory.
    let mut seen = HashMap::<&[String], bool>::with_capacity(entries.len());
    let mut total = 0u64;
    for (id, entry) in entries.iter().enumerate() {
        let invalid = |reason| OfferError::Entry { id, reason };
        let path = entry.path();
        check_path(path).map_err(invalid)?;
        if let [parent @ .., _] = path
            && !parent.is_empty()
            && seen.get(parent) != Some(&true)
        {
            return Err(invalid(EntryError::NoParent));
        }
        if seen.insert(path, entry.is_dir()).is_some() {
            return Err(invalid(EntryError::Duplicate));
        }
        total = total
            .checked_add(entry.size())
            .filter(|total| *total <= MAX_TOTAL_BYTES)
            .ok_or(OfferError::TooLarge)?;
    }
    Ok(())
}

fn check_path(path: &[String]) -> Result<(), EntryError> {
    if path.is_empty() {
        return Err(EntryError::EmptyPath);
    }
    if path.len() > MAX_DEPTH {
        return Err(EntryError::TooDeep);
    }
    if let Some(name) = path.iter().find(|name| !valid_name(name)) {
        return Err(EntryError::Name(name.chars().take(64).collect()));
    }
    let joined = path.iter().map(String::len).sum::<usize>() + path.len() - 1;
    if joined > MAX_PATH_BYTES {
        return Err(EntryError::TooLong);
    }
    Ok(())
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

/// [`super::SenderMessage::Offer`] as it goes on the wire, borrowing its entries.
#[derive(Serialize)]
#[serde(tag = "type", rename = "offer")]
struct OfferRef<'a> {
    entries: &'a [Entry],
}

/// The control frame of an offer of `entries`, once they pass [`validate`].
pub fn encode(entries: &[Entry]) -> Result<Vec<u8>, OfferError> {
    validate(entries)?;
    frame::encode(&OfferRef { entries }).map_err(|_| OfferError::TooLong)
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
    use crate::peer::SenderMessage;

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

    fn entry_error(entries: &[Entry]) -> (usize, EntryError) {
        match validate(entries) {
            Err(OfferError::Entry { id, reason }) => (id, reason),
            other => panic!("expected an entry error, got {other:?}"),
        }
    }

    #[test]
    fn a_tree_in_parent_first_order_is_valid() {
        let entries = [
            file(&["a.txt"], 3),
            dir(&["d"]),
            file(&["d", "b"], 0),
            dir(&["d", "e"]),
            file(&["d", "e", "c"], 9),
            dir(&["empty"]),
        ];
        assert_eq!(validate(&entries), Ok(()));
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
            let (id, reason) = entry_error(&[file(&[name], 1)]);
            assert_eq!(id, 0, "{name:?}");
            assert!(matches!(reason, EntryError::Name(_)), "{name:?}");
        }
        // Unicode and dots inside a name are fine.
        assert_eq!(validate(&[file(&["über..txt"], 1)]), Ok(()));
        assert_eq!(validate(&[file(&["x".repeat(255).as_str()], 1)]), Ok(()));
    }

    #[test]
    fn structure_rules_hold() {
        assert_eq!(
            entry_error(&[file(&["d", "a"], 1)]),
            (0, EntryError::NoParent)
        );
        // The parent must come first and be a directory.
        assert_eq!(
            entry_error(&[file(&["d", "a"], 1), dir(&["d"])]),
            (0, EntryError::NoParent)
        );
        assert_eq!(
            entry_error(&[file(&["d"], 1), file(&["d", "a"], 1)]),
            (1, EntryError::NoParent)
        );
        assert_eq!(
            entry_error(&[dir(&["d"]), file(&["d"], 1)]),
            (1, EntryError::Duplicate)
        );
        assert_eq!(entry_error(&[dir(&[])]), (0, EntryError::EmptyPath));
    }

    #[test]
    fn limits_hold() {
        assert_eq!(validate(&[]), Err(OfferError::Empty));

        let deep: Vec<String> = (0..=MAX_DEPTH).map(|i| i.to_string()).collect();
        let mut entries: Vec<Entry> = (1..=deep.len())
            .map(|n| Entry::Dir {
                path: deep[..n].to_vec(),
            })
            .collect();
        assert_eq!(entry_error(&entries), (MAX_DEPTH, EntryError::TooDeep));
        entries.truncate(MAX_DEPTH);
        assert_eq!(validate(&entries), Ok(()));

        // 20 components of 200 bytes and their separators fit, 21 do not.
        let name = "x".repeat(200);
        let nested: Vec<Entry> = (1..=21)
            .map(|n| Entry::Dir {
                path: vec![name.clone(); n],
            })
            .collect();
        assert_eq!(validate(&nested[..20]), Ok(()));
        assert_eq!(entry_error(&nested), (20, EntryError::TooLong));

        assert_eq!(
            validate(&[file(&["a"], MAX_TOTAL_BYTES), file(&["b"], 1)]),
            Err(OfferError::TooLarge)
        );
        assert_eq!(
            validate(&[file(&["a"], u64::MAX), file(&["b"], u64::MAX)]),
            Err(OfferError::TooLarge)
        );
    }

    #[test]
    fn an_offer_is_encoded_once_valid() {
        let entries = [dir(&["d"]), file(&["d", "a"], 1)];
        let frame = encode(&entries).unwrap();
        let mut decoder = frame::FrameDecoder::default();
        decoder.push(&frame);
        assert_eq!(
            decoder.next_message(),
            Ok(Some(SenderMessage::Offer {
                entries: entries.to_vec()
            }))
        );
        assert_eq!(encode(&[]), Err(OfferError::Empty));
    }

    #[test]
    fn accepts_and_checks_are_checked_against_the_offer() {
        let entries = [file(&["a"], 10), dir(&["d"]), file(&["d", "b"], 5)];
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
