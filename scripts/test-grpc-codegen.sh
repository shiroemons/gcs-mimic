#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
mkdir -p "$fixture/scripts" "$fixture/src/grpc/generated" "$fixture/src/grpc/proto" "$fixture/tools" "$fixture/baseline"
cp scripts/check-grpc-codegen.sh "$fixture/scripts/"
printf 'proto\n' > "$fixture/src/grpc/proto/test.proto"
printf 'generated\n' > "$fixture/src/grpc/generated/test.rs"
cp "$fixture/src/grpc/generated/test.rs" "$fixture/baseline/"
(
  cd "$fixture"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum src/grpc/proto/test.proto > src/grpc/proto/SHA256SUMS
  else
    shasum -a 256 src/grpc/proto/test.proto > src/grpc/proto/SHA256SUMS
  fi
)
# This fixture replaces only the expensive generator; hash checking and diff stay real.
printf '%s\n' '#!/usr/bin/env bash' 'set -euo pipefail' \
  'while [[ $# -gt 0 && $1 != --out-dir ]]; do shift; done' \
  'test $# = 2' 'cp "$GCSM_CODEGEN_FIXTURE/baseline/test.rs" "$2/"' > "$fixture/tools/cargo"
chmod +x "$fixture/tools/cargo"
export GCSM_CODEGEN_FIXTURE="$fixture"
export PATH="$fixture/tools:$PATH"
bash "$fixture/scripts/check-grpc-codegen.sh" > "$fixture/pass.log" 2>&1

printf 'changed proto\n' > "$fixture/src/grpc/proto/test.proto"
if bash "$fixture/scripts/check-grpc-codegen.sh" > "$fixture/hash.log" 2>&1; then
  echo 'Codegen check accepted a changed protobuf source' >&2
  exit 1
fi
if ! grep -q 'FAILED' "$fixture/hash.log"; then
  sed -n '1,100p' "$fixture/hash.log" >&2
  exit 1
fi
printf 'proto\n' > "$fixture/src/grpc/proto/test.proto"
printf 'drift\n' > "$fixture/src/grpc/generated/test.rs"
if bash "$fixture/scripts/check-grpc-codegen.sh" > "$fixture/drift.log" 2>&1; then
  echo 'Codegen check accepted stale generated Rust' >&2
  exit 1
fi
if ! grep -q '^diff ' "$fixture/drift.log"; then
  sed -n '1,100p' "$fixture/drift.log" >&2
  exit 1
fi
printf 'Codegen checks: matching output accepted; changed proto and stale generated output rejected\n'
