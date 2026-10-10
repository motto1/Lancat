// Lottie playback for the bundled Noto Animated Emoji and imported .tgs stickers, rendered by
// ThorVG (WebAssembly, via @lottiefiles/dotlottie-web) into one <canvas> per sticker.
//
// Why ThorVG and not lottie-web (measured in WebKitGTK, 12 stickers at 160 px, see
// docs/postmortem-memory.md): web process +47 MB instead of +135 MB, ~30 % less CPU, higher fps.
// lottie-web builds an SVG DOM per animation and rebuilds attributes every frame.
//
// Memory rules:
// * the player (JS + 1.2 MB wasm) is loaded lazily, the first time a sticker is shown;
// * at most MAX_PLAYING animations run at once; starting another pauses the one started first;
// * Lottie JSON is kept as a string (ThorVG parses it in wasm) in a small LRU (MAX_CACHED) that is
//   dropped when the panel collapses; every player is paused while the panel is hidden, and the
//   whole web process goes away when the collapsed panel is released (dock.rs).
import type { DotLottie } from "@lottiefiles/dotlottie-web";
import wasmUrl from "@lottiefiles/dotlottie-web/dotlottie-player.wasm?url";
import { gunzipSync, strFromU8 } from "fflate";
import { EMOJI, type EmojiEntry } from "./emoji-catalog";
import { api, type EmojiPayload } from "./api";

export type Player = DotLottie;

const urls = import.meta.glob("../assets/emoji/*.json", { query: "?url", import: "default", eager: true }) as Record<string, string>;

const byCode = new Map<string, EmojiEntry>(EMOJI.map((e) => [e.code, e]));
const byChar = new Map<string, EmojiEntry>(EMOJI.map((e) => [e.char.replace(/\ufe0f/g, ""), e]));

export function notoUrl(code: string): string | undefined {
  return urls[`../assets/emoji/${code}.json`];
}

export function emojiByCode(code: string) {
  return byCode.get(code);
}

export function emojiByChar(char: string) {
  return byChar.get(char.replace(/\ufe0f/g, ""));
}

export const MAX_PLAYING = 12;
const MAX_CACHED = 24;

type Lib = typeof import("@lottiefiles/dotlottie-web");
let lib: Promise<Lib> | null = null;
function player(): Promise<Lib> {
  lib ??= import("@lottiefiles/dotlottie-web").then((m) => {
    m.DotLottie.setWasmUrl(wasmUrl); // bundled, never the CDN default
    return m;
  });
  return lib;
}

const dataCache = new Map<string, Promise<string>>();
function cached(key: string, load: () => Promise<string>): Promise<string> {
  let p = dataCache.get(key);
  if (p) {
    dataCache.delete(key); // refresh LRU position
  } else {
    p = load();
    p.catch(() => dataCache.delete(key));
  }
  dataCache.set(key, p);
  while (dataCache.size > MAX_CACHED) dataCache.delete(dataCache.keys().next().value!);
  return p;
}

/** Starts loading the player JS early (in parallel with fetching the Lottie JSON). */
export function warmPlayer() {
  void player();
}

// ---- posters: frame 0 of each animation, kept across panel restarts (localStorage, small LRU).
// A cold-opened panel shows the poster until ThorVG has drawn the same frame into the canvas, so
// the hand-over to the animation is pixel-identical and has no blank or fading frame. Only an
// emoji never seen before falls back to the system emoji glyph (then crossfades).
const POSTER = "lancat.poster.v1:";
// Two LRUs so opening the emoji grid (many tiny cells) cannot evict the chat-size posters.
const posterClass = (size: number) => (size >= 64 ? { idx: "lancat.poster.v1.big", max: 48 } : { idx: "lancat.poster.v1.small", max: 200 });

export function posterKey(e: EmojiPayload, size: number): string | null {
  return e.kind === "noto" || e.kind === "sticker" ? `${e.kind}:${e.code}:${size}` : null;
}

export function getPoster(key: string | null): string | null {
  if (!key) return null;
  try {
    return localStorage.getItem(POSTER + key);
  } catch {
    return null;
  }
}

/** Stores the canvas' current pixels (call right after `load`, when frame 0 is drawn). */
export function savePoster(key: string | null, size: number, canvas: HTMLCanvasElement) {
  if (!key) return;
  try {
    const url = canvas.toDataURL("image/webp", 0.92); // WebKitGTK may fall back to PNG: still fine
    if (url.length < 64) return;
    const c = posterClass(size);
    const idx: string[] = JSON.parse(localStorage.getItem(c.idx) || "[]").filter((k: string) => k !== key);
    idx.push(key);
    while (idx.length > c.max) localStorage.removeItem(POSTER + idx.shift());
    localStorage.setItem(POSTER + key, url);
    localStorage.setItem(c.idx, JSON.stringify(idx));
  } catch {
    /* quota or tainted canvas: no poster, the glyph fallback still works */
  }
}

/** Resolves Lottie JSON text for a payload (bundled Noto, inline sticker data, or a locally imported sticker). */
export function lottieData(e: EmojiPayload): Promise<string> | null {
  if (e.kind === "noto") {
    const url = notoUrl(e.code);
    if (!url) return null;
    return cached(`noto:${e.code}`, () => fetch(url).then((r) => r.text()));
  }
  if (e.kind === "sticker") {
    return cached(`sticker:${e.code}`, () => (e.data ? Promise.resolve(e.data) : api.sticker(e.code)));
  }
  return null;
}

const live = new Set<Player>();

export async function createPlayer(canvas: HTMLCanvasElement, data: string, loop = true): Promise<Player> {
  const { DotLottie } = await player();
  const p = new DotLottie({
    canvas,
    data,
    loop,
    autoplay: false,
    useFrameInterpolation: true,
    renderConfig: { autoResize: false, freezeOnOffscreen: true, devicePixelRatio: Math.min(window.devicePixelRatio || 1, 2) },
  });
  live.add(p);
  return p;
}

export function destroyPlayer(p: Player) {
  stop(p);
  live.delete(p);
  p.destroy();
}

// ---- concurrency cap: the oldest-started animation is paused when a 13th one starts
const playing: Player[] = [];

/** Plays `p`, pausing the oldest running animation if MAX_PLAYING are already running. */
export function play(p: Player) {
  const i = playing.indexOf(p);
  if (i >= 0) playing.splice(i, 1);
  playing.push(p);
  while (playing.length > MAX_PLAYING) playing.shift()!.pause();
  if (!hidden) p.play();
}

export function stop(p: Player, toFirstFrame = false) {
  const i = playing.indexOf(p);
  if (i >= 0) playing.splice(i, 1);
  if (toFirstFrame) p.stop();
  else p.pause();
}

let hidden = false;
/** Pause every animation (panel hidden) / resume. Hiding also drops the JSON cache. */
export function setLottieActive(active: boolean) {
  hidden = !active;
  if (active) {
    for (const p of playing) p.play();
  } else {
    for (const p of live) p.pause();
    dataCache.clear();
  }
}

/** Reads a Telegram .tgs (gzip-compressed Lottie) or a plain Lottie .json file. */
export async function readStickerFile(file: File): Promise<string> {
  const buf = new Uint8Array(await file.arrayBuffer());
  const text = buf[0] === 0x1f && buf[1] === 0x8b ? strFromU8(gunzipSync(buf)) : strFromU8(buf);
  const json = JSON.parse(text);
  if (!json || !Array.isArray(json.layers)) throw new Error("不是有效的 Lottie 动画");
  return JSON.stringify(json);
}
