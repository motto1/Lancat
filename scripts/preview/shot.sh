#!/usr/bin/env bash
# Headless UI screenshots with mocked Tauri APIs (no Rust needed).
#   scripts/preview/shot.sh "window=panel&theme=dark" out.png [W H]
set -euo pipefail
cd "$(dirname "$0")/../.."
Q="${1:-window=panel}"
OUT="${2:-target/preview/panel.png}"
W="${3:-376}"
H="${4:-536}"
PREV=target/preview
mkdir -p "$PREV"
[ -d dist ] || npm run build >/dev/null
rm -rf "$PREV/site" && cp -r dist "$PREV/site"
cp scripts/preview/mock.js "$PREV/site/mock.js"
sed -i 's#<head>#<head><script src="/mock.js"></script><style>html{background:linear-gradient(135deg,\#4b6cb7,\#a8c0ff 60%,\#f6d365)}</style>#' "$PREV/site/index.html"
PORT=47999
(cd "$PREV/site" && python3 -m http.server $PORT >/dev/null 2>&1 &)
sleep 0.6
CHROME=$(command -v google-chrome-stable || command -v chromium || command -v google-chrome)
timeout 40 "$CHROME" --headless=new --disable-gpu --no-first-run --user-data-dir="$PWD/$PREV/chrome-profile" \
  --hide-scrollbars --force-device-scale-factor=2 --window-size="$W,$H" --virtual-time-budget=2500 \
  --screenshot="$PWD/$OUT" "http://127.0.0.1:$PORT/index.html?$Q" >/dev/null 2>&1 || true
pkill -f "http.server $PORT" || true
ls -la "$OUT"
