<script lang="ts">
  // Animated sticker (ThorVG canvas). mode "chat": plays (2 loops) when scrolled into view,
  // replays on hover. mode "grid": shows the first frame, plays while hovered.
  // Never animates off-screen; at most MAX_PLAYING stickers animate at once (lib/lottie.ts).
  import { onMount } from "svelte";
  import type { EmojiPayload } from "../lib/api";
  import { createPlayer, destroyPlayer, lottieData, play, stop, type Player } from "../lib/lottie";

  let { emoji, size = 160, mode = "chat" }: { emoji: EmojiPayload; size?: number; mode?: "chat" | "grid" } = $props();

  let host: HTMLDivElement;
  let canvas: HTMLCanvasElement;
  let failed = $state(false);
  let ready = $state(false);
  let anim: Player | null = null;
  let visible = false;
  let hovering = false;
  let loops = 0;
  let destroyed = false;
  let loading = false;

  function sync() {
    if (!anim || !ready) return;
    if (!visible) {
      stop(anim);
      return;
    }
    if (mode === "grid") {
      if (hovering) play(anim);
      else stop(anim, true);
    } else if (hovering || loops < 2) play(anim);
  }

  async function ensure() {
    if (anim || failed || loading) return;
    const p = lottieData(emoji);
    if (!p) {
      failed = true;
      return;
    }
    loading = true;
    try {
      const data = await p;
      if (!canvas || destroyed) return;
      const a = await createPlayer(canvas, data, true);
      if (destroyed) {
        destroyPlayer(a);
        return;
      }
      anim = a;
      a.addEventListener("load", () => {
        ready = true;
        sync();
      });
      a.addEventListener("loadError", () => (failed = true));
      if (a.isLoaded) {
        ready = true;
        sync();
      }
      a.addEventListener("loop", () => {
        loops++;
        if (mode === "chat" && loops >= 2 && !hovering && anim) stop(anim, true);
      });
    } catch {
      failed = true;
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    const io = new IntersectionObserver(
      (entries) => {
        visible = entries.some((e) => e.isIntersecting);
        if (visible) ensure();
        sync();
      },
      { rootMargin: "40px" },
    );
    io.observe(host);
    return () => {
      destroyed = true;
      io.disconnect();
      if (anim) destroyPlayer(anim);
      anim = null;
    };
  });
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="sticker"
  class:ready
  bind:this={host}
  style:width="{size}px"
  style:height="{size}px"
  onmouseenter={() => {
    hovering = true;
    loops = 0;
    sync();
  }}
  onmouseleave={() => {
    hovering = false;
    sync();
  }}
>
  <canvas class="host" bind:this={canvas} width={size} height={size} style:width="{size}px" style:height="{size}px"></canvas>
  {#if failed || !ready}
    <div class="fallback" style:font-size="{Math.round(size * 0.68)}px">{emoji.alt || "🙂"}</div>
  {/if}
</div>

<style>
  .sticker {
    position: relative;
    display: grid;
    place-items: center;
  }
  .host {
    position: absolute;
    inset: 0;
    opacity: 0;
    transition: opacity 160ms ease;
  }
  .ready .host {
    opacity: 1;
  }
  .fallback {
    line-height: 1;
    font-family: "Noto Color Emoji", "Apple Color Emoji", "Segoe UI Emoji", sans-serif;
  }
</style>
