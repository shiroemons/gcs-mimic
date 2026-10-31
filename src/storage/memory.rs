//! Process-local immutable blobs. Cloned response chunks retain their budget leases.

use bytes::Bytes;
use std::{
    collections::HashMap,
    io::{self, Read, Seek, SeekFrom},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

const CHUNK_OVERHEAD: u64 = 128;
const BLOCK_SIZE: u64 = 64 * 1024;

struct Budget {
    limit: u64,
    used: AtomicU64,
}

struct Chunk {
    bytes: Vec<u8>,
    reservation: Reservation,
}

struct Reservation {
    budget: Arc<Budget>,
    charge: u64,
}

impl Reservation {
    fn resize(&mut self, charge: u64) -> io::Result<()> {
        if charge > self.charge {
            let additional = charge - self.charge;
            self.budget
                .used
                .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                    used.checked_add(additional)
                        .filter(|used| *used <= self.budget.limit)
                })
                .map_err(|_| exhausted())?;
        } else {
            self.budget
                .used
                .fetch_sub(self.charge - charge, Ordering::AcqRel);
        }
        self.charge = charge;
        Ok(())
    }
}

impl Chunk {
    fn compact(&mut self) {
        if self.bytes.len() == self.bytes.capacity() {
            return;
        }
        let mut replacement = Reservation {
            budget: self.reservation.budget.clone(),
            charge: 0,
        };
        // Compaction is optional: keep charging the old capacity if a second buffer cannot fit.
        if replacement.resize(self.bytes.len() as u64).is_err() {
            return;
        }
        let mut bytes = Vec::new();
        if bytes.try_reserve_exact(self.bytes.len()).is_err()
            || bytes.capacity() >= self.bytes.capacity()
            || replacement.resize(bytes.capacity() as u64).is_err()
        {
            return;
        }
        bytes.extend_from_slice(&self.bytes);
        let previous = self.bytes.capacity() as u64;
        self.bytes = bytes;
        self.reservation.charge = replacement.charge + CHUNK_OVERHEAD;
        replacement.charge = 0;
        self.reservation
            .budget
            .used
            .fetch_sub(previous, Ordering::AcqRel);
    }
}

impl AsRef<[u8]> for Chunk {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.charge, Ordering::AcqRel);
    }
}

pub(super) struct Store {
    budget: Arc<Budget>,
    blobs: Mutex<HashMap<String, Arc<Blob>>>,
    drafts: Mutex<HashMap<String, Box<Stage>>>,
    failed: AtomicBool,
}

pub(super) struct Blob {
    chunks: Vec<Bytes>,
    size: u64,
    _reservation: Reservation,
}

pub(super) struct Stage {
    store: Weak<Store>,
    chunks: Vec<Bytes>,
    size: u64,
    pending: Option<Chunk>,
    reservation: Reservation,
    hasher: Option<crate::hashing::Hasher>,
    fixed_charge: u64,
}

impl Store {
    pub(super) fn new(limit: u64) -> Arc<Self> {
        Arc::new(Self {
            budget: Arc::new(Budget {
                limit,
                used: AtomicU64::new(0),
            }),
            blobs: Mutex::new(HashMap::new()),
            drafts: Mutex::new(HashMap::new()),
            failed: AtomicBool::new(false),
        })
    }

    pub(super) fn is_healthy(&self) -> bool {
        !self.blobs.is_poisoned()
            && !self.drafts.is_poisoned()
            && !self.failed.load(Ordering::Acquire)
    }

    pub(super) fn stage(self: &Arc<Self>) -> io::Result<Stage> {
        self.budget
            .used
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(CHUNK_OVERHEAD)
                    .filter(|used| *used <= self.budget.limit)
            })
            .map_err(|_| exhausted())?;
        Ok(Stage {
            store: Arc::downgrade(self),
            chunks: Vec::new(),
            size: 0,
            pending: None,
            reservation: Reservation {
                budget: self.budget.clone(),
                charge: CHUNK_OVERHEAD,
            },
            hasher: None,
            fixed_charge: CHUNK_OVERHEAD,
        })
    }

    pub(super) fn open(&self, id: &str) -> io::Result<Reader> {
        let blobs = self
            .blobs
            .lock()
            .map_err(|_| io::Error::other("RAM blob store lock poisoned"))?;
        let blob = blobs
            .get(id)
            .cloned()
            .ok_or_else(|| io::Error::other("missing RAM blob"))?;
        Ok(Reader {
            blob,
            index: 0,
            within: 0,
            position: 0,
        })
    }

    pub(super) fn remove(&self, id: &str) -> io::Result<()> {
        self.blobs
            .lock()
            .map_err(|_| io::Error::other("RAM blob store lock poisoned"))?
            .remove(id);
        Ok(())
    }

    pub(super) fn clear(&self) -> io::Result<()> {
        self.drafts
            .lock()
            .map_err(|_| io::Error::other("RAM draft store lock poisoned"))?
            .clear();
        self.blobs
            .lock()
            .map_err(|_| io::Error::other("RAM blob store lock poisoned"))?
            .clear();
        Ok(())
    }

    pub(super) fn retain_ids(&self, selected: &std::collections::HashSet<&str>) -> io::Result<()> {
        self.drafts
            .lock()
            .map_err(|_| io::Error::other("RAM draft store lock poisoned"))?
            .clear();
        self.blobs
            .lock()
            .map_err(|_| io::Error::other("RAM blob store lock poisoned"))?
            .retain(|id, _| selected.contains(id.as_str()));
        Ok(())
    }
}

impl Stage {
    pub(super) fn write(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            if self.pending.is_none() {
                self.pending = Some(self.allocate(bytes.len().min(BLOCK_SIZE as usize) as u64)?);
            }
            let Some(chunk) = &mut self.pending else {
                return Err(io::Error::other("missing RAM staging block"));
            };
            let length = bytes.len().min(chunk.bytes.capacity() - chunk.bytes.len());
            chunk.bytes.extend_from_slice(&bytes[..length]);
            self.size += length as u64;
            bytes = &bytes[length..];
            if chunk.bytes.len() == chunk.bytes.capacity() {
                self.flush_pending()?;
            }
        }
        Ok(())
    }

    fn allocate(&self, required: u64) -> io::Result<Chunk> {
        let budget = &self.reservation.budget;
        let used = budget
            .used
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                let available = budget
                    .limit
                    .checked_sub(used)?
                    .checked_sub(CHUNK_OVERHEAD)?;
                let capacity = if available >= BLOCK_SIZE {
                    BLOCK_SIZE
                } else {
                    required.min(available)
                };
                (capacity > 0).then_some(used + CHUNK_OVERHEAD + capacity)
            })
            .map_err(|_| exhausted())?;
        let available = budget.limit - used - CHUNK_OVERHEAD;
        let capacity = if available >= BLOCK_SIZE {
            BLOCK_SIZE
        } else {
            required.min(available)
        };
        let mut chunk = Chunk {
            bytes: Vec::new(),
            reservation: Reservation {
                budget: budget.clone(),
                charge: capacity + CHUNK_OVERHEAD,
            },
        };
        chunk
            .bytes
            .try_reserve_exact(capacity as usize)
            .map_err(io::Error::other)?;
        chunk
            .reservation
            .resize(chunk.bytes.capacity() as u64 + CHUNK_OVERHEAD)?;
        Ok(chunk)
    }

    fn flush_pending(&mut self) -> io::Result<()> {
        if self.pending.is_some() {
            self.reserve_chunk()?;
        }
        let Some(mut chunk) = self.pending.take() else {
            return Ok(());
        };
        chunk.compact();
        self.chunks.push(Bytes::from_owner(chunk));
        Ok(())
    }

    fn reserve_chunk(&mut self) -> io::Result<()> {
        if self.chunks.len() < self.chunks.capacity() {
            return Ok(());
        }
        let capacity = self
            .chunks
            .capacity()
            .checked_mul(2)
            .ok_or_else(exhausted)?
            .max(4);
        let bytes = |capacity: usize| {
            (capacity as u64)
                .checked_mul(std::mem::size_of::<Bytes>() as u64)
                .ok_or_else(exhausted)
        };
        let unused = CHUNK_OVERHEAD.saturating_sub(bytes(self.chunks.capacity())?);
        let mut replacement = Reservation {
            budget: self.reservation.budget.clone(),
            charge: 0,
        };
        replacement.resize(bytes(capacity)?.saturating_sub(unused))?;
        let mut chunks = Vec::new();
        chunks
            .try_reserve_exact(capacity)
            .map_err(io::Error::other)?;
        replacement.resize(bytes(chunks.capacity())?.saturating_sub(unused))?;
        let retained = self.fixed_charge + bytes(chunks.capacity())?.saturating_sub(CHUNK_OVERHEAD);
        let released = replacement
            .charge
            .checked_add(self.reservation.charge)
            .and_then(|total| total.checked_sub(retained))
            .ok_or_else(exhausted)?;
        chunks.append(&mut self.chunks);
        self.chunks = chunks;
        // Keep the table charged across truncation; release the replaced allocation's lease.
        replacement.charge = released;
        self.reservation.charge = retained;
        Ok(())
    }

    pub(super) fn publish(mut self, id: String) -> io::Result<()> {
        self.flush_pending()?;
        let store = self
            .store
            .upgrade()
            .ok_or_else(|| io::Error::other("RAM store is closed"))?;
        let mut blobs = store
            .blobs
            .lock()
            .map_err(|_| io::Error::other("RAM blob store lock poisoned"))?;
        if blobs.contains_key(&id) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        blobs.try_reserve(1).map_err(io::Error::other)?;
        blobs.insert(
            id,
            Arc::new(Blob {
                chunks: self.chunks,
                size: self.size,
                _reservation: self.reservation,
            }),
        );
        Ok(())
    }
}

mod transfer;
pub(super) use transfer::{Checkpoint, Draft};

fn exhausted() -> io::Error {
    io::Error::new(
        io::ErrorKind::StorageFull,
        "RAM blob byte budget exhausted; delete objects or increase memory_max_bytes",
    )
}

pub(crate) struct Reader {
    blob: Arc<Blob>,
    index: usize,
    within: usize,
    position: u64,
}

impl Reader {
    pub(crate) fn next_chunk(&mut self, limit: usize) -> Option<Bytes> {
        while let Some(chunk) = self.blob.chunks.get(self.index) {
            if self.within == chunk.len() {
                self.index += 1;
                self.within = 0;
                continue;
            }
            let end = self.within.saturating_add(limit).min(chunk.len());
            let bytes = chunk.slice(self.within..end);
            self.position += bytes.len() as u64;
            self.within = end;
            return Some(bytes);
        }
        None
    }
}

impl Read for Reader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let Some(bytes) = self.next_chunk(output.len()) else {
            return Ok(0);
        };
        output[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }
}

impl Seek for Reader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let position = match from {
            SeekFrom::Start(value) => Some(value),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
            SeekFrom::End(offset) => self.blob.size.checked_add_signed(offset),
        }
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        self.index = 0;
        self.within = 0;
        self.position = position;
        let mut remaining = position;
        while let Some(chunk) = self.blob.chunks.get(self.index) {
            if remaining < chunk.len() as u64 {
                self.within = remaining as usize;
                break;
            }
            remaining -= chunk.len() as u64;
            self.index += 1;
        }
        Ok(position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_namespace_preserves_new_blobs_and_charges_old_reader_references() {
        let store = Store::new(2048);
        for (id, body) in [("old", b"old"), ("new", b"new")] {
            let mut stage = store.stage().unwrap();
            stage.write(body).unwrap();
            stage.publish(id.into()).unwrap();
        }
        let mut reader = store.open("old").unwrap();
        let chunk = reader.next_chunk(3).unwrap();
        let used = store.budget.used.load(Ordering::Acquire);
        store
            .retain_ids(&std::collections::HashSet::from(["new"]))
            .unwrap();
        assert!(store.open("old").is_err());
        assert_eq!(
            store.open("new").unwrap().next_chunk(3).unwrap(),
            b"new"[..]
        );
        assert_eq!(store.budget.used.load(Ordering::Acquire), used);
        drop(reader);
        let chunk_held = store.budget.used.load(Ordering::Acquire);
        assert!(chunk_held < used);
        assert_eq!(chunk, b"old"[..]);
        drop(chunk);
        assert!(store.budget.used.load(Ordering::Acquire) < chunk_held);
        store.remove("new").unwrap();
        assert_eq!(store.budget.used.load(Ordering::Acquire), 0);
    }

    #[test]
    fn readers_and_response_chunks_retain_budget_after_delete() {
        let store = Store::new(260);
        let mut stage = store.stage().unwrap();
        stage.write(b"body").unwrap();
        stage.publish("blob".into()).unwrap();
        let mut reader = store.open("blob").unwrap();
        let bytes = reader.next_chunk(4).unwrap();
        store.remove("blob").unwrap();
        drop(reader);
        assert_eq!(
            store.stage().unwrap().write(b"new").unwrap_err().kind(),
            io::ErrorKind::StorageFull
        );
        drop(bytes);
        store.stage().unwrap().write(b"new").unwrap();
    }

    #[test]
    fn aborted_stage_releases_budget_and_seek_reads_selected_bytes() {
        let store = Store::new(500);
        let mut stage = store.stage().unwrap();
        stage.write(b"first").unwrap();
        stage.write(b"second").unwrap();
        assert!(stage.write(&[0; 270]).is_err());
        drop(stage);
        let mut stage = store.stage().unwrap();
        stage.write(b"first").unwrap();
        stage.write(b"second").unwrap();
        stage.publish("blob".into()).unwrap();
        let mut reader = store.open("blob").unwrap();
        reader.seek(SeekFrom::Start(3)).unwrap();
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"stsecond");
        reader.seek(SeekFrom::End(-3)).unwrap();
        assert_eq!(reader.next_chunk(2).unwrap(), b"on"[..]);
        assert!(reader.seek(SeekFrom::Current(-100)).is_err());
    }

    #[test]
    fn tiny_frames_are_charged_and_sliced_input_does_not_pin_a_large_backing() {
        struct Owner {
            bytes: Vec<u8>,
            dropped: Arc<std::sync::atomic::AtomicBool>,
        }
        impl AsRef<[u8]> for Owner {
            fn as_ref(&self) -> &[u8] {
                &self.bytes
            }
        }
        impl Drop for Owner {
            fn drop(&mut self) {
                self.dropped.store(true, Ordering::Release);
            }
        }
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let input = Bytes::from_owner(Owner {
            bytes: vec![7; 1024 * 1024],
            dropped: dropped.clone(),
        })
        .slice(..1);
        let store = Store::new(BLOCK_SIZE + 2 * CHUNK_OVERHEAD);
        let mut stage = store.stage().unwrap();
        stage.write(&input).unwrap();
        drop(input);
        assert!(dropped.load(Ordering::Acquire));
        for _ in 0..(BLOCK_SIZE - 1) {
            stage.write(b"b").unwrap();
        }
        assert_eq!(stage.chunks.len(), 1);
        assert_eq!(
            stage.write(b"c").unwrap_err().kind(),
            io::ErrorKind::StorageFull
        );
        stage.publish("tiny".into()).unwrap();
        store.remove("tiny").unwrap();
        store.stage().unwrap().write(b"reclaimed").unwrap();
    }

    #[test]
    fn empty_blobs_consume_their_registry_reservation() {
        let store = Store::new(128);
        store.stage().unwrap().publish("empty".into()).unwrap();
        assert!(store.stage().is_err());
        store.remove("empty").unwrap();
        store.stage().unwrap().publish("new".into()).unwrap();
    }

    #[test]
    fn concurrent_staging_reservations_never_exceed_the_shared_limit() {
        let store = Store::new(2 * BLOCK_SIZE + 1024);
        let start = Arc::new(std::sync::Barrier::new(4));
        let finish = Arc::new(std::sync::Barrier::new(4));
        let mut workers = Vec::new();
        for _ in 0..4 {
            let (store, start, finish) = (store.clone(), start.clone(), finish.clone());
            workers.push(
                crate::task::spawn_thread("ram-capacity-test", move || {
                    let mut stage = store.stage().unwrap();
                    start.wait();
                    let success = stage.write(&vec![1; BLOCK_SIZE as usize]).is_ok();
                    finish.wait();
                    assert!(store.budget.used.load(Ordering::Acquire) <= store.budget.limit);
                    success
                })
                .unwrap(),
            );
        }
        assert_eq!(
            workers
                .into_iter()
                .map(|worker| usize::from(worker.join().unwrap()))
                .sum::<usize>(),
            2
        );
        assert_eq!(store.budget.used.load(Ordering::Acquire), 0);
    }
}
