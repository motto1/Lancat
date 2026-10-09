#!/usr/bin/env bash
# memprobe.sh — memory gate for the Lancat desktop app (Linux).
#
# Starts the app with a throw-away data dir, then samples the WHOLE process tree
# (lancat + WebKitWebProcess / WebKitNetworkProcess / bwrap / dbus-proxy …) from
# /proc/<pid>/smaps_rollup in three states:
#
#   idle    panel collapsed, after --settle seconds
#   open    panel expanded (SIGRTMIN+6), after --open-wait seconds
#   closed  collapsed again (SIGRTMIN+7), after --after seconds
#
# Per process it records PSS, USS (= Private_Clean + Private_Dirty) and SwapPss.
# The gates use "priv" = USS + SwapPss so that pages the kernel swapped out still count.
#
#   A  idle   : sum(priv) over the tree  <= --max-idle   (MB, default 40)
#   A' closed : sum(priv) over the tree  <= --max-idle   (same gate, after the panel was used)
#   B  open   : sum(pss + swappss)       <= --max-open   (MB, default 200)
#
# Output: a table on stderr, JSON on stdout (or --json FILE). Exit 1 when a gate fails,
# 2 on setup errors.
#
# Usage:
#   scripts/memprobe.sh [--bin target/release/lancat] [--settle 25] [--open-wait 6] [--after 75]
#                       [--max-idle 40] [--max-open 200] [--json out.json] [--no-peer]
#                       [--env 'K=V K2=V2'] [--label text] [--no-gate]
#
# Needs a graphical session. Over SSH on the niri dev box:
#   XDG_RUNTIME_DIR=/run/user/$(id -u) WAYLAND_DISPLAY=wayland-1 scripts/memprobe.sh
set -uo pipefail
cd "$(dirname "$0")/.."

BIN=target/release/lancat
CLI=target/release/lancat-cli
SETTLE=25
OPEN_WAIT=6
AFTER=75
MAX_IDLE=40
MAX_OPEN=200
JSON_OUT=""
PEER=1
EXTRA_ENV=""
LABEL=""
GATE=1
while [ $# -gt 0 ]; do
  case "$1" in
    --bin) BIN=$2; shift ;;
    --cli) CLI=$2; shift ;;
    --settle) SETTLE=$2; shift ;;
    --open-wait) OPEN_WAIT=$2; shift ;;
    --after) AFTER=$2; shift ;;
    --max-idle) MAX_IDLE=$2; shift ;;
    --max-open) MAX_OPEN=$2; shift ;;
    --json) JSON_OUT=$2; shift ;;
    --no-peer) PEER=0 ;;
    --env) EXTRA_ENV=$2; shift ;;
    --label) LABEL=$2; shift ;;
    --no-gate) GATE=0 ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "unknown arg $1" >&2; exit 2 ;;
  esac
  shift
done

[ -x "$BIN" ] || { echo "no binary at $BIN (build first)" >&2; exit 2; }
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
[ -n "${WAYLAND_DISPLAY:-}${DISPLAY:-}" ] || export WAYLAND_DISPLAY=wayland-1

T=$(mktemp -d /tmp/lancat-memprobe.XXXXXX)
PORT=$((48000 + RANDOM % 1000))
APP_PID=""
tree_pids() { # all descendants of $1, including itself
  python3 -c '
import os,sys
kids={}
for d in os.listdir("/proc"):
    if d.isdigit():
        try:
            st=open(f"/proc/{d}/stat").read(); kids.setdefault(int(st[st.rindex(")")+2:].split()[1]),[]).append(int(d))
        except Exception: pass
out,todo=[],[int(sys.argv[1])]
while todo:
    p=todo.pop(); out.append(p); todo+=kids.get(p,[])
print(" ".join(map(str,out)))' "$1"
}
cleanup() {
  if [ -n "$APP_PID" ]; then
    local pids; pids=$(tree_pids "$APP_PID")
    kill $pids 2>/dev/null; sleep 1; kill -9 $pids 2>/dev/null
  fi
  rm -rf "$T"
}
trap cleanup EXIT

# shellcheck disable=SC2086
env LANCAT_DATA_DIR="$T/data" LANCAT_RECV_DIR="$T/recv" LANCAT_PORT=$PORT LANCAT_UDP_PORT=$PORT \
    LANCAT_NO_MDNS=1 LANCAT_NAME=memprobe RUST_LOG=info $EXTRA_ENV \
    "$BIN" > "$T/app.log" 2>&1 < /dev/null &
APP_PID=$!
sleep 2
kill -0 "$APP_PID" 2>/dev/null || { echo "app exited early:" >&2; cat "$T/app.log" >&2; exit 2; }

if [ "$PEER" = 1 ] && [ -x "$CLI" ]; then
  # one peer with a short conversation, so the panel renders real content and the dock shows unread
  "$CLI" send --to 127.0.0.1:$PORT --data "$T/peer" --text "memprobe 你好 👋" --text "第二条消息" --text "🎉" --timeout 15 \
    > "$T/peer.log" 2>&1 || echo "warn: peer send failed (see below)" >&2
fi

sample() { # $1 = state name
  python3 - "$APP_PID" "$1" <<'PY'
import os, sys, json
root, state = int(sys.argv[1]), sys.argv[2]
kids = {}
for d in os.listdir('/proc'):
    if not d.isdigit():
        continue
    try:
        st = open(f'/proc/{d}/stat').read()
        ppid = int(st[st.rindex(')') + 2:].split()[1])
        kids.setdefault(ppid, []).append(int(d))
    except Exception:
        pass
tree, todo = [], [root]
while todo:
    p = todo.pop()
    tree.append(p)
    todo += kids.get(p, [])
procs = []
for p in tree:
    try:
        f = {}
        for line in open(f'/proc/{p}/smaps_rollup'):
            parts = line.split()
            if len(parts) >= 3 and parts[1].isdigit():
                f[parts[0].rstrip(':')] = int(parts[1])
        name = open(f'/proc/{p}/comm').read().strip()
        threads = sum(1 for _ in os.listdir(f'/proc/{p}/task'))
    except Exception:
        continue
    uss = f.get('Private_Clean', 0) + f.get('Private_Dirty', 0)
    procs.append({'pid': p, 'name': name, 'threads': threads,
                  'rss_kb': f.get('Rss', 0), 'pss_kb': f.get('Pss', 0), 'uss_kb': uss,
                  'swap_pss_kb': f.get('SwapPss', 0), 'priv_kb': uss + f.get('SwapPss', 0),
                  'anon_kb': f.get('Anonymous', 0)})
tot = {k: sum(x[k] for x in procs) for k in ('rss_kb', 'pss_kb', 'uss_kb', 'swap_pss_kb', 'priv_kb')}
tot['pss_swap_kb'] = tot['pss_kb'] + tot['swap_pss_kb']
print(json.dumps({'state': state, 'procs': procs, 'total': tot}))
PY
}

echo "settling ${SETTLE}s (collapsed)…" >&2
sleep "$SETTLE"
S_IDLE=$(sample idle)
kill -s RTMIN+6 "$APP_PID"
sleep "$OPEN_WAIT"
S_OPEN=$(sample open)
kill -s RTMIN+7 "$APP_PID"
echo "collapsed, waiting ${AFTER}s…" >&2
sleep "$AFTER"
S_CLOSED=$(sample closed)
kill -0 "$APP_PID" 2>/dev/null || { echo "app died during the probe:" >&2; tail -30 "$T/app.log" >&2; exit 2; }
LAT=$(grep -oE 'panel shown [0-9]+ ms' "$T/app.log" | grep -oE '[0-9]+' | head -1)
SIZE=$(stat -c %s "$BIN")

python3 - "$MAX_IDLE" "$MAX_OPEN" "${LAT:-}" "$SIZE" "$LABEL" "$GATE" "$JSON_OUT" "$S_IDLE" "$S_OPEN" "$S_CLOSED" <<'PY'
import json, sys
max_idle, max_open, lat, size, label, gate, out = sys.argv[1:8]
samples = [json.loads(s) for s in sys.argv[8:]]
mb = lambda kb: kb / 1024
err = sys.stderr
print(f"\n== Lancat memprobe {label}".rstrip(), file=err)
for s in samples:
    print(f"\n[{s['state']}]", file=err)
    print(f"  {'process':<22}{'pid':>8}{'thr':>5}{'PSS':>9}{'USS':>9}{'SwapPss':>9}{'priv':>9}", file=err)
    for p in sorted(s['procs'], key=lambda p: -p['pss_kb']):
        print(f"  {p['name']:<22}{p['pid']:>8}{p['threads']:>5}{mb(p['pss_kb']):>8.1f}M{mb(p['uss_kb']):>8.1f}M{mb(p['swap_pss_kb']):>8.1f}M{mb(p['priv_kb']):>8.1f}M", file=err)
    t = s['total']
    print(f"  {'TOTAL':<35}{mb(t['pss_kb']):>8.1f}M{mb(t['uss_kb']):>8.1f}M{mb(t['swap_pss_kb']):>8.1f}M{mb(t['priv_kb']):>8.1f}M", file=err)
by = {s['state']: s for s in samples}
res = {
    'label': label,
    'binary_bytes': int(size),
    'open_latency_ms': int(lat) if lat else None,
    'samples': samples,
    'gates': {
        'A_idle_priv_mb': round(mb(by['idle']['total']['priv_kb']), 1),
        'A_closed_priv_mb': round(mb(by['closed']['total']['priv_kb']), 1),
        'B_open_pss_mb': round(mb(by['open']['total']['pss_swap_kb']), 1),
        'max_idle_mb': float(max_idle), 'max_open_mb': float(max_open),
    },
}
g = res['gates']
fail = []
if g['A_idle_priv_mb'] > g['max_idle_mb']: fail.append(f"A idle {g['A_idle_priv_mb']}MB > {max_idle}MB")
if g['A_closed_priv_mb'] > g['max_idle_mb']: fail.append(f"A closed {g['A_closed_priv_mb']}MB > {max_idle}MB")
if g['B_open_pss_mb'] > g['max_open_mb']: fail.append(f"B open {g['B_open_pss_mb']}MB > {max_open}MB")
res['failures'] = fail
print(f"\nA idle priv {g['A_idle_priv_mb']}MB | A closed priv {g['A_closed_priv_mb']}MB | B open PSS {g['B_open_pss_mb']}MB"
      f" | open latency {res['open_latency_ms']} ms | binary {int(size)/1e6:.2f}MB", file=err)
print("GATE: " + ("FAIL — " + "; ".join(fail) if fail else "PASS"), file=err)
js = json.dumps(res, ensure_ascii=False, indent=1)
if out:
    open(out, 'w').write(js)
else:
    print(js)
sys.exit(1 if (fail and gate == '1') else 0)
PY
