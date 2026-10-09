#!/usr/bin/env python3
"""Downloads the bundled Noto Animated Emoji subset (CC BY 4.0) listed in src/lib/emoji-catalog.ts
into src/assets/emoji/<code>.json, rounding numbers to 3 decimals and stripping whitespace.
Usage: python3 scripts/fetch-emoji.py   (idempotent; skips files that already exist)"""
import concurrent.futures, json, os, re, sys, urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CAT = os.path.join(ROOT, "src", "lib", "emoji-catalog.ts")
OUT = os.path.join(ROOT, "src", "assets", "emoji")
URL = "https://fonts.gstatic.com/s/e/notoemoji/latest/{}/lottie.json"


def rnd(o):
    if isinstance(o, float):
        r = round(o, 3)
        return int(r) if r == int(r) else r
    if isinstance(o, list):
        return [rnd(x) for x in o]
    if isinstance(o, dict):
        return {k: rnd(v) for k, v in o.items()}
    return o


def fetch(code):
    path = os.path.join(OUT, f"{code}.json")
    if os.path.exists(path) and os.path.getsize(path) > 0:
        return code, "cached"
    data = json.loads(urllib.request.urlopen(URL.format(code), timeout=30).read())
    with open(path, "w") as f:
        f.write(json.dumps(rnd(data), separators=(",", ":")))
    return code, "ok"


def main():
    codes = re.findall(r'"code": "([0-9a-f_]+)"', open(CAT, encoding="utf-8").read())
    os.makedirs(OUT, exist_ok=True)
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
