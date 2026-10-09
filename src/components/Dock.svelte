<script lang="ts">
  // The 6px always-on-top strip. Hover (150 ms) or a file drag opens the panel.
  import { onMount } from "svelte";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { listen } from "@tauri-apps/api/event";
  import { api, onCore, type Peer } from "../lib/api";
  import { applyTheme } from "../lib/state.svelte";

  let edge = $state("right");
  let unread = $state(0);
  let open = $state(false);
  let hovering = $state(false);
  let delay = 150;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const peers = new Map<string, Peer>();

  function recount() {
    let n = 0;
    for (const p of peers.values()) if (!p.blocked) n += p.unread;
    unread = n;
  }

  function enter() {
    hovering = true;
    clearTimeout(timer);
    timer = setTimeout(() => api.openPanel("hover"), delay);
  }
  function leave() {
    hovering = false;
    clearTimeout(timer);
  }

  onMount(() => {
    const un: Array<() => void> = [];
    (async () => {
      const s = await api.settings();
      edge = s.dock_edge;
      delay = s.hover_delay_ms;
      applyTheme(s.theme);
      for (const p of await api.peers()) peers.set(p.id, p);
      recount();
      un.push(
        await onCore((e) => {
          if (e.type === "peer") {
            peers.set(e.peer.id, e.peer);
            recount();
          } else if (e.type === "peer_removed") {
            peers.delete(e.peer_id);
            recount();
          } else if (e.type === "history_cleared") {
            for (const p of peers.values()) p.unread = 0;
            recount();
          } else if (e.type === "settings") {
            edge = e.settings.dock_edge;
            delay = e.settings.hover_delay_ms;
            applyTheme(e.settings.theme);
          }
        }),
      );
      // a file dragged onto the strip opens the panel immediately
      un.push(
        await getCurrentWebview().onDragDropEvent((ev) => {
          if (ev.payload.type === "enter" || ev.payload.type === "over") api.openPanel("drag");
        }),
      );
      un.push(await listen<boolean>("panel://visible", (e) => (open = e.payload)));
    })();
    return () => un.forEach((f) => f());
  });
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="strip {edge}"
  class:unread={unread > 0}
  class:open
  class:hovering
  onmouseenter={enter}
  onmouseleave={leave}
  ondragenter={() => api.openPanel("drag")}
>
  <div class="line"></div>
</div>

<style>
  .strip {
    position: fixed;
    inset: 0;
    display: grid;
    place-items: stretch;
    background: transparent;
  }
  .line {
    margin: 0;
    border-radius: 3px;
    background: rgba(128, 128, 140, 0.42);
    box-shadow: inset 0 0 0 0.5px rgba(255, 255, 255, 0.35);
    opacity: 0.75;
    transition: opacity 200ms ease, transform 260ms cubic-bezier(0.2, 0.9, 0.3, 1.3);
  }
  .right .line {
    transform-origin: right center;
  }
  .left .line {
    transform-origin: left center;
  }
  .top .line {
    transform-origin: center top;
  }
  .bottom .line {
    transform-origin: center bottom;
  }
  .hovering .line,
  .open .line {
    opacity: 1;
  }
  .right.hovering .line,
  .left.hovering .line {
    transform: scaleX(1.15) scaleY(1.04);
  }
  .top.hovering .line,
  .bottom.hovering .line {
    transform: scaleY(1.15) scaleX(1.04);
  }
  .unread .line {
    background: var(--accent);
    animation: pulse 1.8s ease-in-out infinite;
  }
  @keyframes pulse {
    0%,
    100% {
      opacity: 0.55;
    }
    50% {
      opacity: 1;
    }
  }
</style>
