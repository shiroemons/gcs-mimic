use super::{ObjectActivity, ObjectCtx, ResponseBody, boxed, empty_response};
use crate::{
    object::{ObjectError, ObjectRecord},
    storage::{ObjectReader, failure::object_unavailable},
};
use bytes::Bytes;
use flate2::bufread::MultiGzDecoder;
use http_body_util::{BodyExt, Full};
use hyper::{HeaderMap, Method, Response, StatusCode, header::HeaderValue};
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

/// Cloud Storage marks a gunzipped download with these two fields and sends no length,
/// encoding or range headers for the expanded bytes, on both the JSON and XML APIs
/// (observed 2026-10-31). The stored-object hashes stay, describing the stored bytes.
pub(super) fn insert_transcoded_headers(headers: &mut HeaderMap) {
    for name in [
        "content-encoding",
        "content-length",
        "content-range",
        "accept-ranges",
    ] {
        headers.remove(name);
    }
    // The expanded bytes differ from the stored ones, so Cloud Storage sends the tag weak
    // on every gunzipped response, 200, HEAD and 400 alike, on both APIs.
    if let Some(etag) = headers.get("etag").cloned()
        && !etag.as_bytes().starts_with(b"W/")
        && let Ok(weak) = HeaderValue::from_bytes(&[b"W/", etag.as_bytes()].concat())
    {
        headers.insert("etag", weak);
    }
    headers.insert(
        "x-guploader-response-body-transformations",
        HeaderValue::from_static("gunzipped"),
    );
    headers.insert(
        "warning",
        HeaderValue::from_static("214 UploadServer gunzipped"),
    );
}

/// A stored-gzip object is either sent compressed or gunzipped depending on the client's
/// `Accept-Encoding`, so caches must key its response on that field. The JSON API sends the
/// field only on the compressed response and never on a gunzipped one; the XML API sends it
/// on every representation, gunzipped included (both observed 2026-10-31). Callers decide
/// which representations get it.
pub(super) fn insert_negotiated_vary(headers: &mut HeaderMap) {
    headers.append("vary", HeaderValue::from_static("Accept-Encoding"));
}

const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Whether an object whose metadata declares gzip holds bytes that are not gzip. Cloud
/// Storage accepts such an upload, then fails the gunzipping read before sending any
/// header (observed 2026-10-31). Only the leading magic bytes are checked, so a body that
/// breaks later still fails mid-stream. An empty object has nothing to contradict.
/// A failed probe read is a backend failure; it keeps its I/O cause for diagnostics.
pub(super) async fn is_plain(
    reader: &mut ObjectReader,
    stored_size: u64,
) -> Result<bool, ObjectError> {
    if stored_size == 0 {
        return Ok(false);
    }
    let prefix = reader
        .peek_prefix(GZIP_MAGIC.len())
        .await
        .map_err(object_unavailable)?;
    Ok(!GZIP_MAGIC.starts_with(&prefix))
}

/// The 11-byte `Bad Request` Cloud Storage sends for a gunzipping read of non-gzip bytes,
/// with the headers the gunzipped response would have carried.
pub(super) fn plain_body_rejection(mut headers: HeaderMap) -> Response<ResponseBody> {
    const BODY: &str = "Bad Request";
    insert_transcoded_headers(&mut headers);
    headers.insert(
        "content-type",
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert("content-length", HeaderValue::from(BODY.len()));
    let mut response = Response::new(Full::new(Bytes::from_static(BODY.as_bytes())));
    *response.status_mut() = StatusCode::BAD_REQUEST;
    *response.headers_mut() = headers;
    boxed(response)
}

/// HEAD sends the selected headers without starting the bounded decompression worker.
pub(super) fn response(
    ctx: ObjectCtx<'_>,
    record: &ObjectRecord,
    reader: ObjectReader,
    headers: HeaderMap,
    method: &Method,
) -> Response<ResponseBody> {
    let mut response = if method == Method::HEAD {
        boxed(empty_response(StatusCode::OK))
    } else {
        Response::new(body(
            reader,
            record.size,
            ctx.config.max_object_bytes,
            ctx.activity,
        ))
    };
    *response.headers_mut() = headers;
    response
}

fn body(
    reader: ObjectReader,
    stored_size: u64,
    expanded_limit: u64,
    activity: &watch::Sender<ObjectActivity>,
) -> ResponseBody {
    let (sender, body) = super::chunk_body::channel();
    let activity = activity.clone();
    crate::task::spawn_supervised("gzip-object-body", async move {
        let mut progress = activity.subscribe();
        let stream = async {
            let file = reader.into_reader().await;
            let mut decoder = crate::task::spawn_blocking("object-gzip-decoder", move || {
                MultiGzDecoder::new(Input {
                    reader: BufReader::new(file.take(stored_size)),
                    budget: CHUNK_BYTES,
                })
            })
            .await
            .map_err(|_| io::Error::other("gzip decoder worker failed"))?
            .map_err(|_| io::Error::other("gzip decoder worker failed"))?;
            let mut total = 0_u64;
            loop {
                let (next, bytes) = crate::task::spawn_blocking("object-gzip-read", move || {
                    decoder.get_mut().budget = CHUNK_BYTES;
                    let mut bytes = vec![0; CHUNK_BYTES];
                    let result = decoder.read(&mut bytes).map(|length| {
                        bytes.truncate(length);
                        Bytes::from(bytes)
                    });
                    (decoder, result)
                })
                .await
                .map_err(|_| io::Error::other("gzip decoder worker failed"))?
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
                if sender.send(Ok(bytes)).await.is_err() {
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
            let _ = sender.send(Err(error)).await;
        }
    });
    body.boxed_unsync()
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, GzBuilder, write::GzEncoder};
    use std::io::{Cursor, Write};

    /// A failing probe read stays an I/O failure with its cause reachable, not a bare 503.
    #[tokio::test]
    async fn is_plain_keeps_the_io_cause_of_a_failed_probe() {
        let path = std::env::temp_dir().join(format!("gcs-mimic-is-plain-{}", std::process::id()));
        std::fs::write(&path, b"").unwrap();
        let file = tokio::fs::File::open(&path).await.unwrap();
        std::fs::remove_file(&path).unwrap();
        // The object claims 5 bytes but the file is empty, so the probe hits end of file.
        let mut reader = ObjectReader::bounded_for_test(file, 5);
        let error = is_plain(&mut reader, 5).await.unwrap_err();
        let ObjectError::Io(cause) = &error else {
            panic!("expected an I/O failure, got {error:?}");
        };
        assert_eq!(cause.kind(), io::ErrorKind::UnexpectedEof);
        assert!(std::error::Error::source(&error).is_some());
    }

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
