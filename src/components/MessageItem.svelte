<script lang="ts">
  import { api, formatSize, type Message } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import { soloEmoji } from "../lib/emoji";
  import { emojiByChar } from "../lib/lottie";
  import Icon from "./Icon.svelte";
  import Sticker from "./Sticker.svelte";

  let { m, first }: { m: Message; first: boolean } = $props();

  let solo = $derived.by(() => {
    if (m.kind !== "text") return null;
    const s = soloEmoji(m.body?.text ?? "");
    if (!s) return null;
    return { char: s.char, code: emojiByChar(s.char)?.code ?? s.code };
  });
  let prog = $derived(app.progress[m.id]);
  let pct = $derived(prog && prog.total > 0 ? prog.done / prog.total : m.status === "done" ? 1 : 0);

  function fileIcon(name: string): string {
    const ext = name.split(".").pop()?.toLowerCase() ?? "";
    if (["png", "jpg", "jpeg", "gif", "webp", "heic", "bmp", "svg", "avif"].includes(ext)) return "image";
    if (["mp4", "mov", "mkv", "avi", "webm"].includes(ext)) return "film";
    if (["mp3", "flac", "wav", "m4a", "ogg", "aac"].includes(ext)) return "music";
    if (["zip", "7z", "rar", "tar", "gz", "xz", "dmg", "iso"].includes(ext)) return "archive";
    return "doc";
  }

  function act(f: (id: string) => Promise<void>) {
    f(m.id).catch((e) => app.flash(String(e)));
  }

  function statusLabel(): string {
    switch (m.status) {
      case "transferring":
        return m.outgoing ? "发送中" : "接收中";
      case "receiving":
        return "接收中";
      case "done":
        return m.outgoing ? "已送达" : "已保存";
      case "paused":
        return "已暂停";
      case "cancelled":
        return m.body?.error ? `已取消：${m.body.error}` : "已取消";
      case "failed":
        return m.body?.error ? `失败：${m.body.error}` : "失败";
      default:
        return "";
    }
  }
</script>

<div class="row" class:out={m.outgoing} class:first>
  {#if m.kind === "emoji"}
    <div class="sticker-wrap" title={m.body?.alt ?? ""}>
      <Sticker emoji={m.body} size={160} />
    </div>
  {:else if solo}
    <div class="sticker-wrap" title={solo.char}>
      <Sticker emoji={{ kind: "noto", code: solo.code, alt: solo.char }} size={160} />
    </div>
  {:else if m.kind === "file"}
    <div class="file" class:failed={m.status === "failed"} class:cancelled={m.status === "cancelled"}>
      <div class="ficon"><Icon name={fileIcon(m.body?.name ?? "")} size={22} stroke={1.5} /></div>
      <div class="finfo">
        <div class="fname selectable" title={m.body?.name}>{m.body?.name}</div>
        <div class="fmeta">
          {formatSize(m.body?.size ?? 0)}
          {#if m.status === "transferring" || m.status === "receiving" || (m.status === "paused" && prog)}
            · {Math.floor(pct * 100)}%
          {/if}
          · <span class="st">{statusLabel()}</span>
        </div>
        {#if m.status === "transferring" || m.status === "receiving" || (m.status === "paused" && prog)}
          <div class="bar" class:paused={m.status === "paused"}><div class="fill" style:transform="scaleX({pct})"></div></div>
        {/if}
      </div>
      {#if m.outgoing && m.status === "transferring"}
        <button class="icon-btn" title="暂停" onclick={() => act(api.pauseTransfer)}><Icon name="pause" size={15} /></button>
      {:else if m.outgoing && m.status === "paused"}
        <button class="icon-btn" title="继续（断点续传）" onclick={() => act(api.retry)}><Icon name="play" size={14} /></button>
      {:else if m.outgoing && m.status === "failed"}
        <button class="icon-btn" title="重试（断点续传）" onclick={() => act(api.retry)}><Icon name="retry" size={15} /></button>
      {/if}
      {#if (m.outgoing && ["transferring", "paused", "failed"].includes(m.status)) || (!m.outgoing && ["receiving", "failed"].includes(m.status))}
        <button class="icon-btn" title="取消传输" onclick={() => act(api.cancelTransfer)}><Icon name="x" size={15} /></button>
      {:else if m.status === "done" && m.body?.path}
        <button class="icon-btn" title="在文件夹中显示" onclick={() => api.reveal(m.body.path).catch((e) => app.flash(String(e)))}>
          <Icon name="reveal" size={15} />
        </button>
      {/if}
    </div>
  {:else}
    <div class="bubble selectable">{m.body?.text}</div>
  {/if}
  {#if m.outgoing && m.kind !== "file"}
    <span class="state" title={m.status}>
      {#if m.status === "pending"}<Icon name="clock" size={11} />
      {:else if m.status === "sent"}<Icon name="check" size={11} />
      {:else if m.status === "delivered"}<Icon name="checks" size={12} />
      {:else if m.status === "failed"}<Icon name="alert" size={11} />{/if}
    </span>
  {/if}
</div>

<style>
  .row {
    display: flex;
    align-items: flex-end;
    gap: 4px;
    margin-top: 2px;
    animation: msg-in 320ms cubic-bezier(0.2, 0.9, 0.3, 1.15);
  }
  .row.first {
    margin-top: 8px;
  }
  .row.out {
    flex-direction: row-reverse;
  }
  @keyframes msg-in {
    from {
      opacity: 0;
      transform: translateY(8px) scale(0.97);
    }
  }
  .bubble {
    max-width: 78%;
    padding: 6px 10px;
    border-radius: 16px;
    background: var(--bubble-in);
    line-height: 1.38;
    font-size: 13px;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .out .bubble {
    background: var(--bubble-out);
    color: var(--accent-contrast);
  }
  .first .bubble {
    border-bottom-left-radius: 5px;
  }
  .out.first .bubble {
    border-bottom-left-radius: 16px;
    border-bottom-right-radius: 5px;
  }
  .state {
    color: var(--text-3);
    display: inline-grid;
    margin-bottom: 2px;
  }
  .sticker-wrap {
    width: 160px;
    height: 160px;
  }
  .file {
    width: 210px;
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 9px 8px 9px 10px;
    border-radius: 14px;
    background: var(--bubble-in);
    border: 0.5px solid var(--sep);
  }
  .out .file {
    background: color-mix(in srgb, var(--accent) 14%, transparent);
  }
  .file.failed {
    border-color: color-mix(in srgb, var(--danger) 50%, transparent);
  }
  .file.cancelled {
    opacity: 0.6;
  }
  .bar.paused .fill {
    background: var(--text-2, #8e8e93);
  }
  .ficon {
    width: 34px;
    height: 34px;
    border-radius: 8px;
    display: grid;
    place-items: center;
    flex: none;
    color: var(--accent);
    background: var(--field);
  }
  .finfo {
    flex: 1;
    min-width: 0;
  }
  .fname {
    font-size: 12.5px;
    font-weight: 500;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .fmeta {
    font-size: 10.5px;
    color: var(--text-2);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .failed .st {
    color: var(--danger);
  }
  .bar {
    margin-top: 5px;
    height: 4px;
    border-radius: 2px;
    background: var(--sep);
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--accent);
    transform-origin: left center;
    transition: transform 200ms linear;
  }
</style>
