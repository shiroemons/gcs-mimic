use super::*;

/// A size of read buffer and the spare ones kept for reuse. A reused buffer skips the allocation
/// and the zero-filling that every frame would otherwise repeat. More in-flight reads than a pool
/// keeps simply allocate.
struct BufferPool {
    buffer_bytes: usize,
    retained: usize,
    spare: Mutex<Vec<Vec<u8>>>,
}

impl BufferPool {
    const fn new(buffer_bytes: usize, retained: usize) -> Self {
        Self {
            buffer_bytes,
            retained,
            spare: Mutex::new(Vec::new()),
        }
    }

    fn spare(&self) -> MutexGuard<'_, Vec<Vec<u8>>> {
        self.spare.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The frames of a large object's last megabyte. This pool retains at most 8 MiB.
static SMALL_BUFFERS: BufferPool = BufferPool::new(256 * 1024, 32);
/// The most one HTTP frame carries, and so what a connection holds in flight. A frame costs about
/// 4 µs beyond its copies, which a large object spreads over more bytes. This pool retains at most
/// 16 MiB.
static LARGE_BUFFERS: BufferPool = BufferPool::new(1024 * 1024, 16);

/// An open immutable generation. Unlinking its directory entry cannot change its bytes.
pub struct ObjectReader {
    pub(super) file: ReaderData,
    pub(super) remaining: u64,
    pub(super) position: u64,
    /// Bytes at `position` a read-ahead already holds; never more than `remaining`.
    pub(super) head: Option<Bytes>,
    /// Selected with the object and authorization, so a concurrent policy change cannot mix headers.
    pub(crate) publicly_readable: bool,
}

pub(in crate::storage::catalog) enum ReaderData {
    /// Every read names its offset, so nothing depends on the file's cursor, a cancelled read
    /// changes no state, and a read still running on a blocking thread keeps the file open.
    Disk(Arc<File>),
    Memory(super::super::super::memory::Reader),
}

/// The filled start of a read buffer. A pooled one returns to its pool when the last `Bytes` over
/// it drops.
struct ReadBuffer {
    bytes: Vec<u8>,
    filled: usize,
    pool: Option<&'static BufferPool>,
}

impl ReadBuffer {
    /// A buffer for `capacity` bytes, at most one [`LARGE_BUFFERS`] buffer. Only a read of a whole
    /// pooled size takes from a pool; any other gets a buffer of exactly its size, so a small
    /// object or a gRPC chunk holds no more memory than its own bytes.
    fn take(capacity: usize) -> Self {
        let length = capacity.min(LARGE_BUFFERS.buffer_bytes);
        let pool = [&SMALL_BUFFERS, &LARGE_BUFFERS]
            .into_iter()
            .find(|pool| pool.buffer_bytes == length);
        let spare = pool.and_then(|pool| pool.spare().pop());
        Self {
            bytes: spare.unwrap_or_else(|| vec![0; length]),
            filled: 0,
            pool,
        }
    }
}

impl AsRef<[u8]> for ReadBuffer {
    fn as_ref(&self) -> &[u8] {
        self.bytes.get(..self.filled).unwrap_or_default()
    }
}

impl Drop for ReadBuffer {
    fn drop(&mut self) {
        let Some(pool) = self.pool else {
            return;
        };
        let mut spare = pool.spare();
        if spare.len() < pool.retained {
            spare.push(std::mem::take(&mut self.bytes));
        }
    }
}

/// Reads up to `capacity` bytes at `offset`, at most one [`LARGE_BUFFERS`] buffer.
fn read_chunk(file: &File, offset: u64, capacity: usize) -> io::Result<Bytes> {
    let mut buffer = ReadBuffer::take(capacity);
    let window = buffer.bytes.as_mut_slice();
    buffer.filled = loop {
        match crate::fsutil::read_at(file, window, offset) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            result => break result?,
        }
    };
    if buffer.filled == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(Bytes::from_owner(buffer))
}

/// Reads up to `capacity` bytes at `offset` if the page cache holds them, without ever waiting for
/// storage, so an async worker may call it. That saves the two thread wakeups of a blocking read,
/// which cost more than the copy. It may return fewer bytes than asked for.
fn read_cached_chunk(file: &File, offset: u64, capacity: usize) -> Option<Bytes> {
    if !crate::fsutil::READS_CACHED_PAGES {
        return None;
    }
    let mut buffer = ReadBuffer::take(capacity);
    let window = buffer.bytes.as_mut_slice();
    buffer.filled = crate::fsutil::read_cached_at(file, window, offset)?;
    Some(Bytes::from_owner(buffer))
}

/// Whether a read may still find its bytes in the page cache before it blocks.
#[derive(Clone, Copy)]
enum PageCache {
    Try,
    /// A non-blocking read at this offset has just come back empty.
    Missed,
}

/// A blocking view of the reader's selected bytes for decoders that pull from `io::Read`.
struct PositionalRead {
    file: Arc<File>,
    position: u64,
}

impl io::Read for PositionalRead {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let length = crate::fsutil::read_at(&self.file, bytes, self.position)?;
        self.position += length as u64;
        Ok(length)
    }
}

impl ObjectReader {
    /// Appendable generations grow in place, so reads stop at the persisted size.
    pub(in crate::storage::catalog) fn bounded_data(file: ReaderData, remaining: u64) -> Self {
        Self {
            file,
            remaining,
            position: 0,
            head: None,
            publicly_readable: false,
        }
    }

    /// Wraps an arbitrary file so server tests can exercise reader failures.
    #[cfg(test)]
    pub(crate) fn bounded_for_test(file: File, remaining: u64) -> Self {
        Self::bounded_data(ReaderData::Disk(Arc::new(file)), remaining)
    }

    /// A reader over a generation the read cache keeps open.
    pub(in crate::storage::catalog) fn from_cached(object: &CachedObject) -> Option<Self> {
        let download = object.download.as_ref()?;
        Some(Self {
            file: ReaderData::Disk(download.file.clone()),
            remaining: object.record.size,
            position: 0,
            head: None,
            publicly_readable: download.publicly_readable,
        })
    }

    /// What the read cache keeps for later downloads of this generation.
    pub(in crate::storage::catalog) fn cached_download(&self) -> Option<CachedDownload> {
        match &self.file {
            ReaderData::Disk(file) => Some(CachedDownload {
                file: file.clone(),
                publicly_readable: self.publicly_readable,
            }),
            ReaderData::Memory(_) => None,
        }
    }

    /// How much the next HTTP frame carries, or nothing when no bytes remain. Measured on 1 MiB and
    /// 10 MiB objects, frames of 1 MiB sent the large object fastest and frames of 256 KiB the
    /// smaller one, so the large size applies only while more than one large frame remains.
    fn frame_capacity(&self) -> Option<usize> {
        let large = LARGE_BUFFERS.buffer_bytes as u64;
        let capacity = if self.remaining > large {
            large
        } else {
            self.remaining.min(SMALL_BUFFERS.buffer_bytes as u64)
        };
        usize::try_from(capacity).ok().filter(|size| *size > 0)
    }

    /// Reads the body's start on the calling thread, which must be one that may block: a pooled
    /// catalog reader, already away from the async workers. A failure only skips the read-ahead;
    /// the streaming read reports it where a client can tell it from a missing object.
    pub(in crate::storage::catalog) fn read_ahead(&mut self) {
        let (ReaderData::Disk(file), Some(capacity)) = (&self.file, self.frame_capacity()) else {
            return;
        };
        self.head = read_chunk(file, self.position, capacity).ok();
    }

    /// Fetches the body's start on a pooled catalog reader. Where the page cache can be read
    /// without waiting, only cached bytes are taken: a wait for storage here would hold a read
    /// slot, an admission slot and the lease an exclusive job drains. Elsewhere the read may
    /// block, which still saves the streaming read its thread hop.
    pub(in crate::storage::catalog) fn read_ahead_on_pool(&mut self) {
        if crate::fsutil::READS_CACHED_PAGES {
            self.read_ahead_cached();
        } else {
            self.read_ahead();
        }
    }

    /// Fetches the body's start only if that cannot wait for storage, so any thread may call it.
    /// Bytes that are not in the page cache are left to the streaming read.
    pub(in crate::storage::catalog) fn read_ahead_cached(&mut self) {
        let (ReaderData::Disk(file), Some(capacity)) = (&self.file, self.frame_capacity()) else {
            return;
        };
        self.head = read_cached_chunk(file, self.position, capacity);
    }

    /// How many bytes a read-ahead holds, for tests that must know whether one ran.
    #[cfg(test)]
    pub(in crate::storage::catalog) fn read_ahead_len(&self) -> Option<usize> {
        self.head.as_ref().map(Bytes::len)
    }

    /// Whether every selected byte has been handed out.
    pub(crate) fn is_finished(&self) -> bool {
        self.remaining == 0
    }

    fn advance(&mut self, bytes: Bytes) -> Bytes {
        self.remaining -= bytes.len() as u64;
        self.position += bytes.len() as u64;
        bytes
    }

    /// The next bytes of an HTTP body if getting them cannot wait for storage, so an async worker
    /// may call this. `None` leaves the reader where it was, for [`Self::next_frame`] to go on.
    pub(crate) fn next_frame_at_hand(&mut self) -> Option<Bytes> {
        let bytes = match self.head.take() {
            Some(head) => head,
            None => {
                let capacity = self.frame_capacity()?;
                match &mut self.file {
                    ReaderData::Disk(file) => read_cached_chunk(file, self.position, capacity)?,
                    ReaderData::Memory(reader) => reader.next_chunk(capacity)?,
                }
            }
        };
        Some(self.advance(bytes))
    }

    /// The next bytes of an HTTP body, after [`Self::next_frame_at_hand`] found none, so this goes
    /// straight to a blocking read. A frame may be larger than [`Self::next_chunk`] allows,
    /// because each one costs a socket write and, for bytes not yet cached, two thread wakeups.
    pub(crate) async fn next_frame(&mut self) -> io::Result<Option<Bytes>> {
        let Some(limit) = self.frame_capacity() else {
            return Ok(None);
        };
        self.next_bytes(limit, PageCache::Missed).await
    }

    /// Transfers the already-opened immutable generation; no storage path is exposed. A duplicate
    /// would share its cursor with every other holder of the file, so a file the read cache also
    /// holds is refused instead. Only HTTP downloads open through the cache, and they never call
    /// this.
    pub async fn into_file(self) -> io::Result<File> {
        match self.file {
            ReaderData::Disk(file) => {
                let mut file = Arc::try_unwrap(file)
                    .map_err(|_| io::Error::other("object data is shared with the read cache"))?;
                io::Seek::seek(&mut file, io::SeekFrom::Start(self.position))?;
                Ok(file)
            }
            ReaderData::Memory(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "RAM objects have no filesystem descriptor",
            )),
        }
    }

    pub(crate) async fn into_reader(self) -> Box<dyn io::Read + Send> {
        match self.file {
            ReaderData::Disk(file) => Box::new(PositionalRead {
                file,
                position: self.position,
            }),
            ReaderData::Memory(reader) => Box::new(reader),
        }
    }

    pub(crate) async fn set_range(&mut self, offset: u64, length: u64) -> io::Result<()> {
        if let ReaderData::Memory(reader) = &mut self.file {
            io::Seek::seek(reader, io::SeekFrom::Start(offset))?;
        }
        self.head = None;
        self.remaining = length;
        self.position = offset;
        Ok(())
    }

    /// Reads up to `limit` leading bytes without consuming them, so a caller can classify the
    /// content before choosing response headers. It reads one bounded chunk, never the whole
    /// object.
    pub(crate) async fn peek_prefix(&mut self, limit: usize) -> io::Result<Bytes> {
        if let Some(head) = &self.head {
            return Ok(head.slice(..head.len().min(limit)));
        }
        let offset = self.position;
        let remaining = self.remaining;
        let prefix = match self.next_chunk().await? {
            Some(bytes) => bytes.slice(..bytes.len().min(limit)),
            None => Bytes::new(),
        };
        self.set_range(offset, remaining).await?;
        Ok(prefix)
    }

    pub async fn next_chunk(&mut self) -> io::Result<Option<Bytes>> {
        self.next_bytes(CHUNK_SIZE, PageCache::Try).await
    }

    async fn next_bytes(
        &mut self,
        limit: usize,
        page_cache: PageCache,
    ) -> io::Result<Option<Bytes>> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let limit = self.remaining.min(limit as u64);
        let capacity = usize::try_from(limit).map_err(io::Error::other)?;
        let bytes = if let Some(mut head) = self.head.take() {
            if head.len() > capacity {
                self.head = Some(head.split_off(capacity));
            }
            head
        } else {
            match &mut self.file {
                ReaderData::Memory(reader) => reader
                    .next_chunk(capacity)
                    .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?,
                ReaderData::Disk(file) => {
                    let offset = self.position;
                    let cached = match page_cache {
                        PageCache::Try => read_cached_chunk(file, offset, capacity),
                        PageCache::Missed => None,
                    };
                    match cached {
                        Some(bytes) => bytes,
                        None => {
                            let file = file.clone();
                            crate::task::run_blocking("object-read", move || {
                                read_chunk(&file, offset, capacity)
                            })
                            .await
                            .map_err(io::Error::other)??
                        }
                    }
                }
            }
        };
        Ok(Some(self.advance(bytes)))
    }
}

#[cfg(test)]
mod tests;
