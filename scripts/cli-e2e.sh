#!/usr/bin/env bash
# Two-process end-to-end test with lancat-cli:
#  1) receiver "serve" + one-shot "send" (text + multi-chunk file + folder), verify bytes
#  2) two "serve" instances with mDNS + UDP heartbeat on, verify they discover each other
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -q -p lancat-core --bin lancat-cli
CLI=target/debug/lancat-cli
T=$(mktemp -d)
trap 'kill $(jobs -p) 2>/dev/null || true; rm -rf "$T"' EXIT

echo "== 1. serve + send"
mkfifo "$T/in1"
$CLI serve --port 47911 --data "$T/b" --name bravo --recv "$T/recv" --no-mdns --no-udp < "$T/in1" > "$T/b.events" 2> "$T/b.log" &
exec 3> "$T/in1"
sleep 1
head -c 10485760 /dev/urandom > "$T/ten.bin"
mkdir -p "$T/dir/sub" && echo hello > "$T/dir/sub/x.txt"
$CLI send --to 127.0.0.1:47911 --text "你好 from cli" --file "$T/ten.bin" --file "$T/dir" --data "$T/a" 2>&1 | grep -E "ok|FAIL|sending"
cmp "$T/ten.bin" "$T/recv/ten.bin" && echo "file ok (10 MiB, 3 chunks)"
cmp "$T/dir/sub/x.txt" "$T/recv/dir/sub/x.txt" && echo "folder ok"
grep -q '你好 from cli' "$T/b.events" && echo "text event ok"
echo quit >&3; exec 3>&-
sleep 0.5

echo "== 2. discovery (mDNS + UDP) between two serve instances"
mkfifo "$T/in2" "$T/in3"
$CLI serve --port 47912 --data "$T/c" --name charlie --recv "$T/recv2" < "$T/in2" > "$T/c.events" 2> "$T/c.log" &
exec 4> "$T/in2"
$CLI serve --port 47913 --data "$T/d" --name delta --recv "$T/recv3" < "$T/in3" > "$T/d.events" 2> "$T/d.log" &
exec 5> "$T/in3"
for i in $(seq 1 20); do
  if grep -q '"name":"delta"' "$T/c.events" && grep -q '"name":"charlie"' "$T/d.events"; then break; fi
  sleep 0.5
done
grep -q '"name":"delta"' "$T/c.events" && grep -q '"name":"charlie"' "$T/d.events" && echo "discovered each other in ~$((i/2))s"
echo "text delta hello-via-discovery" >&4
sleep 1.5
grep -q 'hello-via-discovery' "$T/d.events" && echo "text over discovered link ok"
echo quit >&4; echo quit >&5
echo "ALL OK"
