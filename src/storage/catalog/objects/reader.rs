use super::*;

/// An open immutable generation. Unlinking its directory entry cannot change its bytes.
pub struct ObjectReader {
    pub(super) file: ReaderData,
    pub(super) remaining: u64,
    pub(super) position: u64,
    /// Selected with the object and authorization, so a concurrent policy change cannot mix headers.
    pub(crate) publicly_readable: bool,
}

pub(in crate::storage::catalog) enum ReaderData {
    Disk(tokio::fs::File),
    Memory(super::super::super::memory::Reader),
}

#[cfg(test)]
impl ReaderData {
    pub(super) async fn stream_position(&mut self) -> io::Result<u64> {
        match self {
            Self::Disk(file) => file.stream_position().await,
            Self::Memory(reader) => io::Seek::stream_position(reader),
        }
    }
}

impl ObjectReader {
    /// Appendable generations grow in place, so reads stop at the persisted size.
    pub(in crate::storage::catalog) fn bounded_data(file: ReaderData, remaining: u64) -> Self {
        Self {
            file,
            remaining,
            position: 0,
            publicly_readable: false,
        }
    }

    /// Wraps an arbitrary file so server tests can exercise reader failures.
    #[cfg(test)]
    pub(crate) fn bounded_for_test(file: tokio::fs::File, remaining: u64) -> Self {
        Self::bounded_data(ReaderData::Disk(file), remaining)
    }

    /// Transfers the already-opened immutable generation; no storage path is exposed.
    pub async fn into_file(self) -> io::Result<File> {
        match self.file {
            ReaderData::Disk(file) => Ok(file.into_std().await),
            ReaderData::Memory(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "RAM objects have no filesystem descriptor",
            )),
        }
    }

    pub(crate) async fn into_reader(self) -> Box<dyn io::Read + Send> {
        match self.file {
            ReaderData::Disk(file) => Box::new(file.into_std().await),
            ReaderData::Memory(reader) => Box::new(reader),
        }
    }

    pub(crate) async fn set_range(&mut self, offset: u64, length: u64) -> io::Result<()> {
        match &mut self.file {
            ReaderData::Disk(file) => {
                file.seek(io::SeekFrom::Start(offset)).await?;
            }
            ReaderData::Memory(reader) => {
                io::Seek::seek(reader, io::SeekFrom::Start(offset))?;
            }
        }
        self.remaining = length;
        self.position = offset;
        Ok(())
    }

    /// Reads up to `limit` leading bytes and rewinds, so a caller can classify the content
    /// before choosing response headers. The selected range and current position are preserved;
    /// it reads one bounded chunk, never the whole object.
    pub(crate) async fn peek_prefix(&mut self, limit: usize) -> io::Result<Bytes> {
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
        if self.remaining == 0 {
            return Ok(None);
        }
        let limit = self.remaining.min(CHUNK_SIZE as u64);
        let capacity = usize::try_from(limit).map_err(io::Error::other)?;
        if let ReaderData::Memory(reader) = &mut self.file {
            let bytes = reader
                .next_chunk(capacity)
                .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
            self.remaining -= bytes.len() as u64;
            self.position += bytes.len() as u64;
            return Ok(Some(bytes));
        }
        // Reading into spare capacity skips zero-filling a buffer the read overwrites;
        // `take` keeps the read within the persisted size even if capacity is larger.
        let mut buffer = BytesMut::with_capacity(capacity);
        let ReaderData::Disk(file) = &mut self.file else {
            return Err(io::Error::other("invalid object reader backend"));
        };
        let length = file.take(limit).read_buf(&mut buffer).await?;
        if length == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        self.remaining -= length as u64;
        self.position += length as u64;
        Ok(Some(buffer.freeze()))
    }
}
