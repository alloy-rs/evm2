//! Prefetches a new fork block's remote cache from the keys of an older cached block, like
//! Foundry.
//!
//! A fork of `latest` gets an empty cache on each new head, and every cold account or storage read
//! then blocks the EVM for one RPC round trip. Earlier runs on the same chain read mostly the same
//! accounts and slots, so the keys of the newest older cache file predict the reads. The keys are
//! read again at the new block through the fork backend, so the cache stays exact for that block.
//! The cache file keeps only the keys the EVM then reads, so a wrong prediction only costs an RPC
//! request and does not carry over to the next block.

use crate::{BlockingMode, JsonBlockCacheData, SharedBackend, cache::read_json};
use alloy_primitives::Address;
use alloy_provider::Network;
use evm2::interpreter::Word;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

/// Prefetch requests in flight at once. The fork backend sends its own cold reads without a limit;
/// this bound keeps a large key set from flooding a public endpoint.
const MAX_CONCURRENT_REQUESTS: usize = 32;

/// Largest number of accounts and slots to prefetch. Older cache files can hold the union of many
/// runs; past this size, a prefetch can cost more requests than it saves.
const MAX_KEYS: usize = 4096;

enum Key {
    Account(Address),
    Slot(Address, Word),
}

/// Starts a background prefetch through `backend` at block `number` if its cache is empty, from
/// the newest older cache file in `chain_dir`.
///
/// Each key is read through the backend like an EVM read, so the backend fetches and caches it with
/// its own request path. Loading the previous cache file runs in the background too, so it does not
/// block the caller. Must run inside a Tokio runtime.
pub(crate) fn spawn<N: Network>(
    backend: &SharedBackend<N>,
    chain_dir: PathBuf,
    number: u64,
) -> Option<Prefetch> {
    if !backend.data().is_empty() {
        return None;
    }
    let runtime = tokio::runtime::Handle::try_current().ok()?;
    let stop = Arc::new(AtomicBool::new(false));
    let (done_tx, done) = mpsc::channel::<()>();
    let backend = backend.with_blocking_mode(BlockingMode::Block);
    let coordinator_stop = Arc::clone(&stop);
    runtime.clone().spawn_blocking(move || {
        let stop = coordinator_stop;
        if let Some(path) = previous_cache_file(&chain_dir, number)
            && let Ok(previous) = read_json(&path)
            && !stop.load(Ordering::Relaxed)
        {
            let keys = keys(&previous);
            tracing::debug!(target: "evm2::fork", path = %path.display(), keys = keys.len(), "prefetching fork cache");
            // Workers take the next key without a lock held during a read.
            let keys = Arc::new(Mutex::new(keys.into_iter()));
            for _ in 0..MAX_CONCURRENT_REQUESTS {
                let (keys, stop, done_tx) = (Arc::clone(&keys), Arc::clone(&stop), done_tx.clone());
                let backend = backend.clone();
                runtime.spawn_blocking(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let next = keys.lock().unwrap_or_else(PoisonError::into_inner).next();
                        let Some(key) = next else { break };
                        // A prefetched key stays out of the cache file until the EVM reads it.
                        let read = match key {
                            Key::Account(address) => backend.prefetch_basic(address),
                            Key::Slot(address, slot) => backend.prefetch_storage(address, slot),
                        };
                        // The EVM fetches any key it still needs, so stop at the first failure.
                        if read.is_err() {
                            break;
                        }
                    }
                    // Drop the backend clone before reporting done, as below.
                    drop(backend);
                    drop(done_tx);
                });
            }
        }
        // The last backend clone flushes the disk cache, so drop it before reporting done.
        drop(backend);
        drop(done_tx);
    });
    Some(Prefetch { stop, done })
}

/// Background prefetch workers of one fork backend.
#[derive(Debug)]
pub(crate) struct Prefetch {
    stop: Arc<AtomicBool>,
    done: mpsc::Receiver<()>,
}

impl Prefetch {
    /// Stops the workers and waits until they and their coordinator exit.
    ///
    /// Blocks the calling thread, so never call it on the thread running the fork's handler.
    pub(crate) fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        // Returns once every worker has dropped its sender.
        let _ = self.done.recv();
    }
}

/// Returns the cache file of the newest block below `number` in a chain cache directory.
fn previous_cache_file(dir: &Path, number: u64) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let block = entry.file_name().to_str()?.parse::<u64>().ok()?;
            (block < number && entry.file_type().ok()?.is_file()).then_some((block, entry.path()))
        })
        .max_by_key(|(block, _)| *block)
        .map(|(_, path)| path)
}

/// Returns the accounts first, then the slots, up to [`MAX_KEYS`].
fn keys(data: &JsonBlockCacheData) -> Vec<Key> {
    data.accounts
        .keys()
        .map(|&address| Key::Account(address))
        .chain(
            data.storage.iter().flat_map(|(&address, slots)| {
                slots.keys().map(move |&slot| Key::Slot(address, slot))
            }),
        )
        .take(MAX_KEYS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_newest_cache_file_below_block() {
        let dir = std::env::temp_dir().join(format!("evm2-fork-prefetch-{}", std::process::id()));
        fs::create_dir_all(dir.join("120")).unwrap();
        for block in ["90", "100", "110", "130", "notes"] {
            fs::write(dir.join(block), b"{}").unwrap();
        }

        // `120` is a directory, as anvil writes it, and `130` is not below the block.
        assert_eq!(previous_cache_file(&dir, 125), Some(dir.join("110")));
        assert_eq!(previous_cache_file(&dir, 90), None);
        fs::remove_dir_all(&dir).unwrap();
    }
}
