<script lang="ts">
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import { api, type EmojiPayload } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import { ui } from "../lib/ui.svelte";
  import Icon from "./Icon.svelte";
  import { isAndroid } from "../lib/platform";
  import EmojiPanel from "./EmojiPanel.svelte";

  let text = $state("");
  let area: HTMLTextAreaElement;
  let attachMenu = $state(false);
  let lastTyping = 0;
  let typingPeer: string | null = null;

  let targets = $derived(app.targets());
  $effect(() => {
    ui.draft = text.trim().length > 0;
  });
  let disabled = $derived(targets.length === 0);
  let placeholder = $derived(
    disabled
      ? "没有可发送的对象"
      : targets.length > 1
        ? `发送给 ${targets.length} 台设备…`
        : `发给 ${app.peers.find((p) => p.id === targets[0])?.name ?? ""}…`,
  );

  // Growing with the text: `field-sizing: content` (Chromium / WebView2 123+) lets layout do it.
  // Elsewhere measure at most once per frame, and only shrink-measure ("height: auto" + read
  // scrollHeight, a forced synchronous reflow) when text was removed; typing used to force that
  // reflow on every keystroke.
  const nativeSizing = typeof CSS !== "undefined" && CSS.supports("field-sizing", "content");
  let sizeQueued = false;
  let lastLen = 0;
  function autosize() {
    if (nativeSizing || !area || sizeQueued) return;
    sizeQueued = true;
    requestAnimationFrame(() => {
      sizeQueued = false;
      const len = area.value.length;
      const shrunk = len < lastLen;
      lastLen = len;
      if (shrunk) area.style.height = "auto";
      else if (area.scrollHeight <= area.clientHeight) return; // still fits
      area.style.height = Math.min(area.scrollHeight, 96) + "px";
    });
  }

  function onInput() {
    ui.noteTyping();
    autosize();
    const t = targets.length === 1 ? targets[0] : null;
    if (t && Date.now() - lastTyping > 2500) {
      lastTyping = Date.now();
      typingPeer = t;
      api.typing(t, true).catch(() => {});
    }
  }

  function stopTyping() {
    if (typingPeer) api.typing(typingPeer, false).catch(() => {});
    typingPeer = null;
    lastTyping = 0;
  }

  async function send() {
    const t = text.trim();
    if (!t || disabled) return;
    text = "";
    queueMicrotask(autosize);
    stopTyping();
    try {
      await api.sendText(targets, t);
      if (targets.length > 1) app.flash(`已发给 ${targets.length} 台设备`);
    } catch (e) {
      app.flash(String(e));
    }
  }

  function onKey(e: KeyboardEvent) {
    // every key, Enter included: after an Enter send the panel stays open for the next message
    ui.noteTyping();
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      send();
    }
  }

  async function sendEmoji(emoji: EmojiPayload) {
    if (disabled) return;
    try {
      await api.sendEmoji(targets, emoji);
    } catch (e) {
      app.flash(String(e));
    }
  }

  async function attach(directory: boolean) {
    attachMenu = false;
    if (disabled) return;
    ui.dialogOpen = true; // native dialog: do not collapse underneath it
    try {
      // Android: the system picker hands out content URIs; the native side copies them to paths
      const sel = isAndroid ? await api.pickFiles() : await openDialog({ multiple: true, directory, title: directory ? "选择要发送的文件夹" : "选择要发送的文件" });
      const paths = sel == null ? [] : Array.isArray(sel) ? sel : [sel];
      if (paths.length) {
        await api.sendFiles(targets, paths as string[]);
        if (targets.length > 1) app.flash(`已发送给 ${targets.length} 台设备`);
      }
    } catch (e) {
      app.flash(String(e));
    } finally {
      ui.dialogOpen = false;
    }
  }
</script>

<svelte:window onclick={() => (attachMenu = false)} />

<div class="composer">
  {#if ui.emojiOpen}
    <EmojiPanel onpick={sendEmoji} onclose={() => (ui.emojiOpen = false)} />
  {/if}
  <button
    class="icon-btn"
    class:on={ui.emojiOpen}
    title="表情"
    {disabled}
    onclick={(e) => {
      e.stopPropagation();
      ui.emojiOpen = !ui.emojiOpen;
    }}
  >
    <Icon name="smile" size={18} />
  </button>
  <div class="attach-wrap">
    <button
      class="icon-btn"
      title="发送文件或文件夹"
      {disabled}
      onclick={(e) => {
        e.stopPropagation();
        attachMenu = !attachMenu;
      }}
    >
      <Icon name="clip" size={17} />
    </button>
    {#if attachMenu}
      <div class="attach-menu glass">
        <button onclick={() => attach(false)}><Icon name="doc" size={14} />文件…</button>
        <button onclick={() => attach(true)}><Icon name="folder" size={14} />文件夹…</button>
      </div>
    {/if}
  </div>
  <textarea
    bind:this={area}
    bind:value={text}
    rows="1"
    {placeholder}
    {disabled}
    oninput={onInput}
    onkeydown={onKey}
    onfocus={() => (ui.composerFocused = true)}
    onblur={() => {
      ui.composerFocused = false;
      ui.endTyping();
      stopTyping();
    }}
  ></textarea>
  <button class="send" class:ready={text.trim().length > 0} title="发送 (Enter)" onclick={send} {disabled}>
    <Icon name="send" size={14} stroke={2.2} />
  </button>
</div>

<style>
  .composer {
    position: relative;
    flex: none;
    display: flex;
    align-items: flex-end;
    gap: 2px;
    padding: 8px 8px 9px 6px;
    border-top: 0.5px solid var(--sep);
  }
  .icon-btn {
    width: 30px;
    height: 30px;
  }
  .icon-btn:disabled,
  textarea:disabled {
    opacity: 0.45;
  }
  textarea {
    flex: 1;
    min-width: 0;
    resize: none;
    border: 0.5px solid var(--glass-border);
    background: var(--field);
    border-radius: 15px;
    padding: 6px 11px;
    line-height: 17px;
    max-height: 96px;
    field-sizing: content;
    outline: none;
    font-size: 13px;
    transition: box-shadow var(--dur-fast) var(--ease);
  }
  textarea:focus {
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 35%, transparent);
  }
  textarea::placeholder {
    color: var(--text-3);
  }
  .send {
    width: 30px;
    height: 30px;
    margin-left: 4px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--accent);
    color: var(--accent-contrast);
    opacity: 0.35;
    transform: scale(0.88);
    transition:
      opacity var(--dur) var(--ease),
      transform var(--dur-slow) var(--ease-spring);
    will-change: transform;
  }
  .send.ready {
    opacity: 1;
    transform: scale(1);
  }
  .send:active {
    transform: scale(0.9);
  }
  .attach-wrap {
    position: relative;
  }
  .attach-menu {
    position: absolute;
    bottom: 36px;
    left: 0;
    z-index: 20;
    min-width: 120px;
    padding: 4px;
    border-radius: 9px;
    display: flex;
    flex-direction: column;
    background: var(--glass-strong);
    transform-origin: bottom left;
    animation: pop-in var(--dur) var(--ease-spring);
  }
  .attach-menu button {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 6px 8px;
    border-radius: 6px;
    font-size: 12.5px;
  }
  .attach-menu button:hover {
    background: var(--accent);
    color: var(--accent-contrast);
  }
  @keyframes pop-in {
    from {
      opacity: 0;
      transform: scale(0.9);
    }
  }
</style>
