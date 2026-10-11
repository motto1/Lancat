<script lang="ts">
  import { tick } from "svelte";
  import { formatTime, type Message } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import MessageItem from "./MessageItem.svelte";

  let scroller: HTMLElement;
  let stick = true;

  let list = $derived<Message[]>(app.current ? (app.messages[app.current] ?? []) : []);

  // time separators when the gap is > 5 min. Keyed by the message they precede: the label alone
  // ("10月11日 12:00") repeats across years and duplicate keys break the keyed each.
  let rows = $derived.by(() => {
    const out: Array<{ key: string; sep?: string; m?: Message; first?: boolean }> = [];
    let prev: Message | null = null;
    for (const m of list) {
      if (!prev || m.ts - prev.ts > 5 * 60_000) out.push({ key: `sep:${m.id}`, sep: formatTime(m.ts) });
      const first = !prev || prev.outgoing !== m.outgoing || m.ts - prev.ts > 60_000;
      out.push({ key: m.id, m, first });
      prev = m;
    }
    return out;
  });

  function onScroll() {
    stick = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 40;
    const peer = app.current;
    if (scroller.scrollTop < 30 && peer) {
      const h = scroller.scrollHeight;
      // loadOlder ignores re-entrant calls; only keep the position when it really prepended
      app.loadOlder(peer).then(async (added) => {
        if (!added || app.current !== peer) return;
        await tick();
        scroller.scrollTop += scroller.scrollHeight - h;
      });
    }
  }

  // follow new messages / conversation switches
  let lastKey = "";
  /** Switched to a conversation whose history was not loaded yet: the switch itself saw an empty
   *  list, so the bottom must still be forced when the history arrives (the old `stick` of the
   *  previous conversation decided otherwise and left it scrolled to the top). */
  let bottomPending = false;
  $effect(() => {
    const key = `${app.current}:${list.length}:${list[list.length - 1]?.id ?? ""}`;
    const switched = !lastKey.startsWith(`${app.current}:`);
    if (switched) bottomPending = true;
    if (key !== lastKey) {
      lastKey = key;
      const last = list[list.length - 1];
      if (bottomPending || stick || last?.outgoing) {
        if (list.length) bottomPending = false;
        stick = true;
        tick().then(() => scroller && (scroller.scrollTop = scroller.scrollHeight));
      }
    }
  });
</script>

<div class="scroll" bind:this={scroller} onscroll={onScroll}>
  {#if app.broadcast}
    <div class="hint">
      <div class="big">群发模式</div>
      <div>输入的消息、表情和拖进来的文件会发给全部 {app.onlinePeers.length} 台在线设备，并分别记入各自的会话。</div>
    </div>
  {:else if list.length === 0}
    <div class="hint">
      <div class="big">还没有消息</div>
      <div>把文件拖到这里或头像上即可发送，无需对方确认。</div>
    </div>
  {/if}
  {#if !app.broadcast}
    {#each rows as r (r.key)}
      {#if r.sep}
        <div class="sep">{r.sep}</div>
      {:else if r.m}
        <MessageItem m={r.m} first={!!r.first} />
      {/if}
    {/each}
  {/if}
</div>

<style>
  .scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 10px 10px 6px;
    display: flex;
    flex-direction: column;
  }
  .sep {
    align-self: center;
    margin: 10px 0 6px;
    font-size: 10.5px;
    color: var(--text-3);
    font-weight: 500;
  }
  .hint {
    margin: auto 18px;
    text-align: center;
    color: var(--text-3);
    font-size: 12px;
    line-height: 1.55;
  }
  .hint .big {
    font-size: 14px;
    font-weight: 600;
    color: var(--text-2);
    margin-bottom: 4px;
  }
</style>
