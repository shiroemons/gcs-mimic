use super::{FileIdentity, FsError, read_at, stat_identity};
use crate::hashing::{Hasher, Hashes};
use std::{fs::File, io};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HashOutcome {
    Complete(Hashes),
    Short { read: u64 },
    Changed,
}

fn identity(file: &File) -> io::Result<FileIdentity> {
    stat_identity(file).map_err(|error| match error {
        FsError::Io(error) => error,
        FsError::Unsupported => io::ErrorKind::Unsupported.into(),
    })
}

pub(crate) fn hash_exact<E: From<io::Error>>(
    file: &File,
    size: u64,
    mut sink: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<HashOutcome, E> {
    let before = identity(file)?;
    if before.size != size {
        return Ok(HashOutcome::Changed);
    }
    let mut hasher = Hasher::default();
    let mut buffer = [0; 65_536];
    let mut offset = 0;
    while offset < size {
        let limit = usize::try_from((size - offset).min(65_536)).map_err(io::Error::other)?;
        let count = read_at(
            file,
            buffer
                .get_mut(..limit)
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?,
            offset,
        )?;
        if count == 0 {
            return Ok(HashOutcome::Short { read: offset });
        }
        let chunk = buffer
            .get(..count)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
        sink(chunk)?;
        hasher.update(chunk);
        offset += count as u64;
    }
    if before != identity(file)? {
        return Ok(HashOutcome::Changed);
    }
    Ok(HashOutcome::Complete(hasher.finish()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;
    use std::fs;
    #[test]
    fn exact_hashes_cover_empty_full_short_growing_and_changed_files() {
        let dir = TestDir::new();
        let path = dir.0.join("body");
        for size in [0, 65_536] {
            let bytes = vec![7; size];
            fs::write(&path, &bytes).unwrap();
            let file = File::open(&path).unwrap();
            let mut expected = Hasher::default();
            expected.update(&bytes);
            assert_eq!(
                hash_exact(&file, size as u64, |_| Ok::<_, io::Error>(())).unwrap(),
                HashOutcome::Complete(expected.finish())
            );
        }
        fs::write(&path, vec![7; 131_072]).unwrap();
        let file = File::open(&path).unwrap();
        assert_eq!(
            hash_exact(&file, 131_072, |_| {
                fs::OpenOptions::new()
                    .write(true)
                    .open(&path)?
                    .set_len(65_536)
            })
            .unwrap(),
            HashOutcome::Short { read: 65_536 }
        );
        let sizes = std::iter::once(65_537);
        #[cfg(unix)]
        let sizes = sizes.chain(std::iter::once(65_536));
        for size in sizes {
            fs::write(&path, vec![7; 65_536]).unwrap();
            let file = File::open(&path).unwrap();
            assert_eq!(
                hash_exact(&file, 65_536, |_| fs::write(&path, vec![8; size])).unwrap(),
                HashOutcome::Changed
            );
        }
    }
}
