// Browser-only mock of the Tauri runtime, for UI previews / screenshots without the Rust side.
// Usage: scripts/preview/shot.sh (builds dist/, injects this file, screenshots with headless Chrome).
(function () {
  const q = new URLSearchParams(location.search);
  const label = q.get("window") || "panel";
  const theme = q.get("theme");
  const now = Date.now();
  const listeners = {};
  const callbacks = {};
  let cbId = 1;

  const peers = [
    { id: "a1", name: "MacBook Pro", os: "macos", ver: "0.1.0", online: true, addr: "192.168.1.23", port: 47820, blocked: false, unread: 0, last_ts: now - 60000, last_preview: "" },
    { id: "b2", name: "Studio-PC", os: "windows", ver: "0.1.0", online: true, addr: "192.168.1.31", port: 47820, blocked: false, unread: 3, last_ts: now - 3600000, last_preview: "" },
    { id: "c3", name: "小米的笔记本", os: "linux", ver: "0.1.0", online: true, addr: "192.168.1.40", port: 47820, blocked: false, unread: 0, last_ts: now - 7200000, last_preview: "" },
    { id: "d4", name: "NAS", os: "linux", ver: "0.1.0", online: false, addr: "192.168.1.2", port: 47820, blocked: false, unread: 0, last_ts: 0, last_preview: "" },
  ];
  const empty = q.get("empty") === "1";
  const msgs = {
    a1: [
      { id: "m1", peer_id: "a1", outgoing: false, kind: "text", body: { text: "下午的设计稿发我一下？" }, status: "received", ts: now - 900000 },
      { id: "m2", peer_id: "a1", outgoing: true, kind: "text", body: { text: "好的，马上～" }, status: "delivered", ts: now - 880000 },
      { id: "m3", peer_id: "a1", outgoing: true, kind: "file", body: { name: "Lancat-UI-v3.fig", size: 48234496, transfer_id: "t1", path: "/home/me/Lancat-UI-v3.fig", hash: "" }, status: "done", ts: now - 870000 },
      { id: "m4", peer_id: "a1", outgoing: false, kind: "emoji", body: { kind: "noto", code: "1f973", alt: "🥳" }, status: "received", ts: now - 860000 },
      { id: "m5", peer_id: "a1", outgoing: false, kind: "file", body: { name: "演示视频 final.mov", size: 734003200, transfer_id: "t2", path: "", hash: "" }, status: "receiving", ts: now - 60000 },
      { id: "m6", peer_id: "a1", outgoing: true, kind: "text", body: { text: "收到，谢谢！视频有点大，我先下着 🙌" }, status: "sent", ts: now - 30000 },
    ],
  };
  const settings = {
    device_name: "", receive_dir: "", dock_edge: q.get("edge") || "right", dock_offset: 0.5, dock_length: 160, dock_monitor: 0,
    hover_delay_ms: 150, collapse_delay_ms: 800, max_file_mb: 0, blocked_ips: [], manual_peers: [], theme: theme || "system", recent_emoji: ["1f44d", "1f602", "2764_fe0f"],
  };

  const handlers = {
    get_me: () => ({ id: "me", name: "motto-arch", os: q.get("os") || "linux", ver: "0.1.0", port: 47820 }),
    list_peers: () => (empty ? [] : peers),
    get_messages: (a) => msgs[a.peerId] || [],
    get_settings: () => settings,
    set_settings: (a) => a.settings,
    receive_dir: () => "/home/motto/Downloads",
    list_stickers: () => [],
    list_monitors: () => [{ index: 0, name: "eDP-1", width: 2560, height: 1600, primary: true }],
    autostart_enabled: () => true,
    panel_state: () => ({ visible: false, pinned: false, edge: settings.dock_edge }),
    "plugin:event|listen": (a) => {
      (listeners[a.event] ||= []).push(a.handler);
      return a.handler;
    },
    "plugin:event|unlisten": () => null,
  };

  window.__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label }, currentWebview: { windowLabel: label, label } },
    transformCallback(cb) {
      const id = cbId++;
      callbacks[id] = cb;
      return id;
    },
    unregisterCallback(id) {
      delete callbacks[id];
    },
    convertFileSrc: (p) => p,
    invoke: async (cmd, args) => {
      const h = handlers[cmd];
      return h ? h(args || {}) : null;
    },
  };
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };

  window.__mockEmit = function (event, payload) {
    for (const id of listeners[event] || []) callbacks[id]?.({ event, id: 0, payload });
  };
  window.addEventListener("load", () => {
    setTimeout(() => {
      window.__mockEmit("lancat://event", { type: "progress", msg_id: "m5", peer_id: "a1", done: 301989888, total: 734003200, outgoing: false });
      window.__mockEmit("panel://show", { edge: settings.dock_edge, reason: "hover" });
      if (q.get("emoji") === "1") document.querySelector('button[title="表情"]')?.click();
    }, 400);
  });
})();
