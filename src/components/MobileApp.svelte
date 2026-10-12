<script lang="ts">
  // Android: one full-screen window. Device list -> conversation (system back returns), share
  // sheet -> pick a device. Reuses the desktop conversation, bubbles and composer.
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { api, avatarColor, initials, osGlyph, formatTime, type Peer } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import Conversation from "./Conversation.svelte";
  import Composer from "./Composer.svelte";

  let view = $state<"list" | "chat">("list");
  /** Files shared to us from another app, waiting for a device to be chosen. */
  let shared = $state<string[]>([]);
  let refreshing = $state(false);

  const cur = $derived(app.peers.find((p) => p.id === app.current) ?? null);

  async function openChat(p: Peer) {
    if (shared.length) {
      const files = shared;
      shared = [];
      try {
        await api.sendFiles([p.id], files);
      } catch (e) {
        app.flash(String(e));
      }
    }
    await app.open(p.id);
    view = "chat";
    app.visible = true;
    app.markReadIfVisible();
    history.pushState({ chat: p.id }, "");
  }

  function back() {
    if (view === "chat") history.back();
  }

  async function refresh() {
    refreshing = true;
    await api.refreshPeers().catch(() => {});
    setTimeout(() => (refreshing = false), 3000);
  }

  /** Wi-Fi with client isolation and no tailnet list on Android: add a device by its address. */
  async function addByAddr() {
    const addr = prompt("输入对方的 IP（例如 Tailscale 的 100.x.x.x）");
    if (!addr?.trim()) return;
    try {
      const p = await api.addPeer(addr.trim());
      app.flash(`已连接 ${p.name}`);
    } catch (e) {
      app.flash(`连不上：${e}`);
    }
  }

  async function checkShared() {
    try {
      const got = await api.takeShared();
      if (got.length) {
        shared = got;
        if (view === "chat") history.back();
      }
    } catch {
      /* not on Android */
    }
  }

  function status(p: Peer): string {
    if (p.blocked) return "已拉黑";
    if (!p.online) return "离线";
    return p.via ? `经 ${p.via} 中转` : `在线 · ${osGlyph(p.os)}`;
  }

  onMount(() => {
    app.visible = true;
    // init opens the first conversation; on a phone the list comes first
    app.init().then(() => {
      view = "list";
    });
    const onPop = () => {
      view = "list";
    };
    window.addEventListener("popstate", onPop);
    const onVis = () => {
      app.visible = document.visibilityState === "visible" && view === "chat";
      if (document.visibilityState === "visible") {
        void checkShared();
        app.markReadIfVisible();
      }
    };
    document.addEventListener("visibilitychange", onVis);
    void checkShared();
    const un = listen("lancat://shared", () => void checkShared());
    return () => {
      window.removeEventListener("popstate", onPop);
      document.removeEventListener("visibilitychange", onVis);
      un.then((f) => f());
    };
  });

  $effect(() => {
    app.visible = view === "chat";
  });
</script>

<div class="m">
  {#if view === "list"}
    <header class="top">
      <div class="title">
        <h1>Lancat</h1>
        {#if app.me}<span class="sub">本机：{app.me.name}</span>{/if}
      </div>
      <button class="icon" onclick={addByAddr} aria-label="按地址添加">＋</button>
      <button class="icon" class:spin={refreshing} onclick={refresh} aria-label="刷新">⟳</button>
    </header>
    {#if shared.length}
      <div class="share">发送 {shared.length} 个文件给… <button onclick={() => (shared = [])}>取消</button></div>
    {/if}
    <ul class="list">
      {#each app.peers as p (p.id)}
        <li>
          <button class="row" class:off={!p.online} onclick={() => openChat(p)}>
            <span class="av" style:background={avatarColor(p.id)}>
              {initials(p.name)}
              {#if p.online}<i class="dot" class:via={!!p.via}></i>{/if}
            </span>
            <span class="mid">
              <span class="name">{p.name}</span>
              <span class="prev">{p.last_preview || status(p)}</span>
            </span>
            <span class="end">
              {#if p.last_ts}<span class="time">{formatTime(p.last_ts)}</span>{/if}
              {#if p.unread}<span class="badge">{p.unread > 99 ? "99+" : p.unread}</span>{/if}
            </span>
          </button>
        </li>
      {:else}
        <li class="empty">正在寻找同一网络里的 Lancat 设备…<br /><small>找不到的话点右上角 ＋，输入电脑的 IP</small></li>
      {/each}
    </ul>
  {:else}
    <header class="top chat">
      <button class="icon back" onclick={back} aria-label="返回">‹</button>
      {#if cur}
        <div class="title">
          <h2>{cur.name}</h2>
          <span class="sub">{status(cur)}</span>
        </div>
      {/if}
    </header>
    <section class="conv">
      <Conversation />
      <Composer />
    </section>
  {/if}
  {#if app.toast}<div class="toast">{app.toast}</div>{/if}
</div>

<style>
  .m {
    position: fixed;
    inset: 0;
    display: flex;
    flex-direction: column;
    background: var(--glass-strong);
    color: var(--text);
    font-family: var(--font);
    padding-top: env(safe-area-inset-top);
    padding-bottom: env(safe-area-inset-bottom);
  }
  .top {
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 14px 16px 10px;
    border-bottom: 0.5px solid var(--sep);
  }
  .top.chat {
    padding: 8px 12px 8px 4px;
  }
  .title {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  h1 {
    margin: 0;
    font-size: 24px;
    font-weight: 700;
  }
  h2 {
    margin: 0;
    font-size: 17px;
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .sub {
    font-size: 12px;
    color: var(--text-2);
  }
  .icon {
    border: 0;
    background: none;
    color: var(--accent);
    font-size: 24px;
    width: 44px;
    height: 44px;
    border-radius: 22px;
  }
  .icon.back {
    font-size: 34px;
    line-height: 1;
  }
  .icon:active {
    background: var(--hover);
  }
  .spin {
    animation: spin 1s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .share {
    margin: 8px 12px 0;
    padding: 10px 12px;
    border-radius: 12px;
    background: color-mix(in srgb, var(--accent) 14%, transparent);
    font-size: 14px;
    display: flex;
    justify-content: space-between;
  }
  .share button {
    border: 0;
    background: none;
    color: var(--accent);
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 4px 0;
    overflow-y: auto;
    flex: 1;
  }
  .row {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 16px;
    border: 0;
    background: none;
    color: inherit;
    text-align: left;
    font: inherit;
  }
  .row:active {
    background: var(--hover);
  }
  .row.off .av {
    opacity: 0.55;
  }
  .av {
    position: relative;
    flex: none;
    width: 48px;
    height: 48px;
    border-radius: 24px;
    display: grid;
    place-items: center;
    color: #fff;
    font-weight: 600;
    font-size: 17px;
  }
  .dot {
    position: absolute;
    right: 0;
    bottom: 0;
    width: 13px;
    height: 13px;
    border-radius: 7px;
    background: var(--online);
    border: 2px solid var(--glass-strong);
  }
  .dot.via {
    background: #ff9f0a;
  }
  .mid {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .name {
    font-size: 16px;
    font-weight: 600;
  }
  .prev {
    font-size: 13px;
    color: var(--text-2);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .end {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 4px;
  }
  .time {
    font-size: 11px;
    color: var(--text-3);
  }
  .badge {
    min-width: 20px;
    height: 20px;
    padding: 0 6px;
    border-radius: 10px;
    background: var(--danger);
    color: #fff;
    font-size: 12px;
    display: grid;
    place-items: center;
  }
  .empty {
    padding: 40px 24px;
    text-align: center;
    color: var(--text-2);
  }
  .conv {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .toast {
    position: fixed;
    left: 50%;
    bottom: calc(80px + env(safe-area-inset-bottom));
    transform: translateX(-50%);
    padding: 8px 14px;
    border-radius: 12px;
    background: rgba(0, 0, 0, 0.75);
    color: #fff;
    font-size: 13px;
  }
</style>
