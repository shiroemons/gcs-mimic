#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [ "$#" != 4 ] || [[ ! "$1" =~ ^https://(localhost|127\.0\.0\.1|gcs|host\.docker\.internal|\[::1\]):[0-9]+$ ]] \
  || [[ ! "$2" =~ ^cors-browser-[a-z0-9-]+$ ]] \
  || [[ ! "$3" =~ ^(strict|permissive)$ ]] || [[ ! "$4" =~ ^(initial|restart)$ ]]; then
  echo 'Usage: bash scripts/cors-browser.sh https://LOCAL_HOST:PORT cors-browser-NAME strict|permissive initial|restart' >&2
  echo 'Requires Node.js, Chromium and the emulator CA already trusted in an isolated browser environment.' >&2
  exit 2
fi
port=${CORS_BROWSER_PORT:-54613}
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
node -e '
  const https = require("node:https"), fs = require("node:fs");
  const html = fs.readFileSync("scripts/cors-browser.html");
  const tls = {
    cert: fs.readFileSync("src/tls/development-cert.pem"),
    key: fs.readFileSync("src/tls/development-key.pem")
  };
  https.createServer(tls, (request, response) => {
    response.writeHead(200, {"Content-Type": "text/html; charset=utf-8", "Cache-Control": "no-store"});
    response.end(html);
  }).listen(Number(process.argv[1]), "127.0.0.1", () => console.log("ready"));
' "$port" > "$work/origin.log" 2>&1 &
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
printf 'API origin: %s; browser origin: https://127.0.0.1:%s; profile: %s; phase: %s\n' "$1" "$port" "$3" "$4"
"$browser" "${args[@]}" \
  "https://127.0.0.1:$port/?endpoint=$1&bucket=$2&profile=$3&phase=$4" \
  > "$work/result.html" 2> "$work/browser.log"
sed -n '/<pre id="results">/,/<\/pre>/p' "$work/result.html" \
  | sed 's/^.*<pre id="results">//; s/<\/pre>.*$//'
if ! grep -q '<body data-status="pass">' "$work/result.html"; then
  sed -n '1,50p' "$work/browser.log" >&2
  exit 1
fi
