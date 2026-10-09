// Emoji helpers (the animated library is added in the emoji milestone).

/** Codepoint key used by Noto Animated Emoji, e.g. "1f600" or "1f44d_1f3fb". */
export function codeOf(char: string): string {
  return [...char].map((c) => c.codePointAt(0)!.toString(16)).filter((h) => h !== "fe0f").join("_");
}

const pictographic = /\p{Extended_Pictographic}/u;

/** If `text` is exactly one emoji grapheme, returns it. */
export function soloEmoji(text: string): { char: string; code: string } | null {
  const t = text.trim();
  if (!t || t.length > 16 || !pictographic.test(t)) return null;
  const seg = new Intl.Segmenter(undefined, { granularity: "grapheme" });
  const parts = [...seg.segment(t)];
  if (parts.length !== 1) return null;
  return { char: t, code: codeOf(t) };
}
