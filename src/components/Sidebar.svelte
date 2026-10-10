<script lang="ts">
  // Device avatar column: online dot, unread badge, drop targets, Shift multi-select, context menu.
  import { api, avatarColor, initials, type Peer } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import { ui } from "../lib/ui.svelte";
  import Icon from "./Icon.svelte";

  let menu = $state<{ peer: Peer; x: number; y: number } | null>(null);

  function click(e: MouseEvent, p: Peer) {
    if (e.shiftKey || e.metaKey || e.ctrlKey) {
      app.toggleSelect(p.id);
      return;
    }
    app.clearSelection();
    app.open(p.id);
  }

  function openMenu(e: MouseEvent, p: Peer) {
    e.preventDefault();
    menu = { peer: p, x: Math.min(e.clientX, 220), y: Math.min(e.clientY, 420) };
    ui.menuOpen = true;
  }
  function closeMenu() {
    menu = null;
    ui.menuOpen = false;
  }
  async function act(kind: "block" | "clear" | "forget") {
    const p = menu!.peer;
    closeMenu();
    if (kind === "block") await api.setBlocked(p.id, !p.blocked);
    if (kind === "clear") await api.clearHistory(p.id);
    if (kind === "forget") await api.forgetPeer(p.id);
  }
</script>

<svelte:window onclick={() => menu && closeMenu()} />

<nav class="sidebar">
  <button
    class="item all"
    class:active={app.broadcast}
    class:drop-hot={ui.dropKey === "all"}
    data-drop="all"
    title="全部在线设备（群发）"
    onclick={() => {
      app.clearSelection();
      app.broadcast = !app.broadcast;
    }}
  >
    <span class="avatar all-avatar"><Icon name="grid" size={17} /></span>
    <span class="label">全部</span>
  </button>
  <div class="sep"></div>
  <div class="list">
    {#each app.peers as p (p.id)}
      <button
        class="item"
        class:active={!app.broadcast && app.current === p.id && app.selected.length <= 1}
        class:selected={app.selected.includes(p.id)}
        class:offline={!p.online}
        class:blocked={p.blocked}
        class:drop-hot={ui.dropKey === `peer:${p.id}`}
        data-drop={`peer:${p.id}`}
        title={`${p.name}${p.online ? "" : "（离线）"}\nShift+点击可多选`}
        onclick={(e) => click(e, p)}
        oncontextmenu={(e) => openMenu(e, p)}
      >
        <span class="avatar" style:--c={avatarColor(p.id)}>
          {initials(p.name)}
          {#if p.online}<span class="dot"></span>{/if}
          {#if p.unread > 0 && !p.blocked}<span class="badge">{p.unread > 99 ? "99+" : p.unread}</span>{/if}
          {#if app.selected.includes(p.id)}<span class="tick"><Icon name="check" size={10} stroke={3} /></span>{/if}
        </span>
        <span class="label">{p.name}</span>
      </button>
    {/each}
  </div>
</nav>

{#if menu}
  <div class="menu glass" style:left="{menu.x}px" style:top="{menu.y}px" role="menu">
    <button role="menuitem" onclick={(e) => (e.stopPropagation(), act("block"))}>
      <Icon name="block" size={14} />{menu.peer.blocked ? "取消拉黑" : "拉黑此设备"}
    </button>
    <button role="menuitem" onclick={(e) => (e.stopPropagation(), act("clear"))}><Icon name="trash" size={14} />清空聊天记录</button>
    <button role="menuitem" class="danger" onclick={(e) => (e.stopPropagation(), act("forget"))}><Icon name="x" size={14} />删除设备</button>
  </div>
{/if}

<style>
  .sidebar {
    width: 66px;
    flex: none;
    display: flex;
    flex-direction: column;
    align-items: stretch;
    padding: 8px 0;
    background: var(--sidebar);
    border-right: 0.5px solid var(--sep);
    min-height: 0;
  }
  .sep {
    height: 0.5px;
    background: var(--sep);
    margin: 4px 14px 6px;
  }
  .list {
    flex: 1;
    overflow-y: auto;
    overflow-x: hidden;
    scrollbar-width: none;
  }
  .list::-webkit-scrollbar {
    display: none;
  }
  .item {
    width: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 3px;
    padding: 5px 0;
    border-radius: 10px;
    position: relative;
  }
  .avatar {
    position: relative;
    width: 38px;
    height: 38px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    font-size: 13px;
    font-weight: 600;
    color: #fff;
    background: linear-gradient(180deg, color-mix(in srgb, var(--c, #888) 82%, white), var(--c, #888));
    box-shadow: inset 0 0.5px 0 rgba(255, 255, 255, 0.4), 0 1px 2px rgba(0, 0, 0, 0.15);
    transition:
      transform var(--dur) var(--ease-spring),
      opacity var(--dur) var(--ease),
      box-shadow var(--dur-fast) var(--ease);
    /* Keep the avatar on its own compositor layer at rest too. Otherwise the layer is only created
       when the hover transition starts and dropped when it ends; at fractional DPR (1.25/1.5) the
       inner icon/initials are pixel-snapped differently in the two paint paths, so the glyph jumped
       ~1 device px against the circle at hover start/end ("全部" icon out of sync with its disc). */
    will-change: transform;
  }
  .all-avatar {
    background: var(--hover);
    color: var(--text-2);
    box-shadow: inset 0 0 0 0.5px var(--glass-border);
  }
  .item.active .avatar,
  .item.selected .avatar {
    box-shadow: 0 0 0 2px var(--glass-strong), 0 0 0 4px var(--accent);
  }
  .item.active .all-avatar {
    background: var(--accent);
    color: var(--accent-contrast);
  }
  .item:hover .avatar {
    transform: scale(1.05);
  }
  .item:active .avatar {
    transform: scale(0.94);
  }
  .item.drop-hot .avatar {
    transform: scale(1.18);
    box-shadow: 0 0 0 2px var(--glass-strong), 0 0 0 4px var(--accent), 0 6px 18px color-mix(in srgb, var(--accent) 45%, transparent);
  }
  .item.offline .avatar {
    opacity: 0.45;
  }
  .item.blocked .avatar {
    filter: grayscale(1);
  }
  .label {
    max-width: 58px;
    font-size: 10px;
    color: var(--text-2);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .item.active .label {
    color: var(--text);
    font-weight: 500;
  }
  .dot {
    position: absolute;
    right: -1px;
    bottom: -1px;
    width: 11px;
    height: 11px;
    border-radius: 50%;
    background: var(--online);
    border: 2px solid var(--glass-strong);
  }
  .badge {
    position: absolute;
    top: -4px;
    right: -6px;
    min-width: 17px;
    height: 17px;
    padding: 0 4px;
    border-radius: 9px;
    background: var(--danger);
    color: #fff;
    font-size: 10px;
    font-weight: 600;
    line-height: 17px;
    text-align: center;
    border: 1.5px solid var(--glass-strong);
    animation: pop 320ms cubic-bezier(0.2, 0.9, 0.3, 1.6);
  }
  .tick {
    position: absolute;
    left: -3px;
    bottom: -2px;
    width: 15px;
    height: 15px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--accent);
    color: var(--accent-contrast);
    border: 1.5px solid var(--glass-strong);
  }
  @keyframes pop {
    from {
      transform: scale(0.4);
      opacity: 0;
    }
  }
  .menu {
    position: fixed;
    z-index: 50;
    min-width: 150px;
    padding: 5px;
    border-radius: 10px;
    display: flex;
    flex-direction: column;
    background: var(--glass-strong);
    animation: menu-in var(--dur) var(--ease);
    transform-origin: top left;
  }
  .menu button {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 9px;
    border-radius: 6px;
    text-align: left;
    font-size: 12.5px;
  }
  .menu button:hover {
    background: var(--accent);
    color: var(--accent-contrast);
  }
  .menu .danger {
    color: var(--danger);
  }
  @keyframes menu-in {
    from {
      opacity: 0;
      transform: scale(0.95);
    }
  }
</style>
