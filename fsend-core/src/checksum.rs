use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use fsend_proto::peer::checksum::{Algorithm, Hasher};
use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;

use crate::Error;
use crate::transfer::Outgoing;

/// Files hashed in parallel.
const HASHERS: usize = 4;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
const CHUNK_BYTES: usize = 1024 * 1024;

impl Outgoing {
    /// `on_progress` gets the bytes hashed so far, of [`Outgoing::size`].
    pub async fn add_checksums(
        &mut self,
        algorithm: Algorithm,
        on_progress: &mut (dyn FnMut(u64) + Send),
    ) -> Result<(), Error> {
        let entries = self.offer.flatten();
        let jobs = (entries.iter().zip(&self.sources))
            .filter(|(entry, _)| !entry.is_dir())
            .map(|(entry, source)| (source.clone(), entry.size()))
            .collect();
        let hashers = hash_files(algorithm, jobs, on_progress).await?;
        let digests = hashers.into_iter().map(|hasher| hasher.finish());
        self.offer.set_checksums(algorithm, digests);
        self.encode()
    }
}

#[derive(Default)]
struct Shared {
    done: AtomicU64,
    stop: AtomicBool,
}

/// Stops the hashing threads when dropped.
struct StopOnDrop(Arc<Shared>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.stop.store(true, Ordering::Relaxed);
    }
}

/// The hashers of each `(path, len)` in order, to go on with or finish.
pub(crate) async fn hash_files(
    algorithm: Algorithm,
    jobs: Vec<(PathBuf, u64)>,
    on_progress: &mut (dyn FnMut(u64) + Send),
) -> io::Result<Vec<Hasher>> {
    let shared = Arc::new(Shared::default());
    let _stop = StopOnDrop(shared.clone());
    let mut hashers: Vec<Option<Hasher>> = vec![None; jobs.len()];
    let mut jobs = jobs.into_iter().enumerate();
    let mut running = FuturesUnordered::new();
    let mut tick = tokio::time::interval(PROGRESS_INTERVAL);
    loop {
        while running.len() < HASHERS
            && let Some((index, (path, len))) = jobs.next()
        {
            let shared = shared.clone();
            running.push(tokio::task::spawn_blocking(move || {
                (index, hash_file(algorithm.hasher(), &path, len, &shared))
            }));
        }
        tokio::select! {
            next = running.next() => match next {
                Some(joined) => {
                    let (index, hashed) = joined.map_err(io::Error::other)?;
                    hashers[index] = Some(hashed?);
                }
                None => break,
            },
            _ = tick.tick() => on_progress(shared.done.load(Ordering::Relaxed)),
        }
    }
    on_progress(shared.done.load(Ordering::Relaxed));
    Ok(hashers
        .into_iter()
        .map(|hasher| hasher.expect("every job finished"))
        .collect())
}

fn hash_file(mut hasher: Hasher, path: &Path, len: u64, shared: &Shared) -> io::Result<Hasher> {
    // A file received from the start may not exist yet.
    if len == 0 {
        return Ok(hasher);
    }
    let mut file = std::fs::File::open(path)?.take(len);
    let mut buf = vec![0; CHUNK_BYTES];
    let mut read = 0;
    while !shared.stop.load(Ordering::Relaxed) {
        let n = match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        hasher.update(&buf[..n]);
        read += n as u64;
        shared.done.fetch_add(n as u64, Ordering::Relaxed);
    }
    if read != len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("{} is shorter than {len} bytes", path.display()),
        ));
    }
    Ok(hasher)
}
