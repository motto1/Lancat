<script lang="ts">
  // Animated sticker (ThorVG canvas). mode "chat": plays (2 loops) when scrolled into view,
  // replays on hover. mode "grid": shows the first frame, plays while hovered.
  // Never animates off-screen; at most MAX_PLAYING stickers animate at once (lib/lottie.ts).
  import { onMount } from "svelte";
  import type { EmojiPayload } from "../lib/api";
  import { createPlayer, destroyPlayer, getPoster, lottieData, play, PLAYER_DPR, posterKey, posterLoads, savePoster, stop, warmPlayer, type Player } from "../lib/lottie";

  // underlay=false: the caller keeps its own placeholder (e.g. the grid glyph) under the sticker and
  // swaps it out on `onshown`, so the sticker neither draws a fallback glyph of its own (a different
  // size from the caller's) nor shows an empty canvas while the poster decodes; no fade either.
  let {
    emoji,
    size = 160,
    mode = "chat",
    underlay = true,
    onshown,
  }: { emoji: EmojiPayload; size?: number; mode?: "chat" | "grid"; underlay?: boolean; onshown?: () => void } = $props();

  const FADE_MS = 160;
  let host: HTMLDivElement;
  let canvas: HTMLCanvasElement;
  let failed = $state(false);
  let ready = $state(false);
  // Until ThorVG has drawn frame 0 the canvas shows a placeholder: the cached poster of that very
  // frame, painted into the canvas itself (decoded up front, so it is in the first visible frame),
  // which the player then overwrites with identical pixels; or, for an emoji never seen before,
  // the system glyph underneath, crossfaded.
  const pkey = $derived(posterKey(emoji, size));
  const poster = $derived(getPoster(pkey));
  const pr = PLAYER_DPR; // must match createPlayer's devicePixelRatio
  let posterDrawn = $state(false);
  let posterFailed = $state(false);
  let placeholder = $state(true);
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
    warmPlayer(); // load the player JS while the JSON is fetched
    try {
      const data = await p;
      if (!canvas || destroyed) return;
      const a = await createPlayer(canvas, data, true);
      if (destroyed) {
        destroyPlayer(a);
        return;
      }
      anim = a;
      // `load` is dispatched after the player has rendered and drawn frame 0 into the canvas.
      const onReady = () => {
        if (ready) return;
        if (!poster) savePoster(pkey, size, canvas);
        ready = true;
        if (posterDrawn) placeholder = false;
        else if (!underlay) {
          placeholder = false;
          onshown?.();
        } else setTimeout(() => (placeholder = false), FADE_MS + 40);
        sync();
      };
      a.addEventListener("load", onReady);
      a.addEventListener("loadError", () => (failed = true));
      if (a.isLoaded) onReady();
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
    if (poster) {
      // createImageBitmap, not <img>/img.decode(): those decode through the compositor, which a
      // parked (not yet shown) panel does not run, so the poster arrived one frame after the page.
      const load = fetch(poster)
        .then((r) => r.blob())
        .then((b) => createImageBitmap(b))
        .then((bmp) => {
          if (!ready && !destroyed && canvas) {
            canvas.getContext("2d")?.drawImage(bmp, 0, 0, canvas.width, canvas.height);
            posterDrawn = true;
            onshown?.();
          }
          bmp.close();
        })
        .catch(() => (posterFailed = true));
      posterLoads.add(load);
      void load.finally(() => posterLoads.delete(load));
    }
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
  <canvas
    class="host"
    class:shown={ready || posterDrawn}
    class:instant={posterDrawn || !underlay}
    bind:this={canvas}
    width={Math.round(size * pr)}
    height={Math.round(size * pr)}
    style:width="{size}px"
    style:height="{size}px"
  ></canvas>
  {#if underlay && (failed || ((!poster || posterFailed) && placeholder))}
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
  .host.instant {
    transition: none;
  }
  .host.shown {
    opacity: 1;
  }
  /* The in-flow glyph paints under the positioned canvas and stays fully opaque while the canvas
     fades in over it (removed afterwards), so coverage never dips. */
  .fallback {
    line-height: 1;
    font-family: "Noto Color Emoji", "Apple Color Emoji", "Segoe UI Emoji", sans-serif;
  }
</style>
