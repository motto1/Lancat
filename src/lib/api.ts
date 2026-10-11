// Typed wrappers around the Rust commands and events.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface DeviceInfo {
  id: string;
  name: string;
  os: string;
  ver: string;
  port: number;
}

export interface Peer {
  id: string;
  name: string;
  os: string;
  ver: string;
  online: boolean;
  addr: string;
  port: number;
  blocked: boolean;
  unread: number;
  last_ts: number;
  last_preview: string;
}

export interface EmojiPayload {
  kind: "noto" | "sticker";
  code: string;
  alt: string;
  data?: string | null;
}

export interface FileBody {
  name: string;
  size: number;
  transfer_id: string;
  path: string;
  hash: string;
  error?: string | null;
}

export interface Message {
  id: string;
  peer_id: string;
  outgoing: boolean;
  kind: "text" | "emoji" | "file";
  body: any;
  status: string;
  ts: number;
}

export interface Settings {
  device_name: string;
  receive_dir: string;
  dock_edge: "left" | "right" | "top" | "bottom";
  dock_offset: number;
  dock_length: number;
  dock_monitor: number;
  hover_delay_ms: number;
  collapse_delay_ms: number;
  max_file_mb: number;
  blocked_ips: string[];
  manual_peers: string[];
  theme: "system" | "light" | "dark";
  recent_emoji: string[];
  panel_keep_alive_s: number;
}

export type CoreEvent =
  | { type: "peer"; peer: Peer }
  | { type: "peer_removed"; peer_id: string }
  | { type: "message"; message: Message }
  | { type: "message_update"; message: Message }
  | { type: "typing"; peer_id: string; typing: boolean }
  | { type: "progress"; msg_id: string; peer_id: string; done: number; total: number; outgoing: boolean }
  | { type: "history_cleared"; peer_id: string | null }
  | { type: "settings"; settings: Settings };

/** First-frame state injected into a freshly created panel webview (dock.rs `boot_script`). */
export interface Boot {
  me: DeviceInfo;
  peers: Peer[];
  settings: Settings;
  messages: Record<string, Message[]>;
  panel: { visible: boolean; pinned: boolean; edge: string; reason: string };
}

declare global {
  interface Window {
    __LANCAT_BOOT__?: Boot;
  }
}

export interface StickerMeta {
  id: string;
  name: string;
}

export interface MonitorInfo {
  index: number;
  name: string;
  width: number;
  height: number;
  primary: boolean;
}

export function uiLog(msg: string, level: "debug" | "warn" = "debug") {
  invoke("ui_log", { level, msg }).catch(() => {});
}

export const api = {
  me: () => invoke<DeviceInfo>("get_me"),
  peers: () => invoke<Peer[]>("list_peers"),
  refreshPeers: () => invoke<void>("refresh_peers"),
  messages: (peerId: string, beforeTs?: number, limit = 80) =>
    invoke<Message[]>("get_messages", { peerId, beforeTs: beforeTs ?? null, limit }),
  sendText: (peerIds: string[], text: string) => invoke<Message[]>("send_text", { peerIds, text }),
  sendEmoji: (peerIds: string[], emoji: EmojiPayload) => invoke<Message[]>("send_emoji", { peerIds, emoji }),
  sendFiles: (peerIds: string[], paths: string[]) => invoke<Message[]>("send_files", { peerIds, paths }),
  typing: (peerId: string, typing: boolean) => invoke<void>("send_typing", { peerId, typing }),
  markRead: (peerId: string) => invoke<void>("mark_read", { peerId }),
  retry: (msgId: string) => invoke<void>("retry_transfer", { msgId }),
  pauseTransfer: (msgId: string) => invoke<void>("pause_transfer", { msgId }),
  cancelTransfer: (msgId: string) => invoke<void>("cancel_transfer", { msgId }),
  clearHistory: (peerId: string | null) => invoke<void>("clear_history", { peerId }),
  deleteMessage: (id: string) => invoke<void>("delete_message", { id }),
  settings: () => invoke<Settings>("get_settings"),
  setSettings: (settings: Settings) => invoke<Settings>("set_settings", { settings }),
  setBlocked: (peerId: string, blocked: boolean) => invoke<void>("set_blocked", { peerId, blocked }),
  forgetPeer: (peerId: string) => invoke<void>("forget_peer", { peerId }),
  addPeer: (addr: string) => invoke<Peer>("add_peer", { addr }),
  receiveDir: () => invoke<string>("receive_dir"),
  reveal: (path: string) => invoke<void>("reveal_file", { path }),
  stickers: () => invoke<StickerMeta[]>("list_stickers"),
  importSticker: (name: string, lottie: string) => invoke<StickerMeta>("import_sticker", { name, lottie }),
  sticker: (id: string) => invoke<string>("get_sticker", { id }),
  deleteSticker: (id: string) => invoke<void>("delete_sticker", { id }),
  // window / dock
  openPanel: (reason: string) => invoke<void>("cmd_open_panel", { reason }),
  hidePanel: () => invoke<void>("cmd_hide_panel"),
  setPinned: (pinned: boolean) => invoke<void>("set_pinned", { pinned }),
  dockMoveStart: () => invoke<void>("dock_move_start"),
  panelState: () => invoke<{ visible: boolean; pinned: boolean; edge: string; reason: string }>("panel_state"),
  panelShown: () =>
    invoke<void>("panel_shown", {
      pageMs: performance.now(),
      marks: performance
        .getEntriesByType("mark")
        .map((m) => `${m.name}=${Math.round(m.startTime)}`)
        .join(" "),
    }),
  panelReady: () => invoke<void>("panel_ready"),
  setHold: (hold: boolean) => invoke<void>("set_hold", { hold }),
  openSettings: () => invoke<void>("cmd_open_settings"),
  monitors: () => invoke<MonitorInfo[]>("list_monitors"),
  autostart: () => invoke<boolean>("autostart_enabled"),
  setAutostart: (enabled: boolean) => invoke<void>("set_autostart", { enabled }),
};

export function onCore(cb: (e: CoreEvent) => void): Promise<UnlistenFn> {
  return listen<CoreEvent>("lancat://event", (e) => cb(e.payload));
}

export function formatSize(n: number): string {
  if (n < 1024) return `${n} B`;
  const u = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${u[i]}`;
}

export function formatTime(ts: number): string {
  const d = new Date(ts);
  const now = new Date();
  const hm = d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  if (d.toDateString() === now.toDateString()) return hm;
  const y = new Date(now);
  y.setDate(now.getDate() - 1);
  if (d.toDateString() === y.toDateString()) return `昨天 ${hm}`;
  return `${d.getMonth() + 1}月${d.getDate()}日 ${hm}`;
}

/** Stable pastel-ish color per device id for avatars. */
export function avatarColor(id: string): string {
  let h = 0;
  for (const c of id) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  const hues = [211, 262, 330, 14, 32, 145, 175, 195, 290];
  return `hsl(${hues[h % hues.length]} 70% 55%)`;
}

export function initials(name: string): string {
  const n = name.trim();
  if (!n) return "?";
  const cjk = n.match(/[\u3400-\u9fff]/);
  if (cjk) return cjk[0];
  const parts = n.split(/[\s\-_.]+/).filter(Boolean);
  if (parts.length >= 2) return (parts[0][0] + parts[1][0]).toUpperCase();
  return n.slice(0, 2).toUpperCase();
}

export function osGlyph(os: string): string {
  return { windows: "Windows", macos: "macOS", linux: "Linux", android: "Android", ios: "iOS" }[os] ?? os;
}
