<script lang="ts">
  import { api, osGlyph } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import { ui } from "../lib/ui.svelte";
  import Icon from "./Icon.svelte";

  let now = $state(Date.now());
  $effect(() => {
    const t = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(t);
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

  function togglePin() {
    ui.pinned = !ui.pinned;
    api.setPinned(ui.pinned);
  }
</script>

<header data-tauri-drag-region>
  <div class="titles" data-tauri-drag-region>
    <div class="title">{title}</div>
    <div class="sub" class:typing={subtitle === "正在输入…"}>{subtitle}</div>
  </div>
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
</style>
