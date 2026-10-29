use super::{ObjectActivity, ResponseBody};
use crate::storage::ObjectReader;
use bytes::Bytes;
use flate2::bufread::MultiGzDecoder;
use http_body_util::{BodyExt, Channel};
use std::io::{self, BufRead, BufReader, Read};
use tokio::sync::watch;

const CHUNK_BYTES: usize = 64 * 1024;

// Bound compressed input too: many empty members must not monopolize a worker.
struct Input<R> {
    reader: R,
    budget: usize,
}

impl<R: BufRead> BufRead for Input<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.budget == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let bytes = self.reader.fill_buf()?;
        Ok(&bytes[..bytes.len().min(self.budget)])
    }

    fn consume(&mut self, length: usize) {
        self.budget -= length;
        self.reader.consume(length);
    }
}

impl<R: BufRead> Read for Input<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let bytes = self.fill_buf()?;
        let length = bytes.len().min(output.len());
        output[..length].copy_from_slice(&bytes[..length]);
        self.consume(length);
        Ok(length)
    }
}

pub(super) fn body(
    reader: ObjectReader,
    stored_size: u64,
    expanded_limit: u64,
    activity: &watch::Sender<ObjectActivity>,
) -> ResponseBody {
    let (mut sender, body) = Channel::<Bytes, io::Error>::new(1);
    let activity = activity.clone();
    tokio::spawn(async move {
        let mut progress = activity.subscribe();
        let stream = async {
            let file = reader.into_file().await;
            let mut decoder = tokio::task::spawn_blocking(move || {
                MultiGzDecoder::new(Input {
                    reader: BufReader::new(file.take(stored_size)),
                    budget: CHUNK_BYTES,
                })
            })
            .await
            .map_err(|_| io::Error::other("gzip decoder worker failed"))?;
            let mut total = 0_u64;
            loop {
                let (next, bytes) = tokio::task::spawn_blocking(move || {
                    decoder.get_mut().budget = CHUNK_BYTES;
                    let mut bytes = vec![0; CHUNK_BYTES];
                    let result = decoder.read(&mut bytes).map(|length| {
                        bytes.truncate(length);
                        Bytes::from(bytes)
                    });
                    (decoder, result)
                })
                .await
                .map_err(|_| io::Error::other("gzip decoder worker failed"))?;
                decoder = next;
                let bytes = match bytes {
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                    other => other?,
                };
                if bytes.is_empty() {
                    return Ok::<_, io::Error>(());
                }
                total = total
                    .checked_add(bytes.len() as u64)
                    .filter(|size| *size <= expanded_limit)
                    .ok_or_else(|| io::Error::other("expanded object exceeds max_object_bytes"))?;
                if sender.send_data(bytes).await.is_err() {
                    return Ok(());
                }
                activity.send_modify(|progress| progress.awaiting_catalog = false);
            }
        };
        // RequestFinished clears the deadline on response cancellation or completion.
        let result = tokio::select! {
            result = stream => result,
            _ = progress.wait_for(|state| state.deadline.is_none()) => Ok(()),
        };
        if let Err(error) = result {
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                "object_transcoding_failed",
                serde_json::json!({"cause": error.kind().to_string()}),
            );
            sender.abort(error);
        }
    });
    body.boxed_unsync()
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, GzBuilder, write::GzEncoder};
    use std::io::{Cursor, Write};

    #[test]
    fn gzip_budget_resumes_optional_headers_deflate_trailers_and_member_boundaries() {
        let mut encoder = GzBuilder::new()
            .filename(vec![b'f'; 65535])
            .comment(vec![b'c'; 65535])
            .extra(vec![7; 65535])
            .write(Vec::new(), Compression::fast());
        encoder.write_all(b"header data").unwrap();
        let compressed = [
            encoder.finish().unwrap(),
            GzEncoder::new(Vec::new(), Compression::default())
                .finish()
                .unwrap(),
        ]
        .concat();
        for budget in [
            1,
            2,
            7,
            8,
            9,
            10,
            11,
            16,
            64,
            CHUNK_BYTES - 1,
            CHUNK_BYTES,
            CHUNK_BYTES + 1,
        ] {
            let mut decoder = MultiGzDecoder::new(Input {
                reader: Cursor::new(&compressed),
                budget: 0,
            });
            let mut output = [0; 3];
            let mut decoded = Vec::new();
            let mut steps = 0;
            loop {
                steps += 1;
                assert!(steps < compressed.len() * 4, "decoder stopped progressing");
                decoder.get_mut().budget = budget;
                match decoder.read(&mut output) {
                    Ok(0) => break,
                    Ok(length) => decoded.extend_from_slice(&output[..length]),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) => panic!("budget {budget}: {error}"),
                }
            }
            assert_eq!(decoded, b"header data", "budget {budget}");
        }
    }

    #[test]
    fn gzip_budget_one_never_hides_corruption_at_member_boundaries() {
        let mut encoder = GzBuilder::new()
            .filename("file")
            .comment("comment")
            .extra(vec![1, 2, 3])
            .write(Vec::new(), Compression::default());
        encoder.write_all(b"payload").unwrap();
        let first = encoder.finish().unwrap();
        let empty = GzEncoder::new(Vec::new(), Compression::default())
            .finish()
            .unwrap();
        let complete = [first.clone(), empty.clone()].concat();
        let mut bad_crc = first.clone();
        bad_crc[first.len() - 8] ^= 1;
        let mut bad_size = first.clone();
        bad_size[first.len() - 4] ^= 1;
        for compressed in [
            [bad_crc, empty.clone()].concat(),
            [bad_size, empty].concat(),
            complete[..complete.len() - 1].to_vec(),
            [complete.as_slice(), b"garbage"].concat(),
            first[..first.len() - 10].to_vec(),
        ] {
            let limit = compressed.len() * 100;
            let mut decoder = MultiGzDecoder::new(Input {
                reader: Cursor::new(compressed),
                budget: 0,
            });
            let mut steps = 0;
            loop {
                steps += 1;
                assert!(steps < limit, "decoder stopped progressing");
                decoder.get_mut().budget = 1;
                match decoder.read(&mut [0; 3]) {
                    Ok(0) => panic!("invalid gzip completed cleanly"),
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(_) => break,
                }
            }
        }
    }

    #[test]
    fn gzip_decode_bounds_input_and_output_across_members() {
        let mut compressed = Vec::new();
        for _ in 0..4000 {
            compressed.extend(
                GzEncoder::new(Vec::new(), Compression::default())
                    .finish()
                    .unwrap(),
            );
        }
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        for _ in 0..1024 {
            encoder.write_all(&[0; CHUNK_BYTES]).unwrap();
        }
        compressed.extend(encoder.finish().unwrap());
        let mut decoder = MultiGzDecoder::new(Input {
            reader: Cursor::new(compressed),
            budget: 0,
        });
        let mut output = [0_u8; CHUNK_BYTES];
        let mut total = 0;
        let mut yielded = false;
        loop {
            decoder.get_mut().budget = CHUNK_BYTES;
            match decoder.read(&mut output) {
                Ok(0) => break,
                Ok(length) => {
                    assert!(length <= CHUNK_BYTES);
                    assert!(output[..length].iter().all(|byte| *byte == 0));
                    total += length;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => yielded = true,
                Err(error) => panic!("gzip decode failed: {error}"),
            }
        }
        assert!(yielded);
        assert_eq!(total, 64 * 1024 * 1024);
    }
}
