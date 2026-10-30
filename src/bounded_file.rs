//! Shared bounded reads for operator-controlled local inputs.

use std::{
    fs::File,
    io::{self, Read},
    path::Path,
};

#[derive(Debug)]
pub enum ReadError {
    Io(io::Error),
    Limit,
}

pub fn read(path: &Path, limit: u64) -> Result<Vec<u8>, ReadError> {
    let file = File::open(path).map_err(ReadError::Io)?;
    read_from(file, limit)
}

pub(crate) fn read_from(reader: impl Read, limit: u64) -> Result<Vec<u8>, ReadError> {
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(ReadError::Io)?;
    if bytes.len() as u64 > limit {
        return Err(ReadError::Limit);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::test_support::{TestDir, fifo};

    #[cfg(unix)]
    #[test]
    fn bounded_reads_preserve_fifo_inputs() {
        use std::io::Write;
        let directory = TestDir::new();
        let path = directory.0.join("input");
        fifo(&path);
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            File::create(writer_path)
                .unwrap()
                .write_all(b"input")
                .unwrap()
        });
        let result = read(&path, 5);
        writer.join().unwrap();
        assert_eq!(result.unwrap(), b"input");
    }

    #[test]
    fn inclusive_limit_reads_only_one_extra_byte() {
        assert_eq!(read_from(&b"abcd"[..], 4).ok(), Some(b"abcd".to_vec()));
        let mut input = &b"abcdef"[..];
        assert!(matches!(read_from(&mut input, 4), Err(ReadError::Limit)));
        assert_eq!(input, b"f");
        assert!(read_from(&b""[..], 0).is_ok());
        assert!(matches!(read_from(&b"a"[..], 0), Err(ReadError::Limit)));
    }
}
