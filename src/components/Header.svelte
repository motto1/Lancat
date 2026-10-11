<script lang="ts">
  import { api, osGlyph } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import { ui } from "../lib/ui.svelte";
  import Icon from "./Icon.svelte";

  // "typing…" expires at a known time: re-render exactly then instead of ticking every second
  // for as long as the panel lives.
  let now = $state(Date.now());
  $effect(() => {
    const p = app.currentPeer;
    const until = p ? (app.typing[p.id] ?? 0) : 0;
    const left = until - Date.now();
    if (left <= 0) return;
    now = Date.now();
    const t = setTimeout(() => (now = Date.now()), left + 20);
    return () => clearTimeout(t);
  });

  let title = $derived.by(() => {
    if (app.broadcast) return "全部设备";
    if (app.selected.length > 1) return `已选 ${app.selected.length} 台设备`;
    return app.currentPeer?.name ?? "Lancat";
  });
  let subtitle = $derived.by(() => {
    if (app.broadcast) return `群发给 ${app.onlinePeers.length} 台在线设备`;
    if (app.selected.length > 1)
      return app.selected.map((id) => app.peers.find((p) => p.id === id)?.name ?? "?").join("、");
    const p = app.currentPeer;
    if (!p) return app.me ? `本机：${app.me.name}` : "";
    if ((app.typing[p.id] ?? 0) > now) return "正在输入…";
    if (p.blocked) return "已拉黑";
    return p.online ? `在线 · ${osGlyph(p.os)} · ${p.addr}` : "离线";
  });

  // a probe burst lasts ~3 s (0 s / 1 s / 3 s); spin the icon meanwhile
  let refreshing = $state(false);
  function refresh() {
    if (refreshing) return;
    refreshing = true;
    api.refreshPeers().catch(() => {});
    setTimeout(() => (refreshing = false), 3200);
  }

  // press on the header (not a button) and drag: moves the dock to any edge / position
  function onPress(e: PointerEvent) {
    if (e.button !== 0 || (e.target as HTMLElement).closest("button")) return;
    e.preventDefault();
    api.dockMoveStart();
  }

  function togglePin() {
    ui.pinned = !ui.pinned;
    api.setPinned(ui.pinned);
  }
</script>

<header onpointerdown={onPress} title="拖动可移动到屏幕任意边缘">
  <div class="titles">
    <div class="title">{title}</div>
    <div class="sub" class:typing={subtitle === "正在输入…"}>{subtitle}</div>
  </div>
  <button class="icon-btn" class:spin={refreshing} title="刷新设备（重新探测局域网和虚拟局域网）" onclick={refresh}>
    <Icon name="retry" size={15} />
  </button>
  <button class="icon-btn" class:on={ui.pinned} title={ui.pinned ? "取消钉住" : "钉住（不自动收起）"} onclick={togglePin}>
    <Icon name="pin" size={15} />
  </button>
  <button class="icon-btn" title="设置" onclick={() => api.openSettings()}>
    <Icon name="gear" size={15} />
  </button>
</header>

<style>
  header {
    height: 46px;
    flex: none;
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 0 10px 0 14px;
    border-bottom: 0.5px solid var(--sep);
  }
  .titles {
    flex: 1;
    min-width: 0;
  }
  .title {
    font-size: 13px;
    font-weight: 600;
    letter-spacing: -0.01em;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .sub {
    font-size: 11px;
    color: var(--text-2);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .sub.typing {
    color: var(--accent);
  }
  .icon-btn.spin {
    color: var(--accent);
  }
  .icon-btn.spin :global(svg) {
    animation: spin 1s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
