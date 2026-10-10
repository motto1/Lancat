<script lang="ts">
  // Second-level glass popover: categories, recent, search, imported .tgs stickers.
  import { onMount, tick } from "svelte";
  import { api, type EmojiPayload, type StickerMeta } from "../lib/api";
  import { CATEGORIES, EMOJI, type EmojiEntry } from "../lib/emoji-catalog";
  import { emojiByCode, readStickerFile } from "../lib/lottie";
  import { app } from "../lib/state.svelte";
  import Icon from "./Icon.svelte";
  import Sticker from "./Sticker.svelte";

  let { onpick, onclose }: { onpick: (e: EmojiPayload) => void; onclose: () => void } = $props();

  let query = $state("");
  let active = $state("recent");
  let hovered = $state<string | null>(null);
  // The glyph stays in place under the hovered cell's sticker until the sticker has pixels in its
  // canvas (poster or frame 0); swapping straight away showed a smaller fallback glyph or an empty
  // cell for a frame or two.
  let covered = $state<string | null>(null);
  let stickers = $state<StickerMeta[]>([]);
  let scroller: HTMLDivElement;
  let fileInput: HTMLInputElement;
  let root: HTMLDivElement;
  let importing = $state(false);

  let recent = $derived((app.settings?.recent_emoji ?? []).slice(0, 21));
  let results = $derived.by(() => {
    const q = query.trim().toLowerCase();
    if (!q) return null;
    return EMOJI.filter((e) => e.kw.toLowerCase().includes(q) || e.char === q);
  });

  onMount(() => {
    api.stickers().then((s) => (stickers = s)).catch(() => {});
    const close = (ev: MouseEvent) => {
      if (!root.contains(ev.target as Node)) onclose();
    };
    const key = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") {
        ev.stopPropagation();
        onclose();
      }
    };
    setTimeout(() => window.addEventListener("click", close), 0);
    window.addEventListener("keydown", key, true);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("keydown", key, true);
    };
  });

  function remember(key: string) {
    const s = app.settings;
    if (!s) return;
    const next = [key, ...s.recent_emoji.filter((k) => k !== key)].slice(0, 24);
    app.settings = { ...s, recent_emoji: next };
    api.setSettings(app.settings).catch(() => {});
  }

  function pickEmoji(e: EmojiEntry) {
    remember(e.code);
    onpick({ kind: "noto", code: e.code, alt: e.char });
  }

  async function pickSticker(s: StickerMeta) {
    try {
      const data = await api.sticker(s.id);
      remember(`s:${s.id}`);
      onpick({ kind: "sticker", code: s.id, alt: s.name, data });
    } catch (e) {
      app.flash(String(e));
    }
  }

  async function jump(id: string) {
    active = id;
    query = "";
    await tick();
    const el = scroller.querySelector<HTMLElement>(`[data-cat="${id}"]`);
    if (el) scroller.scrollTo({ top: el.offsetTop - 4, behavior: "smooth" });
  }

  function onScroll() {
    if (results) return;
    const secs = [...scroller.querySelectorAll<HTMLElement>("[data-cat]")];
    let cur = secs[0]?.dataset.cat ?? "recent";
    for (const s of secs) if (s.offsetTop - scroller.scrollTop <= 24) cur = s.dataset.cat!;
    active = cur;
  }

  async function importFiles(files: FileList | null) {
    if (!files?.length) return;
    importing = true;
    let n = 0;
    for (const f of files) {
      try {
        const json = await readStickerFile(f);
        await api.importSticker(f.name.replace(/\.(tgs|json)$/i, ""), json);
        n++;
      } catch (e) {
        app.flash(`${f.name}: ${e instanceof Error ? e.message : e}`);
      }
    }
    importing = false;
    fileInput.value = "";
    stickers = await api.stickers();
    if (n) app.flash(`已导入 ${n} 个贴纸`);
    jump("stickers");
  }

  async function removeSticker(s: StickerMeta, ev: MouseEvent) {
    ev.preventDefault();
    await api.deleteSticker(s.id);
    stickers = stickers.filter((x) => x.id !== s.id);
  }
</script>

{#snippet cell(e: EmojiEntry)}
  <button
    class="cell"
    title={e.kw.split(" ")[0]}
    onclick={() => pickEmoji(e)}
    onmouseenter={() => (hovered = e.code)}
    onmouseleave={() => {
      if (hovered === e.code) hovered = null;
      if (covered === e.code) covered = null;
    }}
  >
    <span class="glyph" class:gone={covered === e.code && hovered === e.code}>{e.char}</span>
    {#if hovered === e.code}
      <span class="over">
        <Sticker emoji={{ kind: "noto", code: e.code, alt: e.char }} size={30} mode="chat" underlay={false} onshown={() => hovered === e.code && (covered = e.code)} />
      </span>
    {/if}
  </button>
{/snippet}

{#snippet stickerCell(s: StickerMeta)}
  <button class="cell sticker-cell" title={`${s.name}（右键删除）`} onclick={() => pickSticker(s)} oncontextmenu={(ev) => removeSticker(s, ev)}>
    <Sticker emoji={{ kind: "sticker", code: s.id, alt: "🖼️" }} size={34} mode="grid" />
  </button>
{/snippet}

<div class="emoji-panel glass" bind:this={root} role="dialog" aria-label="表情">
  <div class="search">
    <Icon name="search" size={13} />
    <!-- svelte-ignore a11y_autofocus -->
    <input bind:value={query} placeholder="搜索表情（中文 / English）" autofocus />
  </div>

  <div class="grid-scroll" bind:this={scroller} onscroll={onScroll}>
    {#if results}
      <div class="grid">
        {#each results as e (e.code)}{@render cell(e)}{/each}
      </div>
      {#if results.length === 0}<div class="none">没有找到「{query}」</div>{/if}
    {:else}
      {#each CATEGORIES as c (c.id)}
        {#if c.id === "recent"}
          {#if recent.length}
            <div class="sec" data-cat="recent">最近使用</div>
            <div class="grid">
              {#each recent as k (k)}
                {#if k.startsWith("s:")}
                  {@const s = stickers.find((x) => x.id === k.slice(2))}
                  {#if s}{@render stickerCell(s)}{/if}
                {:else}
                  {@const e = emojiByCode(k)}
                  {#if e}{@render cell(e)}{/if}
                {/if}
              {/each}
            </div>
          {/if}
        {:else if c.id === "stickers"}
          <div class="sec" data-cat="stickers">
            贴纸
            <button class="import" onclick={() => fileInput.click()} disabled={importing}>
              <Icon name="upload" size={12} />{importing ? "导入中…" : "导入 .tgs"}
            </button>
          </div>
          {#if stickers.length}
            <div class="grid">
              {#each stickers as s (s.id)}{@render stickerCell(s)}{/each}
            </div>
          {:else}
            <div class="none">导入 Telegram 的 .tgs 动态贴纸（或 Lottie .json），即可在这里发送。</div>
          {/if}
        {:else}
          <div class="sec" data-cat={c.id}>{c.label}</div>
          <div class="grid">
            {#each EMOJI.filter((e) => e.cat === c.id) as e (e.code)}{@render cell(e)}{/each}
          </div>
        {/if}
      {/each}
    {/if}
  </div>

  <nav class="tabs">
    {#each CATEGORIES as c (c.id)}
      <button class="tab" class:on={active === c.id && !results} title={c.label} onclick={() => jump(c.id)}>
        <Icon name={c.icon} size={15} />
      </button>
    {/each}
  </nav>
  <input bind:this={fileInput} type="file" accept=".tgs,.json,application/json,application/x-tgsticker" multiple hidden onchange={(e) => importFiles(e.currentTarget.files)} />
</div>

<style>
  .emoji-panel {
    position: absolute;
    left: 6px;
    right: 6px;
    bottom: calc(100% + 6px);
    height: 288px;
    z-index: 30;
    border-radius: 14px;
    display: flex;
    flex-direction: column;
    background: var(--glass-strong);
    transform-origin: 12% 100%;
    animation: pop 300ms cubic-bezier(0.2, 0.95, 0.3, 1.18);
  }
  @keyframes pop {
    from {
      opacity: 0;
      transform: translateY(10px) scale(0.92);
    }
  }
  .search {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 8px 8px 4px;
    padding: 4px 9px;
    border-radius: 8px;
    background: var(--hover);
    color: var(--text-3);
  }
  .search input {
    flex: 1;
    border: none;
    outline: none;
    background: transparent;
    font-size: 12px;
    color: var(--text);
  }
  .grid-scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 0 6px 6px;
    position: relative;
  }
  .sec {
    display: flex;
    align-items: center;
    justify-content: space-between;
    font-size: 10.5px;
    font-weight: 600;
    color: var(--text-3);
    padding: 8px 4px 4px;
    letter-spacing: 0.02em;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(36px, 1fr));
  }
  .cell {
    position: relative;
    height: 38px;
    display: grid;
    place-items: center;
    border-radius: 9px;
    /* The hover zoom animates a registered number (global.css) instead of `transform`: a transform
       transition moves the cell into its own compositing layer for its duration, and at a
       fractional DPR (1.5) the layer and the page paint the glyph on different device-pixel grids,
       so it jumped ~1 px when the zoom started and again when it ended. Too many cells to keep them
       all in layers; a custom-property transition runs on the main thread and never promotes.
       The rest value is 1.0001, not 1: an identity transform snaps the box to device pixels while
       any other scale does not, which would put the same jump at the first and last frame. */
    transform: scale(var(--cell-scale));
    transition:
      --cell-scale var(--dur) var(--ease-spring),
      background-color var(--dur-fast) var(--ease);
  }
  .cell:hover {
    background: var(--hover);
    --cell-scale: 1.12;
  }
  .cell:active {
    --cell-scale: 0.92;
  }
  .over {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    pointer-events: none;
  }
  .glyph.gone {
    visibility: hidden;
  }
  .glyph {
    font-size: 25px;
    line-height: 1;
    font-family: "Noto Color Emoji", "Apple Color Emoji", "Segoe UI Emoji", sans-serif;
  }
  .sticker-cell {
    height: 44px;
  }
  .none {
    padding: 12px 6px;
    color: var(--text-3);
    font-size: 11.5px;
    line-height: 1.5;
  }
  .import {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 10.5px;
    font-weight: 500;
    color: var(--accent);
    padding: 2px 6px;
    border-radius: 5px;
    transition: background-color var(--dur-fast) var(--ease);
  }
  .import:hover {
    background: var(--hover);
  }
  .tabs {
    display: flex;
    justify-content: space-around;
    padding: 4px 6px 6px;
    border-top: 0.5px solid var(--sep);
  }
  .tab {
    width: 30px;
    height: 26px;
    border-radius: 7px;
    display: grid;
    place-items: center;
    color: var(--text-3);
    transition:
      color var(--dur-fast) var(--ease),
      background-color var(--dur-fast) var(--ease),
      transform var(--dur) var(--ease);
  }
  .tab:hover {
    background: var(--hover);
  }
  .tab.on {
    color: var(--accent);
    transform: scale(1.06);
  }
</style>
