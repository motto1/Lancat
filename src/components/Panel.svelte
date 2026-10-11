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
  import { posterLoads, setLottieActive } from "../lib/lottie";
  import { isWindows } from "../lib/platform";
  import Sidebar from "./Sidebar.svelte";
  import Conversation from "./Conversation.svelte";
  import Composer from "./Composer.svelte";
  import Header from "./Header.svelte";
  import DropPicker from "./DropPicker.svelte";
  import { liquid } from "../lib/liquid";
  import Empty from "./Empty.svelte";
  import Banner from "./Banner.svelte";
  import { diag } from "../lib/diag.svelte";

  let shell: HTMLElement;
  let edge = $state("right");
  let anim: Animation | null = null;
  let phase: "hidden" | "opening" | "open" | "closing" = "hidden";
  let firstShow = true;
  const afterFrames = (n: number, f: () => void) => requestAnimationFrame(() => (n > 1 ? afterFrames(n - 1, f) : f()));
  const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

  /** Resolves once a frame with content has been *presented*: paint timing stamps
   *  first-contentful-paint with the compositor's presentation feedback and reports it only then.
   *  Immediately if that already happened. */
  function contentPresented(): Promise<void> {
    return new Promise((res) => {
      if (performance.getEntriesByName("first-contentful-paint").length) return res();
      try {
        const po = new PerformanceObserver((l) => {
          if (l.getEntriesByName("first-contentful-paint").length) {
            po.disconnect();
            res();
          }
        });
        po.observe({ type: "paint", buffered: true });
      } catch {
        res();
      }
    });
  }

  /** Windows, first show of a cold page: the window is already shown (alpha 0) but WebView2 starts
   *  producing frames for it only now, and a rAF count is no measure of what reached the screen
   *  (the first frames are presented with a delay: the reveal then faded in a background-only
   *  window, then text without the sticker canvases). So reveal only after (1) a frame with
   *  content was presented, (2) the sticker posters are in their canvases, (3) three more frames
   *  (the first presented frame can still lack the bubbles and canvases). Measured on a Win11
   *  laptop, cold: content presented 110–150 ms after show() with and without --disable-gpu,
   *  where the old fixed 2 rAFs revealed 4–6 frames of bare background first.
   *  Bounded, and Rust reveals anyway after 4 s. */
  async function revealWhenPainted() {
    const t0 = performance.now();
    await Promise.race([contentPresented(), sleep(1500)]);
    const t1 = performance.now();
    await Promise.race([Promise.allSettled([...posterLoads]), sleep(300)]);
    const t2 = performance.now();
    afterFrames(3, () => {
      uiLog(`panel first reveal: presented +${Math.round(t1 - t0)} ms, posters +${Math.round(t2 - t1)} ms, total ${Math.round(performance.now() - t0)} ms`);
      api.panelReady().catch(() => {});
    });
  }
  let pointerInside = true;
  /** Opened from the tray/shortcut: only auto-collapse after the pointer has visited the panel. */
  let entered = true;
  let collapseTimer: ReturnType<typeof setTimeout> | undefined;

  /** What keeps the panel open after the pointer left. Only things that would lose work or that
   *  live outside the page: pinned, a native file dialog, a drag, a half-typed message. Popovers
   *  (emoji panel, context menu), a merely focused composer and running transfers used to count
   *  too; they kept the panel open indefinitely, since nothing released them unless the user went
   *  back in and closed them. They are closed by the collapse instead.
   *  A focused composer holds while it has a draft, or for a few seconds after the last keystroke
   *  (ui.recentlyTyped): Enter sends and empties the draft while the pointer is usually outside,
   *  which used to collapse the panel in the middle of a conversation. An idle, empty, focused
   *  composer still does not hold. */
  function holding(): boolean {
    return ui.pinned || ui.dialogOpen || (ui.composerFocused && (ui.draft || ui.recentlyTyped));
  }
  function blocked(): boolean {
    return holding() || ui.dragging;
  }

  function show(from: string) {
    clearTimeout(collapseTimer);
    // opened from the message toast: go to that conversation
    if (from.startsWith("toast:")) {
      const id = from.slice(6);
      if (app.peers.some((p) => p.id === id)) app.open(id);
    }
    if (phase === "hidden" || phase === "closing") entered = from === "hover" || from === "drag";
    app.visible = true;
    setLottieActive(true);
    if (phase === "open" || phase === "opening") return;
    anim?.cancel();
    phase = "opening";
    // Windows: the whole window (native Acrylic included) fades in, so the page must not slide or
    // fade inside a material that is already there; only a slight settle in scale.
    // a compositor layer for the shell only while it animates (it used to keep one permanently)
    shell.style.willChange = "transform, opacity";
    anim = isWindows
      ? shell.animate([{ transform: "scale(0.985)", opacity: 1 }, { transform: "none", opacity: 1 }], { duration: 160, easing: "cubic-bezier(0.2, 0.8, 0.3, 1)", fill: "both" })
      : springTo(shell, { ...edgeOffset(edge, 36), scale: 0.94, opacity: 0 }, { x: 0, y: 0, scale: 1, opacity: 1 }, { stiffness: 300, damping: 24 });
    uiLog(`panel show (${from}), visibility=${document.visibilityState}`);
    requestAnimationFrame(() => api.panelShown().catch(() => {}));
    // Windows keeps the window transparent until a frame of this open has been painted. Warm (the
    // parked renderer kept producing frames): two rAFs, the first runs before that frame, the
    // second after it was produced. Cold: see revealWhenPainted.
    if (isWindows) {
      if (firstShow) void revealWhenPainted();
      else afterFrames(2, () => api.panelReady().catch(() => {}));
    }
    firstShow = false;
    anim.finished
      .then(() => {
        uiLog("panel open animation done");
        if (phase === "opening") phase = "open";
        shell.style.willChange = "";
      })
      .catch(() => {});
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
      shell.style.willChange = "transform, opacity";
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
    ui.menuOpen = false;
    ui.endTyping();
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
    api.setHold(holding()).catch(() => {});
  });

  // Collapsed: pause every CSS animation (infinite ones like the empty-state ping or a spinning
  // refresh icon kept the parked renderer producing frames while nothing was on screen).
  $effect(() => {
    document.documentElement.classList.toggle("lc-hidden", !app.visible);
  });

  // ---- drag hit testing: `over` events come with every pointer move; elementFromPoint forces a
  // style/layout flush (mid-animation, under the picker's blur). Resolve at most once per frame,
  // and only when the position changed.
  let dragPos: { x: number; y: number } | null = null;
  let dragLast = "";
  let dragFrame = 0;
  function queueHitTest(x: number, y: number) {
    dragPos = { x, y };
    if (dragFrame) return;
    dragFrame = requestAnimationFrame(() => {
      dragFrame = 0;
      if (!dragPos || !ui.dragging) return;
      const k = `${Math.round(dragPos.x)},${Math.round(dragPos.y)}`;
      if (k === dragLast) return;
      dragLast = k;
      const key = hitTest(dragPos.x, dragPos.y);
      if (key !== ui.dropKey) ui.dropKey = key;
    });
  }
  function endHitTest() {
    cancelAnimationFrame(dragFrame);
    dragFrame = 0;
    dragPos = null;
    dragLast = "";
  }

  onMount(() => {
    performance.mark("mount");
    const un: Array<() => void> = [];
    // A cold panel gets its first-frame state injected by Rust (window.__LANCAT_BOOT__): start the
    // open animation right away instead of after several IPC round-trips.
    const boot = window.__LANCAT_BOOT__;
    app.init(boot).then(() => performance.mark("init"));
    diag.init();
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
        listen<string>("panel://edge", (e) => (edge = e.payload)),
        // the mouse button went up: a drag that never reached (or already left) the page is over
        listen("panel://button-up", () => {
          setTimeout(() => {
            if (!ui.dragging) return;
            uiLog("drag ended outside the page");
            ui.dragging = false;
            ui.dropKey = null;
            if (!pointerInside && phase !== "hidden") scheduleCollapse();
          }, 300);
        }),
        // files released on the dock strip itself go to the current conversation
        listen<string[]>("panel://strip-drop", (e) => {
          ui.dragging = false;
          ui.dropKey = null;
          dropFiles("conversation", e.payload);
        }),
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
            if (!ui.dragging) ui.dragging = true;
            pointerInside = true;
            clearTimeout(collapseTimer);
            const pos = p.position;
            queueHitTest(pos.x / devicePixelRatio, pos.y / devicePixelRatio);
            if (p.type === "enter") ui.dragPaths = p.paths;
          } else if (p.type === "drop") {
            endHitTest();
            const key = hitTest(p.position.x / devicePixelRatio, p.position.y / devicePixelRatio);
            ui.dragging = false;
            ui.dropKey = null;
            dropFiles(key, p.paths);
          } else {
            endHitTest();
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
      ui.endTyping();
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
    uiLog(`drop: ${paths.length} item(s) on ${key ?? "nothing"}`);
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
    <Banner />
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
    {#if ui.dragging}
      <DropPicker />
    {/if}
    {#if app.toast}
      <div class="toast" use:liquid={{ radius: 12, depth: 12, blur: 5 }}>{app.toast}</div>
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
  }
  .shell.dragging > :global(header),
  .shell.dragging > .body {
    transform: scale(0.97);
    transition: transform 320ms cubic-bezier(0.2, 0.9, 0.3, 1);
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
    background: rgba(0, 0, 0, 0.42);
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.28),
      0 6px 18px rgba(0, 0, 0, 0.18);
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
