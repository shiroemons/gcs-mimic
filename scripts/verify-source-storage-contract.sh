#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
manifest=compatibility/upstream/source-storage-contract.json
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

verify_digest() {
  local bits=$1 expected=$2 file=$3 actual
  actual=$(shasum -a "$bits" "$file" | awk '{print $1}')
  if [[ $actual != "$expected" ]]; then
    echo "SHA-$bits mismatch: expected $expected, got $actual" >&2
    return 1
  fi
}

verify_blob() {
  local expected=$1 file=$2 actual
  actual=$(git hash-object --no-filters "$file")
  if [[ $actual != "$expected" ]]; then
    echo "Git blob mismatch: expected $expected, got $actual" >&2
    return 1
  fi
}

fetch() {
  curl -q --fail --silent --show-error --location --max-time 30 \
    --max-filesize 8388608 --proto '=https' --proto-redir '=https' \
    --output "$2" "$1"
}

if [[ ${1:-} == --self-test && $# == 1 ]]; then
  : >"$work/empty"
  printf 'changed' >"$work/changed"
  sha256=e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
  sha512=cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e
  blob=e69de29bb2d1d6434b8b29ae775ad8c2e48c5391
  verify_digest 256 "$sha256" "$work/empty"
  verify_digest 512 "$sha512" "$work/empty"
  verify_blob "$blob" "$work/empty"
  if verify_digest 256 "$sha256" "$work/changed" 2>"$work/error"; then
    echo 'Changed bytes passed SHA-256 verification' >&2
    exit 1
  fi
  if verify_digest 512 "$sha512" "$work/changed" 2>"$work/error"; then
    echo 'Changed bytes passed SHA-512 verification' >&2
    exit 1
  fi
  if verify_blob "$blob" "$work/changed" 2>"$work/error"; then
    echo 'Changed bytes passed Git blob verification' >&2
    exit 1
  fi
  echo 'PASS: known digests accepted, altered bytes rejected (fetch/fallback/HTTP branches not tested)'
  exit 0
fi
if [[ $# != 0 ]]; then
  echo 'Usage: bash scripts/verify-source-storage-contract.sh [--self-test]' >&2
  exit 2
fi

reference=$(jq -er '.reference.commit' "$manifest")
jq -e '.reference.git_blobs | length == 14' "$manifest" >/dev/null
jq -e '.mime_profile.go.sources | length == 2' "$manifest" >/dev/null
while IFS=$'\t' read -r path blob; do
  fetch "https://raw.githubusercontent.com/fsouza/fake-gcs-server/$reference/$path" "$work/reference"
  verify_blob "$blob" "$work/reference"
  if [[ $path == main.go ]]; then
    verify_digest 256 "$(jq -er '.mime_profile.yaml.source_sha256' "$manifest")" "$work/reference"
  fi
done < <(jq -r '.reference.git_blobs[] | [.path, .blob] | @tsv' "$manifest")
echo 'PASS: all 14 reference Git blobs and YAML source SHA-256'

go_commit=$(jq -er '.mime_profile.go.commit' "$manifest")
while IFS=$'\t' read -r path blob sha256; do
  fetch "https://raw.githubusercontent.com/golang/go/$go_commit/$path" "$work/go"
  verify_blob "$blob" "$work/go"
  verify_digest 256 "$sha256" "$work/go"
done < <(jq -r '.mime_profile.go.sources[] | [.path, .blob, .sha256] | @tsv' "$manifest")
echo 'PASS: Go MIME Git blobs and SHA-256'

fetch "$(jq -er '.mime_profile.mailcap.apkbuild_url' "$manifest")" "$work/APKBUILD"
source_sha512=$(jq -er '.mime_profile.mailcap.source_sha512' "$manifest")
source_member=$(jq -er '.mime_profile.mailcap.source_member' "$manifest")
source_name=${source_member%%/*}.tar.xz
source_version=${source_name#mailcap-}
source_version=${source_version%.tar.xz}
grep -Fqx "$source_sha512  $source_name" "$work/APKBUILD"
grep -Fqx "source=\"https://releases.pagure.org/mailcap/mailcap-\$pkgver.tar.xz\"" "$work/APKBUILD"
grep -Fqx "pkgver=$source_version" "$work/APKBUILD"
if ! fetch "$(jq -er '.mime_profile.mailcap.source_url' "$manifest")" "$work/source.tar.xz"; then
  echo 'Source endpoint unavailable; verifying the pinned Alpine source mirror' >&2
  fetch "$(jq -er '.mime_profile.mailcap.source_mirror_url' "$manifest")" "$work/source.tar.xz"
fi
verify_digest 512 "$source_sha512" "$work/source.tar.xz"
tar -xOf "$work/source.tar.xz" "$source_member" >"$work/mime.types"
member_sha256=$(jq -er '.mime_profile.mailcap.member_sha256' "$manifest")
verify_digest 256 "$member_sha256" "$work/mime.types"
echo 'PASS: APKBUILD source SHA-512 and MIME member SHA-256'

if package_status=$(curl -q --fail --silent --show-error --location --max-time 30 \
  --max-filesize 8388608 --proto '=https' --proto-redir '=https' \
  --write-out '%{http_code}' --output "$work/mailcap.apk" \
  "$(jq -er '.mime_profile.mailcap.package_url' "$manifest")"); then
  test "$(wc -c <"$work/mailcap.apk" | tr -d ' ')" = "$(jq -er '.mime_profile.mailcap.package_bytes' "$manifest")"
  verify_digest 256 "$(jq -er '.mime_profile.mailcap.package_sha256' "$manifest")" "$work/mailcap.apk"
  tar -xOf "$work/mailcap.apk" "$(jq -er '.mime_profile.mailcap.member' "$manifest")" >"$work/package-mime.types"
  verify_digest 256 "$member_sha256" "$work/package-mime.types"
  echo 'PASS: mailcap APK bytes, SHA-256 and MIME member SHA-256'
elif [[ $package_status == 404 || $package_status == 410 ]]; then
  echo "APK unavailable (HTTP $package_status); source route verified, APK digest not reverified"
else
  echo "APK verification failed (HTTP $package_status)" >&2
  exit 1
fi
