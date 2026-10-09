<script lang="ts">
  import { api } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import Icon from "./Icon.svelte";

  let addr = $state("");
  let busy = $state(false);

  async function add() {
    if (!addr.trim()) return;
    busy = true;
    try {
      const p = await api.addPeer(addr.trim());
      const s = app.settings;
      if (s && !s.manual_peers.includes(addr.trim())) {
        await api.setSettings({ ...s, manual_peers: [...s.manual_peers, addr.trim()] });
      }
      addr = "";
      app.open(p.id);
    } catch (e) {
      app.flash(String(e));
    } finally {
      busy = false;
    }
  }
</script>

<div class="empty">
  <div class="radar">
    <span class="ring r1"></span><span class="ring r2"></span><span class="ring r3"></span>
    <span class="core"><Icon name="radar" size={22} /></span>
  </div>
  <div class="t">正在寻找局域网内的设备…</div>
  <div class="s">在其他电脑上打开 Lancat，它们会自动出现在左侧。{#if app.me}本机名为「{app.me.name}」。{/if}</div>
  <form
    class="manual"
    onsubmit={(e) => {
      e.preventDefault();
      add();
    }}
  >
    <input bind:value={addr} placeholder="手动添加：192.168.1.20" />
    <button type="submit" disabled={busy}><Icon name="plus" size={14} /></button>
  </form>
</div>

<style>
  .empty {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    text-align: center;
    padding: 20px;
    gap: 6px;
  }
  .radar {
    position: relative;
    width: 88px;
    height: 88px;
    display: grid;
    place-items: center;
    margin-bottom: 8px;
  }
  .ring {
    position: absolute;
    inset: 0;
    border-radius: 50%;
    border: 1.5px solid var(--accent);
    opacity: 0;
    animation: ping 2.4s cubic-bezier(0.2, 0.6, 0.4, 1) infinite;
  }
  .r2 {
    animation-delay: 0.8s;
  }
  .r3 {
    animation-delay: 1.6s;
  }
  .core {
    width: 44px;
    height: 44px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    color: #fff;
    background: var(--accent);
    box-shadow: 0 4px 14px color-mix(in srgb, var(--accent) 45%, transparent);
  }
  @keyframes ping {
    0% {
      transform: scale(0.45);
      opacity: 0.7;
    }
    100% {
      transform: scale(1);
      opacity: 0;
    }
  }
  .t {
    font-weight: 600;
    font-size: 13.5px;
  }
  .s {
    color: var(--text-2);
    font-size: 11.5px;
    line-height: 1.5;
    max-width: 230px;
  }
  .manual {
    margin-top: 12px;
    display: flex;
    gap: 6px;
  }
  .manual input {
    width: 170px;
    border: 0.5px solid var(--glass-border);
    background: var(--field);
    border-radius: 7px;
    padding: 5px 9px;
    outline: none;
    font-size: 12px;
  }
  .manual button {
    width: 28px;
    border-radius: 7px;
    background: var(--accent);
    color: #fff;
    display: grid;
    place-items: center;
  }
</style>
