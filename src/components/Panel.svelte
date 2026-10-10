<script lang="ts">
  // The chat popover: spring in from the docked edge, collapse 800 ms after the pointer leaves
  // (unless the composer has focus, the emoji panel / a menu is open, a transfer runs, or it is pinned).
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { api, uiLog } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import { edgeOffset, exitTo, springTo } from "../lib/spring";
  import { ui } from "../lib/ui.svelte";
  import { setLottieActive } from "../lib/lottie";
  import { isWindows } from "../lib/platform";
  import Sidebar from "./Sidebar.svelte";
  import Conversation from "./Conversation.svelte";
  import Composer from "./Composer.svelte";
  import Header from "./Header.svelte";
  import Empty from "./Empty.svelte";

  let shell: HTMLElement;
  let edge = $state("right");
  let anim: Animation | null = null;
  let phase: "hidden" | "opening" | "open" | "closing" = "hidden";
  let firstShow = true;
  const afterFrames = (n: number, f: () => void) => requestAnimationFrame(() => (n > 1 ? afterFrames(n - 1, f) : f()));
  let pointerInside = true;
  /** Opened from the tray/shortcut: only auto-collapse after the pointer has visited the panel. */
  let entered = true;
  let collapseTimer: ReturnType<typeof setTimeout> | undefined;

  function blocked(): boolean {
    return ui.pinned || ui.composerFocused || ui.emojiOpen || ui.menuOpen || ui.dragging || app.transferring;
  }

  function show(from: string) {
    clearTimeout(collapseTimer);
    if (phase === "hidden" || phase === "closing") entered = from === "hover" || from === "drag";
    app.visible = true;
    setLottieActive(true);
    if (phase === "open" || phase === "opening") return;
    anim?.cancel();
    phase = "opening";
    // Windows: the whole window (native Acrylic included) fades in, so the page must not slide or
    // fade inside a material that is already there; only a slight settle in scale.
    anim = isWindows
      ? shell.animate([{ transform: "scale(0.985)", opacity: 1 }, { transform: "none", opacity: 1 }], { duration: 160, easing: "cubic-bezier(0.2, 0.8, 0.3, 1)", fill: "both" })
      : springTo(shell, { ...edgeOffset(edge, 36), scale: 0.94, opacity: 0 }, { x: 0, y: 0, scale: 1, opacity: 1 }, { stiffness: 300, damping: 24 });
    uiLog(`panel show (${from}), visibility=${document.visibilityState}`);
    requestAnimationFrame(() => api.panelShown().catch(() => {}));
    // Windows keeps the window transparent until a frame of this open has been painted
    // (two rAFs: the first runs before that frame, the second after it was produced). On the first
    // show of a cold page the compositor only starts now: the first presented frames held just the
    // background, then the text without the sticker canvases, so wait a few frames more there.
    if (isWindows) afterFrames(firstShow ? 5 : 2, () => api.panelReady().catch(() => {}));
    firstShow = false;
    anim.finished.then(() => (uiLog("panel open animation done"), phase === "opening" && (phase = "open"))).catch(() => {});
    app.markReadIfVisible();
    if (from === "drag") ui.dragging = true;
  }

  async function collapse(force = false) {
    uiLog(`collapse(force=${force}) phase=${phase} blocked=${blocked()}`);
    if (phase === "hidden" || phase === "closing") return;
    if (!force && blocked()) return;
    phase = "closing";
    // Windows: Rust fades the whole window out; the page stays as it is (its last frame is also
    // what a parked panel shows first when it comes back).
    if (!isWindows) {
      anim?.cancel();
      anim = exitTo(shell, { x: 0, y: 0, scale: 1, opacity: 1 }, { ...edgeOffset(edge, 28), scale: 0.96, opacity: 0 }, 220);
      try {
        await anim.finished;
      } catch {
        return; // cancelled by a re-open
      }
      if (phase !== "closing") return;
    }
    phase = "hidden";
    app.visible = false;
    setLottieActive(false); // nothing animates while hidden
    ui.emojiOpen = false;
    await api.hidePanel();
  }

  function scheduleCollapse() {
    clearTimeout(collapseTimer);
    const delay = app.settings?.collapse_delay_ms ?? 800;
    const tick = () => {
      if (pointerInside) return;
      if (blocked()) {
        collapseTimer = setTimeout(tick, 400); // re-check until free
        return;
      }
      collapse();
    };
    collapseTimer = setTimeout(tick, delay);
  }

  function pointer(inside: boolean) {
    if (inside) entered = true;
    if (!inside && !entered) return;
    if (inside === pointerInside) return;
    pointerInside = inside;
    if (inside) clearTimeout(collapseTimer);
    else scheduleCollapse();
  }

  // the Rust side tells us when anything blocks collapsing for a while (e.g. native dialog open)
  $effect(() => {
    api.setHold(ui.pinned || ui.composerFocused || ui.emojiOpen || ui.menuOpen || app.transferring).catch(() => {});
  });

  onMount(() => {
    performance.mark("mount");
    const un: Array<() => void> = [];
    // A cold panel gets its first-frame state injected by Rust (window.__LANCAT_BOOT__): start the
    // open animation right away instead of after several IPC round-trips.
    const boot = window.__LANCAT_BOOT__;
    app.init(boot).then(() => performance.mark("init"));
    if (boot?.panel) {
      ui.pinned = boot.panel.pinned;
      if (boot.panel.visible) {
        edge = boot.panel.edge;
        show(boot.panel.reason || "tray");
      }
    }
    (async () => {
      // register all listeners concurrently (each one is an IPC round-trip)
      const ls = await Promise.all([
        listen<{ edge: string; reason: string }>("panel://show", (e) => {
          edge = e.payload.edge;
          pointerInside = true;
          show(e.payload.reason);
        }),
        listen<{ inside: boolean }>("panel://cursor", (e) => pointer(e.payload.inside)),
        listen("panel://collapse", () => collapse(true)),
        listen<boolean>("panel://pinned", (e) => (ui.pinned = e.payload)),
        // Rust hid the window without our collapse (fallback after an unanswered collapse request):
        // resync so the next panel://show plays the open again.
        listen<boolean>("panel://visible", (e) => {
          if (e.payload || phase === "hidden") return;
          uiLog(`hidden by the shell while phase=${phase}`);
          clearTimeout(collapseTimer);
          anim?.cancel();
          phase = "hidden";
          app.visible = false;
          setLottieActive(false);
          ui.emojiOpen = false;
        }),
      ]);
      un.push(...ls);
      // the window may have been shown before this webview listened (pre-warmed, then opened)
      api
        .panelState()
        .then((st) => {
          performance.mark("state");
          ui.pinned = st.pinned;
          // the panel webview is created on demand, so the first show usually happens right here
          if (st.visible && phase === "hidden") {
            edge = st.edge;
            show(st.reason || "tray");
          } else if (isWindows && !st.visible && phase !== "hidden") {
            // hidden by the shell while this page was still loading (e.g. --hide-panel right after
            // a cold open): the boot state said visible, so resync before the next panel://show
            uiLog(`hidden before the page was ready (phase=${phase})`);
            anim?.cancel();
            phase = "hidden";
            app.visible = false;
            setLottieActive(false);
          }
        })
        .catch(() => {});
      un.push(
        await getCurrentWebview().onDragDropEvent((ev) => {
          const p = ev.payload;
          if (p.type === "enter" || p.type === "over") {
            ui.dragging = true;
            pointerInside = true;
            clearTimeout(collapseTimer);
            const pos = p.position;
            ui.dropKey = hitTest(pos.x / devicePixelRatio, pos.y / devicePixelRatio);
            if (p.type === "enter") ui.dragPaths = p.paths;
          } else if (p.type === "drop") {
            const key = hitTest(p.position.x / devicePixelRatio, p.position.y / devicePixelRatio);
            ui.dragging = false;
            ui.dropKey = null;
            dropFiles(key, p.paths);
          } else {
            ui.dragging = false;
            ui.dropKey = null;
          }
        }),
      );
    })();
    const onLeave = (e: MouseEvent) => {
      if (!e.relatedTarget) pointer(false);
    };
    const onEnter = () => pointer(true);
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !ui.emojiOpen && !ui.menuOpen) collapse(true);
    };
    // Keyboard focus moved to another app (e.g. the user clicked elsewhere and started typing).
    // WebKitGTK on a layer-shell surface does not always blur the textarea, which left
    // composerFocused stuck at true and blocked collapsing forever. Losing window focus always
    // releases the composer hold; if the pointer is already outside, collapse as usual.
    const onWinBlur = () => {
      uiLog(`window blur, pointerInside=${pointerInside}`);
      ui.composerFocused = false;
      (document.activeElement as HTMLElement | null)?.blur?.();
      if (!pointerInside && phase !== "hidden") scheduleCollapse();
    };
    window.addEventListener("blur", onWinBlur);
    getCurrentWindow()
      .onFocusChanged(({ payload }) => {
        if (!payload) onWinBlur();
      })
      .then((f) => un.push(f))
      .catch(() => {});
    // Pointer moving inside the page proves it is inside (covers a missed mouseenter).
    const onMove = () => {
      if (!pointerInside) pointer(true);
    };
    document.addEventListener("mousemove", onMove, { passive: true });
    document.documentElement.addEventListener("mouseleave", onLeave);
    document.documentElement.addEventListener("mouseenter", onEnter);
    window.addEventListener("keydown", onKey);
    return () => {
      un.forEach((f) => f());
      document.documentElement.removeEventListener("mouseleave", onLeave);
      document.documentElement.removeEventListener("mouseenter", onEnter);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("blur", onWinBlur);
      document.removeEventListener("mousemove", onMove);
    };
  });

  /** Which drop zone is under (x, y): "peer:<id>" | "all" | "conversation" | null */
  function hitTest(x: number, y: number): string | null {
    const el = document.elementFromPoint(x, y) as HTMLElement | null;
    const zone = el?.closest<HTMLElement>("[data-drop]");
    return zone?.dataset.drop ?? null;
  }

  async function dropFiles(key: string | null, paths: string[]) {
    if (!paths.length || !key) return;
    let targets: string[] = [];
    if (key === "all") targets = app.onlinePeers.map((p) => p.id);
    else if (key.startsWith("peer:")) {
      const id = key.slice(5);
      targets = app.selected.includes(id) && app.selected.length > 1 ? [...app.selected] : [id];
    } else if (key === "conversation") targets = app.targets();
    if (!targets.length) {
      app.flash("没有可发送的在线设备");
      return;
    }
    try {
      await api.sendFiles(targets, paths);
      const names = targets.map((t) => app.peers.find((p) => p.id === t)?.name ?? "?");
      app.flash(`已发送 ${paths.length} 项给 ${names.length > 2 ? `${names.length} 台设备` : names.join("、")}`);
      if (targets.length === 1 && key !== "conversation") app.open(targets[0]);
    } catch (e) {
      app.flash(String(e));
    }
  }
</script>

<div class="frame">
  <div class="shell glass" bind:this={shell} class:dragging={ui.dragging}>
    <Header />
    <div class="body">
      <Sidebar />
      <section class="main" data-drop="conversation" class:drop-hot={ui.dropKey === "conversation"}>
        {#if app.peers.length === 0}
          <Empty />
        {:else}
          <Conversation />
          <Composer />
        {/if}
      </section>
    </div>
    {#if app.toast}
      <div class="toast">{app.toast}</div>
    {/if}
  </div>
</div>

<style>
  .frame {
    position: fixed;
    inset: 0;
    padding: 8px;
  }
  :global(.native-glass) .frame {
    padding: 0;
  }
  :global(.native-glass) .shell {
    box-shadow: none;
    border: none;
  }
  .shell {
    position: relative;
    width: 100%;
    height: 100%;
    border-radius: var(--radius);
    overflow: hidden;
    display: flex;
    flex-direction: column;
    opacity: 0;
    will-change: transform, opacity;
  }
  .body {
    flex: 1;
    display: flex;
    min-height: 0;
  }
  .main {
    position: relative;
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .main::after {
    content: "";
    position: absolute;
    inset: 6px;
    border-radius: 12px;
    border: 2px dashed var(--accent);
    opacity: 0;
    transform: scale(0.98);
    transition:
      opacity var(--dur) var(--ease),
      transform var(--dur) var(--ease);
    pointer-events: none;
  }
  .main.drop-hot::after {
    opacity: 0.85;
    transform: scale(1);
  }
  .toast {
    position: absolute;
    left: 50%;
    bottom: 64px;
    transform: translateX(-50%);
    max-width: 80%;
    padding: 7px 12px;
    border-radius: 10px;
    background: rgba(0, 0, 0, 0.72);
    color: #fff;
    font-size: 12px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    animation: toast-in 260ms cubic-bezier(0.2, 0.9, 0.3, 1.2);
    pointer-events: none;
  }
  @keyframes toast-in {
    from {
      opacity: 0;
      transform: translate(-50%, 8px) scale(0.96);
    }
  }
</style>
