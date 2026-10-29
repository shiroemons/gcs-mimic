# Bucket Autoclass and object storage class on write: evidence

<!-- markdownlint-configure-file {"MD013": false} -->

Contract: [slice](../slices/bucket-autoclass-storage-class.md). Verified
2026-10-29 on the pinned Rust toolchain from the repository checkout.

| Command | Result |
| --- | --- |
| `cargo fmt --all --check` | PASS |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS |
| `cargo test --locked --lib` | PASS (all unit tests, including `bucket::autoclass` and the schema 31 migration) |
| `cargo test --locked --test bucket_autoclass_http --test sdk_ruby_defaults_http --test unsupported_http --test object_multipart_http --test bucket_http --test auth_http --test seed --test object_versioning_http` | PASS |
| `bash scripts/test-rust.sh` | PASS: 46 test binaries and doctests |
| `cargo xtask compatibility check` | PASS |
| `bash scripts/sdk-go.sh --with-ruby` | PASS, including `ruby-bucket-autoclass` and `ruby-unsupported-fails-fast` |
| `bash sdk-scorecard/run.sh --out <dir> gcs-mimic` | Go 47/57, Ruby 42/51 (82.4); `bucket.autoclass` and `object.storage_class_on_write` pass on both SDKs; no case that passed in the 2026-10-29 baseline regressed |

SDKs: `cloud.google.com/go/storage` v1.70.0 (JSON transport) and Ruby
`google-cloud-storage` 1.62.1, pinned by `sdk-scorecard/`.
