#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
usage() {
  echo 'Usage: signed-browser.sh prepare https://LOCAL_HOST:PORT signed-browser-NAME initial|restart > urls.json' >&2
  echo '       signed-browser.sh run urls.json' >&2
  echo 'Prepare uses GCS_MIMIC_BIN (default target/debug/gcs-mimic); run needs Node.js and trusted Chromium.' >&2
  exit 2
}
if [ "${1:-}" = prepare ]; then
  [ "$#" = 4 ] || usage
  node - "$2" "$3" "$4" "${GCS_MIMIC_BIN:-target/debug/gcs-mimic}" <<'NODE'
const {execFileSync} = require('node:child_process');
const [endpoint, bucket, phase, binary] = process.argv.slice(2);
if (!/^https:\/\/(localhost|127\.0\.0\.1|gcs|host\.docker\.internal|\[::1\]):[0-9]+$/.test(endpoint)
    || !/^signed-browser-[a-z0-9-]+$/.test(bucket) || !['initial', 'restart'].includes(phase)) {
  throw new Error('Use a local HTTPS endpoint, disposable signed-browser-* bucket and initial|restart phase');
}
const sign = (method, object = 'media', extra = []) => execFileSync(binary, [
  'url', 'sign', '--base-url', endpoint, '--bucket', bucket, '--object', object,
  '--method', method, '--expires', '15m', '--signer', 'local-viewer@example.invalid',
  '--private-key', 'fixtures/signing/test-only-private.pem', ...extra
], {encoding: 'utf8'}).trim();
const headers = ['--header', 'Content-Type:application/octet-stream', '--header', 'x-goog-meta-browser:original'];
const urls = {
  get: sign('GET'), head: sign('HEAD'), put: sign('PUT', 'media', headers),
  override: sign('GET', 'media', ['--query', 'response-content-type=text/custom',
    '--query', 'response-content-disposition=attachment; filename="browser.txt"']),
  unpublished: sign('PUT', 'unpublished', headers),
  expired: sign('PUT', 'unpublished', [...headers, '--timestamp', '20000101T000000Z'])
};
process.stdout.write(JSON.stringify({endpoint, bucket, phase, urls}) + '\n');
NODE
  exit
fi
[ "${1:-}" = run ] && [ "$#" = 2 ] || usage
manifest=$2
port=${SIGNED_BROWSER_PORT:-54614}
[[ "$port" =~ ^[0-9]+$ ]] && ((port > 1023 && port < 65536))
work=$(mktemp -d)
origin_pid=
cleanup() {
  if [ -n "$origin_pid" ]; then
    kill "$origin_pid"
    wait "$origin_pid" || test "$?" = 143
  fi
  rm -r "$work"
}
trap cleanup EXIT
node - "$manifest" "$port" <<'NODE' > "$work/origin.log" 2>&1 &
const https = require('node:https'), fs = require('node:fs');
const manifest = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const endpoint = new URL(manifest.endpoint);
if (!/^https:\/\/(localhost|127\.0\.0\.1|gcs|host\.docker\.internal|\[::1\]):[0-9]+$/.test(manifest.endpoint)
    || !/^signed-browser-[a-z0-9-]+$/.test(manifest.bucket)
    || !['initial', 'restart'].includes(manifest.phase)) throw new Error('Invalid local fixture manifest');
for (const value of Object.values(manifest.urls)) {
  const url = new URL(value);
  if (url.origin !== endpoint.origin || !url.pathname.startsWith(`/${manifest.bucket}/`)
      || url.searchParams.get('X-Goog-Algorithm') !== 'GOOG4-RSA-SHA256') throw new Error('Invalid signed fixture URL');
}
const html = fs.readFileSync('scripts/signed-browser.html');
https.createServer({cert: fs.readFileSync('src/tls/development-cert.pem'),
  key: fs.readFileSync('src/tls/development-key.pem')}, (request, response) => {
  response.writeHead(200, {'Content-Type': request.url === '/urls.json' ? 'application/json' : 'text/html; charset=utf-8',
    'Cache-Control': 'no-store'});
  response.end(request.url === '/urls.json' ? JSON.stringify(manifest) : html);
}).listen(Number(process.argv[3]), '127.0.0.1', () => console.log('ready'));
NODE
origin_pid=$!
for _ in {1..50}; do
  if grep -q '^ready$' "$work/origin.log"; then break; fi
  if ! kill -0 "$origin_pid" 2>/dev/null; then sed -n '1,30p' "$work/origin.log" >&2; exit 1; fi
  sleep 0.1
done
grep -q '^ready$' "$work/origin.log"
browser=${CHROMIUM_BIN:-chromium}
args=(--headless --disable-gpu --no-proxy-server --disable-background-networking
  --user-data-dir="$work/profile" --dump-dom --timeout=60000 --virtual-time-budget=30000)
if [ "$(id -u)" = 0 ]; then args+=(--no-sandbox); fi
"$browser" --version
"$browser" "${args[@]}" "https://127.0.0.1:$port/" > "$work/result.html" 2> "$work/browser.log"
sed -n '/<pre id="results">/,/<\/pre>/p' "$work/result.html" \
  | sed 's/^.*<pre id="results">//; s/<\/pre>.*$//'
if ! grep -q '<body data-status="pass">' "$work/result.html"; then
  sed -n '1,50p' "$work/browser.log" >&2
  exit 1
fi
