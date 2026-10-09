// Lottie playback for the bundled Noto Animated Emoji and imported .tgs stickers.
// lottie-web "light" (SVG renderer only) keeps the bundle small. Animations play only while
// visible (IntersectionObserver in Sticker.svelte) and everything is frozen while the panel is hidden.
import lottie from "lottie-web/build/player/lottie_light";
import type { AnimationItem } from "lottie-web";
import { gunzipSync, strFromU8 } from "fflate";
import { EMOJI, type EmojiEntry } from "./emoji-catalog";
import { api, type EmojiPayload } from "./api";

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

const dataCache = new Map<string, Promise<unknown>>();

/** Resolves Lottie JSON for a payload (bundled Noto, inline sticker data, or a locally imported sticker). */
export function lottieData(e: EmojiPayload): Promise<unknown> | null {
  if (e.kind === "noto") {
    const url = notoUrl(e.code);
    if (!url) return null;
    const key = `noto:${e.code}`;
    if (!dataCache.has(key)) dataCache.set(key, fetch(url).then((r) => r.json()));
    return dataCache.get(key)!;
  }
  if (e.kind === "sticker") {
    const key = `sticker:${e.code}`;
    if (!dataCache.has(key)) {
      dataCache.set(key, e.data ? Promise.resolve(JSON.parse(e.data)) : api.sticker(e.code).then((s) => JSON.parse(s)));
    }
    return dataCache.get(key)!;
  }
  return null;
}

export function createPlayer(container: Element, data: unknown, loop: boolean | number = true): AnimationItem {
  return lottie.loadAnimation({
    container,
    renderer: "svg",
    loop,
    autoplay: false,
    animationData: data,
    rendererSettings: { progressiveLoad: true, hideOnTransparent: true, preserveAspectRatio: "xMidYMid meet" },
  });
}

/** Pause every animation (panel hidden) / resume. */
export function setLottieActive(active: boolean) {
  const l = lottie as unknown as { freeze?: () => void; unfreeze?: () => void };
  if (active) l.unfreeze?.();
  else l.freeze?.();
}

/** Reads a Telegram .tgs (gzip-compressed Lottie) or a plain Lottie .json file. */
export async function readStickerFile(file: File): Promise<string> {
  const buf = new Uint8Array(await file.arrayBuffer());
  const text = buf[0] === 0x1f && buf[1] === 0x8b ? strFromU8(gunzipSync(buf)) : strFromU8(buf);
  const json = JSON.parse(text);
  if (!json || !Array.isArray(json.layers)) throw new Error("不是有效的 Lottie 动画");
  return JSON.stringify(json);
}
