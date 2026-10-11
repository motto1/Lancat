<script lang="ts">
  // Shown while files are dragged over the panel: the online devices as large round avatars
  // (HarmonyOS-style user picker). The one under the pointer inflates and pushes its neighbours
  // aside; dropping on it sends the files there. Each avatar is a drop zone ("peer:<id>"), so the
  // drop routing in Panel.svelte (hitTest / dropFiles) works unchanged.
  import { avatarColor, initials, osGlyph } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import { ui } from "../lib/ui.svelte";
  import { liquid } from "../lib/liquid";

  let peers = $derived(app.onlinePeers);
  let focused = $derived(ui.dropKey?.startsWith("peer:") ? ui.dropKey.slice(5) : ui.dropKey === "all" ? "all" : null);
  let count = $derived(ui.dragPaths.length);
  let focusName = $derived(focused === "all" ? `全部 ${peers.length} 台设备` : (peers.find((p) => p.id === focused)?.name ?? null));
</script>

<div class="picker" class:has-focus={focused !== null}>
  <div class="hint" use:liquid={{ radius: 15, depth: 14, blur: 4 }}>
    {#if focusName}
      松开发送给 <b>{focusName}</b>
    {:else if peers.length}
      拖到头像上发送{count ? ` ${count} 项` : ""}
    {:else}
      没有在线设备
    {/if}
  </div>
  <div class="ring">
    {#each peers as p, i (p.id)}
      <div class="slot" class:focus={focused === p.id} data-drop={"peer:" + p.id} style:--i={i}>
        <div class="avatar" style:--c={avatarColor(p.id)}>{initials(p.name)}</div>
        <div class="name">{p.name}</div>
        <div class="os">{osGlyph(p.os)}</div>
      </div>
    {/each}
    {#if peers.length > 1}
      <div class="slot all" class:focus={focused === "all"} data-drop="all" style:--i={peers.length}>
        <div class="avatar">全部</div>
        <div class="name">全部设备</div>
        <div class="os">{peers.length} 台</div>
      </div>
    {/if}
  </div>
</div>

<style>
  .picker {
    position: absolute;
    inset: 0;
    z-index: 20;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 22px;
    padding: 24px 14px;
    background: color-mix(in srgb, var(--bg, #f5f5f7) 55%, transparent);
    backdrop-filter: blur(14px) saturate(160%);
    animation: picker-in 240ms cubic-bezier(0.2, 0.9, 0.3, 1) both;
  }
  @keyframes picker-in {
    from {
      opacity: 0;
    }
  }
  .hint {
    font-size: 13px;
    color: var(--text-2);
    min-height: 18px;
    text-align: center;
    padding: 7px 16px;
    border-radius: 15px;
    background: color-mix(in srgb, var(--glass-strong) 35%, transparent);
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.7),
      inset 0 -1px 0 rgba(0, 0, 0, 0.06),
      0 6px 18px rgba(0, 0, 0, 0.12);
    transition: color 160ms;
  }
  .hint b {
    color: var(--text);
    font-weight: 600;
  }
  .ring {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    align-items: center;
    gap: 6px;
    max-width: 100%;
  }
  .slot {
    /* the width itself grows on focus, so the row reflows: neighbours are pushed aside */
    width: 84px;
    height: 132px;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 6px;
    border-radius: 18px;
    transition:
      width 320ms cubic-bezier(0.34, 1.56, 0.64, 1),
      opacity 220ms;
    animation: slot-in 420ms cubic-bezier(0.34, 1.56, 0.64, 1) both;
    animation-delay: calc(var(--i) * 45ms);
  }
  @keyframes slot-in {
    from {
      opacity: 0;
      transform: translateY(18px) scale(0.6);
    }
  }
  .has-focus .slot:not(.focus) {
    width: 70px;
    opacity: 0.62;
  }
  .slot.focus {
    width: 124px;
  }
  .avatar {
    width: 58px;
    height: 58px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    color: #fff;
    font-size: 21px;
    font-weight: 600;
    background:
      radial-gradient(120% 90% at 30% 20%, rgba(255, 255, 255, 0.45), transparent 55%),
      var(--c, var(--accent));
    box-shadow:
      0 6px 16px -4px color-mix(in srgb, var(--c, var(--accent)) 45%, transparent),
      inset 0 1px 1px rgba(255, 255, 255, 0.55),
      inset 0 -2px 6px rgba(0, 0, 0, 0.12);
    transform: scale(1);
    transition:
      transform 320ms cubic-bezier(0.34, 1.56, 0.64, 1),
      box-shadow 320ms;
    pointer-events: none;
  }
  .has-focus .slot:not(.focus) .avatar {
    transform: scale(0.86);
  }
  .slot.focus .avatar {
    transform: scale(1.55);
    /* a soft halo in the avatar's own colour instead of a hard ring */
    box-shadow:
      0 0 22px 4px color-mix(in srgb, var(--c, var(--accent)) 55%, transparent),
      0 0 46px 14px color-mix(in srgb, var(--c, var(--accent)) 22%, transparent),
      inset 0 1px 1px rgba(255, 255, 255, 0.6);
  }
  .all .avatar {
    font-size: 15px;
    background:
      radial-gradient(120% 90% at 30% 20%, rgba(255, 255, 255, 0.45), transparent 55%),
      linear-gradient(135deg, #5ac8fa, #0a84ff 55%, #5e5ce6);
  }
  .name,
  .os {
    max-width: 100%;
    padding: 0 4px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    text-align: center;
    pointer-events: none;
    transition: transform 320ms cubic-bezier(0.34, 1.56, 0.64, 1);
  }
  .name {
    font-size: 12.5px;
    font-weight: 600;
    color: var(--text);
  }
  .os {
    font-size: 11px;
    color: var(--text-3);
    margin-top: -4px;
  }
  .slot.focus .name,
  .slot.focus .os {
    transform: translateY(16px);
  }
</style>
