#!/usr/bin/env python3
"""Downloads the bundled Noto Animated Emoji subset (CC BY 4.0) listed in src/lib/emoji-catalog.ts
into src/assets/emoji/<code>.json and minifies it:
  * numbers with |v| >= 1 rounded to 1 decimal (coordinates on a 512 px canvas shown at <= 160 px:
    0.1 unit = 0.03 px), |v| < 1 (colours, easing handles) to 3 decimals;
  * layer/shape names and HTML id/class hints dropped ("nm", "mn", "ln", "cl"; unused by the SVG
    renderer, the emoji use no expressions);
  * whitespace stripped.
This took the 116 files from 952 KB to 719 KB brotli-compressed (what tauri embeds), see
docs/postmortem-memory.md.
Usage: python3 scripts/fetch-emoji.py            (idempotent; skips files that already exist)
       python3 scripts/fetch-emoji.py --reminify (re-applies the minification to existing files)"""
import concurrent.futures, json, os, re, sys, urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CAT = os.path.join(ROOT, "src", "lib", "emoji-catalog.ts")
OUT = os.path.join(ROOT, "src", "assets", "emoji")
URL = "https://fonts.gstatic.com/s/e/notoemoji/latest/{}/lottie.json"


DROP = {"nm", "mn", "ln", "cl"}


def rnd(o):
    if isinstance(o, float):
        r = round(o, 1 if abs(o) >= 1 else 3)
        return int(r) if r == int(r) else r
    if isinstance(o, list):
        return [rnd(x) for x in o]
    if isinstance(o, dict):
        return {k: rnd(v) for k, v in o.items() if k not in DROP}
    return o


def minify(data):
    return json.dumps(rnd(data), separators=(",", ":"))


def fetch(code):
    path = os.path.join(OUT, f"{code}.json")
    if os.path.exists(path) and os.path.getsize(path) > 0:
        return code, "cached"
    data = json.loads(urllib.request.urlopen(URL.format(code), timeout=30).read())
    with open(path, "w") as f:
        f.write(minify(data))
    return code, "ok"


def main():
    codes = re.findall(r'"code": "([0-9a-f_]+)"', open(CAT, encoding="utf-8").read())
    os.makedirs(OUT, exist_ok=True)
    if "--reminify" in sys.argv:
        for code in codes:
            path = os.path.join(OUT, f"{code}.json")
            if os.path.exists(path):
                data = json.load(open(path))
                with open(path, "w") as f:
                    f.write(minify(data))
        print(f"re-minified {len(codes)} files in {OUT}")
        return
    failed = []
    with concurrent.futures.ThreadPoolExecutor(12) as ex:
        futs = {ex.submit(fetch, c): c for c in codes}
        for f in concurrent.futures.as_completed(futs):
            try:
                f.result()
            except Exception as e:  # noqa: BLE001
                failed.append((futs[f], str(e)))
    print(f"{len(codes) - len(failed)}/{len(codes)} emoji in {OUT}")
    if failed:
        print("failed:", failed)
        sys.exit(1)


if __name__ == "__main__":
    main()
