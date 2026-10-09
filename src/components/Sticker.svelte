<script lang="ts">
  // Animated sticker. mode "chat": plays (2 loops) when scrolled into view, replays on hover.
  // mode "grid": shows the first frame, plays while hovered. Never animates off-screen.
  import { onMount } from "svelte";
  import type { AnimationItem } from "lottie-web";
  import type { EmojiPayload } from "../lib/api";
  import { createPlayer, lottieData } from "../lib/lottie";

  let { emoji, size = 160, mode = "chat" }: { emoji: EmojiPayload; size?: number; mode?: "chat" | "grid" } = $props();

  let host: HTMLDivElement;
  let failed = $state(false);
  let ready = $state(false);
  let anim: AnimationItem | null = null;
  let visible = false;
  let hovering = false;
  let loops = 0;

  function sync() {
    if (!anim) return;
    if (!visible) {
      anim.pause();
      return;
    }
    if (mode === "grid") {
      if (hovering) anim.play();
      else anim.goToAndStop(0, true);
    } else if (hovering || loops < 2) anim.play();
  }

  async function ensure() {
    if (anim || failed) return;
    const p = lottieData(emoji);
    if (!p) {
      failed = true;
      return;
    }
    try {
      const data = await p;
      if (!host) return;
      anim = createPlayer(host, data, true);
      anim.addEventListener("loopComplete", () => {
        loops++;
        if (mode === "chat" && loops >= 2 && !hovering) anim?.goToAndStop(0, true);
      });
      anim.addEventListener("DOMLoaded", () => (ready = true));
      sync();
    } catch {
      failed = true;
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
      io.disconnect();
      anim?.destroy();
      anim = null;
    };
  });
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="sticker"
  class:ready
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
  <div class="host" bind:this={host}></div>
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
