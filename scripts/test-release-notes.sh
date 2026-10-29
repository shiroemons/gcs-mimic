#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/scripts" "$work/docs/en/releases" "$work/docs/en/slices" "$work/sdk-scorecard"
cp scripts/release-notes.sh "$work/scripts/"
touch "$work/docs/en/releases/0.1.0.md" "$work/docs/en/slices/acl.md" "$work/sdk-scorecard/README.md"
export GITHUB_REPOSITORY=example/gcs-mimic
render() { bash "$work/scripts/release-notes.sh" "$@"; }
blob=https://github.com/example/gcs-mimic/blob/v9.9.9

cat > "$work/docs/en/releases/9.9.9.md" <<'MD'
# Notes

See [the old notes](0.1.0.md#known-limitations), [ACLs](../slices/acl.md),
[the scorecard](../../../sdk-scorecard/README.md) and [ACLs](./../slices/acl.md).
Keep [this section](#notes), [upstream](https://example.com/a_(b)) and [mail](mailto:a@example.com).

```sh
echo '[literal](not-a-link.md)'
```

[old]: 0.1.0.md
[web]: https://example.com/
MD
cat > "$work/expected.md" <<MD
# Notes

See [the old notes]($blob/docs/en/releases/0.1.0.md#known-limitations), [ACLs]($blob/docs/en/slices/acl.md),
[the scorecard]($blob/sdk-scorecard/README.md) and [ACLs]($blob/docs/en/slices/acl.md).
Keep [this section](#notes), [upstream](https://example.com/a_(b)) and [mail](mailto:a@example.com).

\`\`\`sh
echo '[literal](not-a-link.md)'
\`\`\`

[old]: $blob/docs/en/releases/0.1.0.md
[web]: https://example.com/
MD
render docs/en/releases/9.9.9.md v9.9.9 > "$work/actual.md"
diff -u "$work/expected.md" "$work/actual.md"

expect_failure() {
  local name=$1 message=$2
  shift 2
  if render "$@" > /dev/null 2> "$work/error"; then
    echo "Release notes accepted $name" >&2
    exit 1
  fi
  if [[ -n "$message" ]]; then
    grep -q "$message" "$work/error"
  fi
}
printf '[gone](absent.md)\n' > "$work/docs/en/releases/missing.md"
expect_failure 'a link without a target' 'has no target' docs/en/releases/missing.md v9.9.9
printf '[out](../../../../outside.md)\n' > "$work/docs/en/releases/escape.md"
expect_failure 'a link outside the repository' 'leaves the repository' docs/en/releases/escape.md v9.9.9
printf '[run](x`touch${IFS}%s/ran`.md)\n' "$work" > "$work/docs/en/releases/shell.md"
expect_failure 'shell syntax in a link' 'supported path set' docs/en/releases/shell.md v9.9.9
if [[ -e "$work/ran" ]]; then
  echo 'A link target reached a shell' >&2
  exit 1
fi
: > "$work/docs/en/releases/empty.md"
expect_failure 'empty notes' '' docs/en/releases/empty.md v9.9.9
expect_failure 'an absolute notes path' '' "$work/docs/en/releases/9.9.9.md" v9.9.9
expect_failure 'a ref with a slash' '' docs/en/releases/9.9.9.md refs/tags/v9.9.9

# Every committed release note must render without a broken link.
for notes in docs/en/releases/[0-9]*.md; do
  bash scripts/release-notes.sh "$notes" main > /dev/null
done
echo 'PASS: release notes rewrite repository links to the released ref, keep other links and code, and reject broken links'
