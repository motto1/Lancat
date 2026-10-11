// Reactive app state shared by the panel components (Svelte 5 runes).
import { api, onCore, type Boot, type CoreEvent, type DeviceInfo, type Message, type Peer, type Settings } from "./api";

const peerOrder = (a: Peer, b: Peer) => Number(b.online) - Number(a.online) || b.last_ts - a.last_ts || a.name.localeCompare(b.name);

/** Sorted copy. Sorting the reactive array in place went through the $state proxy: every swap was
 *  a tracked write (and a re-render of everything reading `peers`) instead of one assignment. */
function sortedPeers(list: Peer[]): Peer[] {
  return list.slice().sort(peerOrder);
}

/** Union by id, ordered by ts. `fresh` wins over `base` except for ids in `keep` (changed by an
 *  event after `fresh` was fetched). */
function mergeMessages(base: Message[], fresh: Message[], keep?: Set<string>): Message[] {
  const byId = new Map<string, Message>();
  for (const m of base) byId.set(m.id, m);
  for (const m of fresh) if (!keep?.has(m.id) || !byId.has(m.id)) byId.set(m.id, m);
  return [...byId.values()].sort((a, b) => a.ts - b.ts);
}

class AppState {
  me = $state<DeviceInfo | null>(null);
  peers = $state<Peer[]>([]);
  settings = $state<Settings | null>(null);
  /** Conversation shown on the right. */
  current = $state<string | null>(null);
  /** Shift-multi-selected peers (send targets when >1). */
  selected = $state<string[]>([]);
  /** "全部" mode: send to every online device. */
  broadcast = $state(false);
  messages = $state<Record<string, Message[]>>({});
  hasMore = $state<Record<string, boolean>>({});
  progress = $state<Record<string, { done: number; total: number }>>({});
  typing = $state<Record<string, number>>({});
  /** Panel is on screen (unread is only cleared while visible). */
  visible = $state(false);
  toast = $state<string | null>(null);
  private toastTimer: ReturnType<typeof setTimeout> | undefined;
  /** Conversations whose history is being fetched -> events that arrived meanwhile. */
  private fetching = new Map<string, Message[]>();
  /** Conversations loading older history (scroll to top); guards against re-entry. */
  private loadingOlder = new Set<string>();
  /** Message ids changed by events while a resync runs (the resync result may be older). */
  private touched: Set<string> | null = null;
  /** Ids of messages that arrived live (not from history): only these play the entry animation. */
  private arrivals = new Set<string>();

  onlinePeers = $derived(this.peers.filter((p) => p.online && !p.blocked));
  totalUnread = $derived(this.peers.reduce((n, p) => n + (p.blocked ? 0 : p.unread), 0));
  currentPeer = $derived(this.peers.find((p) => p.id === this.current) ?? null);
  transferring = $derived(
    Object.values(this.messages).some((l) => l.some((m) => m.kind === "file" && (m.status === "transferring" || m.status === "receiving"))),
  );

  /** Ids a message typed in the composer goes to. */
  targets(): string[] {
    if (this.broadcast) return this.onlinePeers.map((p) => p.id);
    if (this.selected.length > 1) return [...this.selected];
    return this.current ? [this.current] : [];
  }

  /** `boot` = state injected by Rust into a freshly created panel (no IPC needed for the first frame). */
  async init(boot?: Boot) {
    const [me, peers, settings] = boot ? [boot.me, boot.peers, boot.settings] : await Promise.all([api.me(), api.peers(), api.settings()]);
    performance.mark("ipc3");
    if (boot) for (const [id, list] of Object.entries(boot.messages)) {
      this.messages[id] = list;
      this.hasMore[id] = list.length >= 80;
    }
    this.me = me;
    if (me.os === "windows" || me.os === "macos") document.documentElement.classList.add("native-glass");
    this.peers = sortedPeers(peers);
    this.settings = settings;
    applyTheme(settings.theme);
    if (!this.current && peers.length) await this.open(peers[0].id);
    performance.mark("msgs");
    await onCore((e) => this.onEvent(e));
    if (boot) this.resync();
  }

  /** After a boot from injected state: pick up anything that changed before our listener existed. */
  private async resync() {
    // Merge, never replace: events handled since the boot snapshot (and while these calls are in
    // flight) are already in the lists, and older history may have been loaded meanwhile.
    this.touched = new Set();
    try {
      const peers = await api.peers();
      this.peers = sortedPeers(peers);
      for (const id of Object.keys(this.messages)) {
        const list = await api.messages(id);
        this.messages[id] = mergeMessages(this.messages[id] ?? [], list, this.touched);
      }
      this.markReadIfVisible();
    } catch {
      /* the next event brings us up to date anyway */
    } finally {
      this.touched = null;
    }
  }

  async open(peerId: string) {
    this.current = peerId;
    this.broadcast = false;
    if (!this.messages[peerId] && !this.fetching.has(peerId)) {
      // messages arriving while the history is fetched are kept here and merged in afterwards
      // (they used to be dropped: no list yet, then the fetched one replaced everything)
      this.fetching.set(peerId, []);
      try {
        const list = await api.messages(peerId);
        const live = this.fetching.get(peerId) ?? [];
        this.messages[peerId] = live.length ? mergeMessages(list, live) : list;
        this.hasMore[peerId] = list.length >= 80;
      } finally {
        this.fetching.delete(peerId);
      }
    }
    this.markReadIfVisible();
  }

  /** Prepends the next page of history. Re-entrant calls (scroll events keep firing near the
   *  top) are ignored while one is in flight: they fetched the same page twice and inserted
   *  duplicate ids, which broke the keyed list and made it jump. */
  async loadOlder(peerId: string): Promise<boolean> {
    const list = this.messages[peerId];
    if (!list || !this.hasMore[peerId] || this.loadingOlder.has(peerId)) return false;
    this.loadingOlder.add(peerId);
    try {
      const older = await api.messages(peerId, list[0]?.ts, 80);
      this.hasMore[peerId] = older.length >= 80;
      if (!older.length) return false;
      this.messages[peerId] = mergeMessages(this.messages[peerId] ?? [], older);
      return true;
    } finally {
      this.loadingOlder.delete(peerId);
    }
  }

  /** True once, for a message that arrived live (plays the entry animation). */
  takeArrival(id: string): boolean {
    return this.arrivals.delete(id);
  }

  markReadIfVisible() {
    const id = this.current;
    if (!id || !this.visible) return;
    const p = this.peers.find((x) => x.id === id);
    if (p && p.unread > 0) {
      p.unread = 0;
      api.markRead(id).catch(() => {});
    }
  }

  toggleSelect(peerId: string) {
    this.broadcast = false;
    const i = this.selected.indexOf(peerId);
    if (i >= 0) this.selected.splice(i, 1);
    else {
      if (this.selected.length === 0 && this.current && this.current !== peerId) this.selected.push(this.current);
      this.selected.push(peerId);
    }
  }

  clearSelection() {
    this.selected = [];
  }

  /** One timer for all toasts: each flash restarts the 2.6 s (a repeated message used to be
   *  cut short by the timer of its first showing). */
  flash(msg: string) {
    this.toast = msg;
    clearTimeout(this.toastTimer);
    this.toastTimer = setTimeout(() => (this.toast = null), 2600);
  }

  private upsertMessage(m: Message, live = false) {
    this.touched?.add(m.id);
    const list = this.messages[m.peer_id];
    if (!list) {
      const buf = this.fetching.get(m.peer_id);
      if (buf) {
        const j = buf.findIndex((x) => x.id === m.id);
        if (j >= 0) buf[j] = m;
        else buf.push(m);
      }
      return;
    }
    const i = list.findIndex((x) => x.id === m.id);
    if (i >= 0) list[i] = m;
    else {
      if (live) {
        this.arrivals.add(m.id);
        if (this.arrivals.size > 200) this.arrivals.delete(this.arrivals.values().next().value!);
      }
      // keep ordered by ts
      let j = list.length;
      while (j > 0 && list[j - 1].ts > m.ts) j--;
      list.splice(j, 0, m);
    }
  }

  onEvent(e: CoreEvent) {
    switch (e.type) {
      case "peer": {
        const list = this.peers.slice();
        const i = list.findIndex((p) => p.id === e.peer.id);
        if (i >= 0) list[i] = e.peer;
        else list.push(e.peer);
        this.peers = sortedPeers(list);
        if (!this.current) this.open(e.peer.id);
        if (e.peer.id === this.current) this.markReadIfVisible();
        break;
      }
      case "peer_removed":
        this.peers = this.peers.filter((p) => p.id !== e.peer_id);
        delete this.messages[e.peer_id];
        if (this.current === e.peer_id) this.current = this.peers[0]?.id ?? null;
        this.selected = this.selected.filter((x) => x !== e.peer_id);
        break;
      case "message":
        this.upsertMessage(e.message, true);
        if (!e.message.outgoing) delete this.typing[e.message.peer_id];
        break;
      case "message_update":
        this.upsertMessage(e.message);
        if (e.message.status === "done" || e.message.status === "failed") delete this.progress[e.message.id];
        break;
      case "progress":
        this.progress[e.msg_id] = { done: e.done, total: e.total };
        break;
      case "typing":
        if (e.typing) this.typing[e.peer_id] = Date.now() + 5000;
        else delete this.typing[e.peer_id];
        break;
      case "history_cleared":
        if (e.peer_id) this.messages[e.peer_id] = [];
        else for (const k of Object.keys(this.messages)) this.messages[k] = [];
        for (const p of this.peers) if (!e.peer_id || p.id === e.peer_id) (p.unread = 0), (p.last_preview = "");
        break;
      case "settings":
        this.settings = e.settings;
        applyTheme(e.settings.theme);
        break;
    }
  }
}

let appliedTheme: string | null = null;

/** Applies the theme setting. A no-op when it did not change: the `settings` event fires for
 *  every settings save (recent emoji after each pick, dock moves …) and this forces a synchronous
 *  style recalc + layout of the whole page. */
export function applyTheme(theme: string) {
  if (theme === appliedTheme) return;
  appliedTheme = theme;
  const root = document.documentElement;
  if (theme === "light" || theme === "dark") root.dataset.theme = theme;
  else delete root.dataset.theme;
  // Some engines do not re-resolve inherited `color: var(--text)` after a root attribute flip;
  // re-assigning it on <body> forces the cascade to pick up the new palette.
  document.body.style.color = "";
  void document.body.offsetHeight;
  document.body.style.color = "var(--text)";
}

export const app = new AppState();
