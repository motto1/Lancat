<script lang="ts">
  // Settings window: device, dock edge/position/length/monitor, timings, receive folder,
  // size limit, blacklist, manual peers, autostart, theme, clear history, about.
  import { onMount } from "svelte";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import { api, onCore, osGlyph, type DeviceInfo, type MonitorInfo, type Peer, type Settings } from "../lib/api";
  import { applyTheme } from "../lib/state.svelte";
  import { EMOJI } from "../lib/emoji-catalog";

  let s = $state<Settings | null>(null);
  let me = $state<DeviceInfo | null>(null);
  let peers = $state<Peer[]>([]);
  let monitors = $state<MonitorInfo[]>([]);
  let recvDefault = $state("");
  let autostart = $state(false);
  let confirmClear = $state(false);
  let saved = $state(false);
  let blockedIps = $state("");
  let saveTimer: ReturnType<typeof setTimeout> | undefined;

  onMount(() => {
    let un: (() => void) | undefined;
    (async () => {
      [s, me, peers, monitors, recvDefault, autostart] = await Promise.all([
        api.settings(),
        api.me(),
        api.peers(),
        api.monitors(),
        api.receiveDir(),
        api.autostart().catch(() => false),
      ]);
      blockedIps = s!.blocked_ips.join("\n");
      applyTheme(s!.theme);
      un = await onCore((e) => {
        if (e.type === "peer") {
          const i = peers.findIndex((p) => p.id === e.peer.id);
          if (i >= 0) peers[i] = e.peer;
          else peers.push(e.peer);
        } else if (e.type === "peer_removed") peers = peers.filter((p) => p.id !== e.peer_id);
      });
    })();
    return () => un?.();
  });

  function save(immediate = false) {
    clearTimeout(saveTimer);
    const run = async () => {
      if (!s) return;
      s.blocked_ips = blockedIps
        .split(/[\s,]+/)
        .map((x) => x.trim())
        .filter(Boolean);
      s = await api.setSettings($state.snapshot(s) as Settings);
      applyTheme(s.theme);
      recvDefault = await api.receiveDir();
      me = await api.me();
      saved = true;
      setTimeout(() => (saved = false), 1200);
    };
    if (immediate) run();
    else saveTimer = setTimeout(run, 250);
  }

  async function pickDir() {
    const d = await openDialog({ directory: true, title: "选择接收目录" });
    if (typeof d === "string" && s) {
      s.receive_dir = d;
      save(true);
    }
  }

  async function toggleAutostart() {
    autostart = !autostart;
    try {
      await api.setAutostart(autostart);
    } catch {
      autostart = !autostart;
    }
  }

  async function clearAll() {
    if (!confirmClear) {
      confirmClear = true;
      setTimeout(() => (confirmClear = false), 4000);
      return;
    }
    confirmClear = false;
    await api.clearHistory(null);
    saved = true;
    setTimeout(() => (saved = false), 1200);
  }

  function removeManual(a: string) {
    if (!s) return;
    s.manual_peers = s.manual_peers.filter((x) => x !== a);
    save(true);
  }
</script>

<main class="settings">
  {#if s && me}
    <h1>Lancat 设置 {#if saved}<span class="saved">已保存</span>{/if}</h1>

    <section>
      <h2>本机</h2>
      <label class="row">
        <span>设备名称</span>
        <input bind:value={s.device_name} placeholder={me.name} oninput={() => save()} />
      </label>
      <div class="row info"><span>设备 ID</span><code class="selectable">{me.id}</code></div>
      <div class="row info"><span>服务端口</span><code>TCP/UDP {me.port}</code></div>
    </section>

    <section>
      <h2>停靠</h2>
      <div class="row">
        <span>屏幕边缘</span>
        <div class="seg">
          {#each [["left", "左"], ["right", "右"], ["top", "上"], ["bottom", "下"]] as [v, l]}
            <button class:on={s.dock_edge === v} onclick={() => ((s!.dock_edge = v as Settings["dock_edge"]), save(true))}>{l}</button>
          {/each}
        </div>
      </div>
      <label class="row">
        <span>位置</span>
        <input type="range" min="0" max="1" step="0.01" bind:value={s.dock_offset} oninput={() => save()} />
        <em>{Math.round(s.dock_offset * 100)}%</em>
      </label>
      <label class="row">
        <span>隐藏条长度</span>
        <input type="range" min="60" max="800" step="10" bind:value={s.dock_length} oninput={() => save()} />
        <em>{s.dock_length}px</em>
      </label>
      {#if monitors.length > 1}
        <label class="row">
          <span>显示器</span>
          <select bind:value={s.dock_monitor} onchange={() => save(true)}>
            {#each monitors as m}
              <option value={m.index}>{m.name}（{m.width}×{m.height}）{m.primary ? " · 主" : ""}</option>
            {/each}
          </select>
        </label>
      {/if}
      <label class="row">
        <span>悬停弹出延迟</span>
        <input type="range" min="0" max="800" step="10" bind:value={s.hover_delay_ms} oninput={() => save()} />
        <em>{s.hover_delay_ms}ms</em>
      </label>
      <label class="row">
        <span>离开后收起延迟</span>
        <input type="range" min="200" max="5000" step="50" bind:value={s.collapse_delay_ms} oninput={() => save()} />
        <em>{s.collapse_delay_ms}ms</em>
      </label>
    </section>

    <section>
      <h2>接收</h2>
      <div class="row">
        <span>接收目录</span>
        <code class="path selectable" title={recvDefault}>{recvDefault}</code>
      </div>
      <div class="row right">
        <button class="btn" onclick={pickDir}>选择…</button>
        {#if s.receive_dir}<button class="btn" onclick={() => ((s!.receive_dir = ""), save(true))}>恢复默认（下载）</button>{/if}
      </div>
      <label class="row">
        <span>单文件大小上限</span>
        <input class="num" type="number" min="0" step="100" bind:value={s.max_file_mb} oninput={() => save()} />
        <em>MB（0 = 不限）</em>
      </label>
      <p class="note">收到的文件直接保存、自动重名，永远不会被自动打开或执行。</p>
    </section>

    <section>
      <h2>设备与黑名单</h2>
      {#if peers.length === 0}
        <p class="note">还没有发现其他设备。</p>
      {/if}
      {#each peers as p (p.id)}
        <div class="row device">
          <span class="dot" class:online={p.online}></span>
          <span class="dname">{p.name}<small>{osGlyph(p.os)} · {p.addr || "—"}</small></span>
          <button class="btn" class:danger={!p.blocked} onclick={() => api.setBlocked(p.id, !p.blocked)}>{p.blocked ? "取消拉黑" : "拉黑"}</button>
        </div>
      {/each}
      <label class="col">
        <span>按 IP 拉黑（每行一个）</span>
        <textarea rows="2" bind:value={blockedIps} oninput={() => save()} placeholder="192.168.1.66"></textarea>
      </label>
      {#if s.manual_peers.length}
        <div class="col">
          <span>手动添加的设备（每 5 秒探测一次）</span>
          {#each s.manual_peers as a}
            <div class="row"><code>{a}</code><button class="btn" onclick={() => removeManual(a)}>移除</button></div>
          {/each}
        </div>
      {/if}
    </section>

    <section>
      <h2>通用</h2>
      <div class="row">
        <span>开机自动启动</span>
        <button class="switch" class:on={autostart} onclick={toggleAutostart} aria-label="开机自启"><i></i></button>
      </div>
      <div class="row">
        <span>外观</span>
        <div class="seg">
          {#each [["system", "跟随系统"], ["light", "浅色"], ["dark", "深色"]] as [v, l]}
            <button class:on={s.theme === v} onclick={() => ((s!.theme = v as Settings["theme"]), save(true))}>{l}</button>
          {/each}
        </div>
      </div>
    </section>

    <section>
      <h2>数据</h2>
      <div class="row">
        <span>聊天记录永久保存在本机</span>
        <button class="btn danger" onclick={clearAll}>{confirmClear ? "再点一次确认清空" : "清空全部聊天记录"}</button>
      </div>
    </section>

    <section class="about">
      <h2>关于</h2>
      <p>Lancat {me.ver} · {osGlyph(me.os)} · 局域网零配置快传</p>
      <p class="note">
        内置 {EMOJI.length} 个动态表情来自 Google
        <b>Noto Animated Emoji</b>，以 <b>CC BY 4.0</b> 许可使用（https://creativecommons.org/licenses/by/4.0/）。
      </p>
    </section>
  {/if}
</main>

<style>
  :global(html[data-window="settings"]),
  :global(html[data-window="settings"] body) {
    background: var(--glass-strong);
    overflow: auto;
    user-select: none;
  }
  .settings {
    padding: 18px 22px 30px;
    max-width: 620px;
    margin: 0 auto;
  }
  h1 {
    font-size: 17px;
    font-weight: 700;
    margin: 0 0 14px;
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .saved {
    font-size: 11px;
    font-weight: 500;
    color: var(--online);
    animation: fade 1.2s ease forwards;
  }
  @keyframes fade {
    70% {
      opacity: 1;
    }
    to {
      opacity: 0;
    }
  }
  section {
    background: var(--field);
    border: 0.5px solid var(--glass-border);
    border-radius: 11px;
    padding: 6px 14px 10px;
    margin-bottom: 14px;
  }
  h2 {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-3);
    margin: 8px 0 4px;
    font-weight: 600;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: 34px;
    border-bottom: 0.5px solid var(--sep);
  }
  .row:last-child {
    border-bottom: none;
  }
  .row > span:first-child:not(.dot) {
    flex: 1;
  }
  .row.right {
    justify-content: flex-end;
    border-bottom: none;
  }
  .col {
    display: flex;
    flex-direction: column;
    gap: 5px;
    padding: 8px 0 2px;
  }
  .info code,
  code {
    font-family: var(--mono);
    font-size: 11px;
    color: var(--text-2);
  }
  .path {
    max-width: 300px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    direction: rtl;
  }
  input:not([type]),
  input[type="number"],
  select,
  textarea {
    border: 0.5px solid var(--glass-border);
    background: var(--glass-strong);
    border-radius: 6px;
    padding: 4px 8px;
    outline: none;
    font-size: 12.5px;
  }
  input:not([type]) {
    width: 220px;
  }
  .num {
    width: 90px;
  }
  textarea {
    resize: vertical;
    font-family: var(--mono);
  }
  input:focus,
  textarea:focus,
  select:focus {
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 35%, transparent);
  }
  input[type="range"] {
    width: 180px;
    accent-color: var(--accent);
  }
  em {
    font-style: normal;
    font-size: 11.5px;
    color: var(--text-2);
    min-width: 54px;
    text-align: right;
  }
  .seg {
    display: inline-flex;
    padding: 2px;
    border-radius: 7px;
    background: var(--hover);
  }
  .seg button {
    padding: 3px 12px;
    border-radius: 5px;
    font-size: 12px;
  }
  .seg button.on {
    background: var(--glass-strong);
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.15);
    font-weight: 500;
  }
  .btn {
    padding: 4px 11px;
    border-radius: 6px;
    background: var(--glass-strong);
    border: 0.5px solid var(--glass-border);
    font-size: 12px;
    box-shadow: 0 0.5px 1px rgba(0, 0, 0, 0.08);
  }
  .btn:active {
    transform: scale(0.97);
  }
  .btn.danger {
    color: var(--danger);
  }
  .note {
    font-size: 11.5px;
    color: var(--text-2);
    line-height: 1.5;
    margin: 6px 0 2px;
  }
  .device .dname {
    flex: 1;
    display: flex;
    flex-direction: column;
    font-size: 12.5px;
  }
  .device small {
    color: var(--text-3);
    font-size: 10.5px;
  }
  .dot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--text-3);
  }
  .dot.online {
    background: var(--online);
  }
  .switch {
    width: 38px;
    height: 22px;
    border-radius: 11px;
    background: var(--hover);
    position: relative;
    box-shadow: inset 0 0 0 0.5px var(--glass-border);
    transition: background-color 160ms ease;
  }
  .switch i {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.3);
    transition: transform 220ms cubic-bezier(0.2, 0.9, 0.3, 1.3);
  }
  .switch.on {
    background: var(--online);
  }
  .switch.on i {
    transform: translateX(16px);
  }
  .about p {
    margin: 4px 0;
    font-size: 12px;
  }
</style>
