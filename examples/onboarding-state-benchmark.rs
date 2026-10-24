use bytes::Bytes;
use gcs_mimic::storage::admin::{
    BucketDefinition, BucketName, ObjectName, Origin, Publication, PublishMode,
};
use gcs_mimic::{
    object::{ExpectedHashes, UploadChunk},
    storage::Catalog,
};
use md5::{Digest, Md5};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime},
};

const BLOCK: usize = 65_536;
const GIB: usize = 1_073_741_824;

fn percentile(sorted: &[f64], numerator: usize) -> f64 {
    sorted[(sorted.len() * numerator).div_ceil(100).max(1) - 1]
}

fn report(label: &str, mut latencies: Vec<f64>, bytes: usize, elapsed: Duration) -> Value {
    latencies.sort_by(f64::total_cmp);
    json!({"workload": label, "requests": latencies.len(), "errors": 0,
            "bytes": bytes, "wall_seconds": elapsed.as_secs_f64(),
            "requests_per_second": latencies.len() as f64 / elapsed.as_secs_f64(),
            "mib_per_second": bytes as f64 / 1_048_576.0 / elapsed.as_secs_f64(),
            "p50_seconds": percentile(&latencies, 50),
            "p95_seconds": percentile(&latencies, 95),
            "p99_seconds": percentile(&latencies, 99)})
}

fn request(address: &str, method: &str, path: &str, size: usize, byte: u8) -> BufReader<TcpStream> {
    assert!(address.starts_with("127.0.0.1:"), "loopback only");
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(180)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(180)))
        .unwrap();
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: {address}\r\nContent-Length: {size}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n").unwrap();
    let block = [byte; BLOCK];
    let mut remaining = size;
    while remaining > 0 {
        let count = remaining.min(block.len());
        stream.write_all(&block[..count]).unwrap();
        remaining -= count;
    }
    BufReader::new(stream)
}

fn headers(reader: &mut impl BufRead) -> usize {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line.split_whitespace().nth(1), Some("200"), "{line}");
    let mut length = None;
    let mut total = line.len();
    loop {
        line.clear();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        total += line.len();
        assert!(total <= 16_384);
        if line == "\r\n" {
            return length.expect("bounded benchmark expects Content-Length");
        }
        let (name, value) = line.split_once(':').unwrap();
        if name.eq_ignore_ascii_case("content-length") {
            assert!(length.replace(value.trim().parse().unwrap()).is_none());
        }
        assert!(!name.eq_ignore_ascii_case("transfer-encoding"));
    }
}

fn upload(address: &str, name: &str, size: usize, byte: u8) -> Value {
    let mut reader = request(
        address,
        "POST",
        &format!("/upload/storage/v1/b/benchmark-bucket/o?uploadType=media&name={name}"),
        size,
        byte,
    );
    let length = headers(&mut reader);
    assert!(length <= BLOCK);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["size"], size.to_string());
    assert_eq!(value["name"], name);
    assert!(
        value["generation"]
            .as_str()
            .unwrap()
            .parse::<i64>()
            .unwrap()
            > 0
    );
    value
}

fn hashes(size: usize, byte: u8) -> (String, String) {
    let block = [byte; BLOCK];
    let mut md5 = Md5::new();
    let mut crc = 0;
    let mut remaining = size;
    while remaining > 0 {
        let count = remaining.min(BLOCK);
        md5.update(&block[..count]);
        crc = crc32c::crc32c_append(crc, &block[..count]);
        remaining -= count;
    }
    (base64(&md5.finalize()), base64(&crc.to_be_bytes()))
}

fn base64(bytes: &[u8]) -> String {
    let mut child = Command::new("base64")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn checked_generation(value: &Value, previous: i64) -> i64 {
    let generation = value["generation"].as_str().unwrap().parse().unwrap();
    assert!(generation > previous, "generation was reused");
    generation
}

fn small(address: &str, overwrite: bool, count: usize, previous: i64) {
    assert!((1..=100_000).contains(&count));
    let byte = if overwrite { 0x62 } else { 0x61 };
    let (md5, crc) = hashes(1024, byte);
    let mut latencies = Vec::with_capacity(count);
    let start = Instant::now();
    let mut last_generation = previous;
    let mut first_generation = 0;
    for index in 0..count {
        let before = Instant::now();
        let result = upload(address, &format!("object-{index:06}"), 1024, byte);
        latencies.push(before.elapsed().as_secs_f64());
        assert_eq!(result["md5Hash"], md5);
        assert_eq!(result["crc32c"], crc);
        let generation = checked_generation(&result, last_generation);
        if index == 0 {
            first_generation = generation;
        }
        last_generation = generation;
    }
    let mut result = report(
        if overwrite {
            "small-overwrite"
        } else {
            "small-publish"
        },
        latencies,
        count * 1024,
        start.elapsed(),
    );
    result["first_generation"] = first_generation.into();
    result["last_generation"] = last_generation.into();
    println!("{result}");
}

fn upload_large(address: &str) {
    let (md5, crc) = hashes(GIB, 0x5a);
    let before = Instant::now();
    let result = upload(address, "large", GIB, 0x5a);
    let elapsed = before.elapsed();
    assert_eq!(result["md5Hash"], md5);
    assert_eq!(result["crc32c"], crc);
    println!(
        "{}",
        report("upload-1gib", vec![elapsed.as_secs_f64()], GIB, elapsed)
    );
}

fn download_large(address: &str) {
    let before = Instant::now();
    let mut reader = request(
        address,
        "GET",
        "/storage/v1/b/benchmark-bucket/o/large?alt=media",
        0,
        0,
    );
    assert_eq!(headers(&mut reader), GIB);
    let mut block = [0; BLOCK];
    let mut remaining = GIB;
    while remaining > 0 {
        let count = remaining.min(BLOCK);
        reader.read_exact(&mut block[..count]).unwrap();
        assert!(block[..count].iter().all(|byte| *byte == 0x5a));
        remaining -= count;
    }
    assert_eq!(reader.read(&mut block).unwrap(), 0);
    let elapsed = before.elapsed();
    println!(
        "{}",
        report("download-1gib", vec![elapsed.as_secs_f64()], GIB, elapsed)
    );
}

fn prepare(path: &Path, count: usize) {
    assert!(!path.exists(), "fixture destination must be new");
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let catalog = Catalog::open(path).await.unwrap();
            let record = gcs_mimic::json::parse_insert(
                br#"{"name":"benchmark-bucket"}"#,
                "benchmark-project",
                SystemTime::now(),
            )
            .unwrap();
            catalog
                .admin(Origin::Seed)
                .ensure_bucket(BucketDefinition::new(record).unwrap())
                .await
                .unwrap();
            let mut uploads = tokio::task::JoinSet::new();
            for index in 0..count {
                if uploads.len() == 8 {
                    uploads.join_next().await.unwrap().unwrap();
                }
                let catalog = catalog.clone();
                uploads.spawn(async move {
                    let (sender, receiver) = tokio::sync::mpsc::channel(2);
                    sender
                        .send(Ok(UploadChunk::Data(Bytes::from_static(&[0x61; 1024]))))
                        .await
                        .unwrap();
                    sender.send(Ok(UploadChunk::End)).await.unwrap();
                    drop(sender);
                    let record = catalog
                        .admin(Origin::Seed)
                        .publish_object(
                            &BucketName::new("benchmark-bucket").unwrap(),
                            &ObjectName::new(format!("object-{index:06}")).unwrap(),
                            Publication {
                                content_type: "application/octet-stream".into(),
                                expected: ExpectedHashes::default(),
                                condition: None,
                                metadata: Default::default(),
                                max_bytes: 1024,
                                mode: PublishMode::Replace,
                            },
                            receiver,
                        )
                        .await
                        .unwrap();
                    assert_eq!(record.size, 1024);
                });
            }
            while let Some(result) = uploads.join_next().await {
                result.unwrap();
            }
            let config = gcs_mimic::config::Config {
                data_dir: path.into(),
                ..Default::default()
            };
            gcs_mimic::seed::import(&catalog, &config, &gcs_mimic::clock::SystemClock)
                .await
                .unwrap();
            catalog.close().await.unwrap();
        });
    let db = rusqlite::Connection::open_with_flags(
        path.join("catalog.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let actual: i64 = db
        .query_row("SELECT count(*) FROM objects", [], |row| row.get(0))
        .unwrap();
    assert_eq!(actual, i64::try_from(count).unwrap());
    println!(
        "{}",
        json!({"fixture_objects":count,"publication":"Catalog::put_object","result":"PASS"})
    );
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("prepare") if args.len() == 4 => {
            prepare(Path::new(&args[2]), args[3].parse().unwrap())
        }
        Some("publish" | "overwrite") if args.len() == 5 => small(
            &args[2],
            args[1] == "overwrite",
            args[3].parse().unwrap(),
            args[4].parse().unwrap(),
        ),
        Some("upload") if args.len() == 3 => upload_large(&args[2]),
        Some("download") if args.len() == 3 => download_large(&args[2]),
        Some("hold-catalog" | "check-catalog") if args.len() == 3 => {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let catalog = Catalog::open(&args[2]).await.unwrap();
                    println!("catalog opened");
                    if args[1] == "hold-catalog" {
                        std::future::pending::<()>().await;
                    }
                    catalog.close().await.unwrap();
                });
        }
        _ => panic!(
            "usage: onboarding-state-benchmark prepare NEW_DIR COUNT | publish|overwrite ADDRESS COUNT PREVIOUS_GENERATION | upload|download ADDRESS | hold-catalog|check-catalog DIR"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_checks_lengths_percentiles_and_hashes() {
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0, 5.0], 50), 3.0);
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0, 5.0], 99), 5.0);
        assert_eq!(
            headers(&mut &b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\n\r\n"[..]),
            1024
        );
        assert_eq!(
            hashes(0, 0),
            ("1B2M2Y8AsgTpgAmY7PhCfg==".into(), "AAAAAA==".into())
        );
        assert_ne!(hashes(1024, 0x61), hashes(1024, 0x62));
    }

    #[test]
    #[should_panic(expected = "bounded benchmark expects Content-Length")]
    fn benchmark_rejects_unframed_response() {
        headers(&mut &b"HTTP/1.1 200 OK\r\n\r\n"[..]);
    }

    #[test]
    #[should_panic(expected = "generation was reused")]
    fn benchmark_rejects_reused_overwrite_generation() {
        checked_generation(&json!({"generation":"100"}), 100);
    }
}
