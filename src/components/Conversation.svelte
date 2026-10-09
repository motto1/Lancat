<script lang="ts">
  import { tick } from "svelte";
  import { formatTime, type Message } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import MessageItem from "./MessageItem.svelte";

  let scroller: HTMLElement;
  let stick = true;

  let list = $derived<Message[]>(app.current ? (app.messages[app.current] ?? []) : []);

  // time separators when the gap is > 5 min
  let rows = $derived.by(() => {
    const out: Array<{ sep?: string; m?: Message; first?: boolean }> = [];
    let prev: Message | null = null;
    for (const m of list) {
      if (!prev || m.ts - prev.ts > 5 * 60_000) out.push({ sep: formatTime(m.ts) });
      const first = !prev || prev.outgoing !== m.outgoing || m.ts - prev.ts > 60_000;
      out.push({ m, first });
      prev = m;
    }
    return out;
  });

  function onScroll() {
    stick = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 40;
    if (scroller.scrollTop < 30 && app.current) {
      const h = scroller.scrollHeight;
      app.loadOlder(app.current).then(async () => {
        await tick();
        scroller.scrollTop = scroller.scrollHeight - h;
      });
    }
  }

  // follow new messages / conversation switches
  let lastKey = "";
  $effect(() => {
    const key = `${app.current}:${list.length}:${list[list.length - 1]?.id ?? ""}`;
    const switched = !lastKey.startsWith(`${app.current}:`);
    if (key !== lastKey) {
      lastKey = key;
      const last = list[list.length - 1];
      if (switched || stick || last?.outgoing) {
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
    {#each rows as r (r.m?.id ?? `sep-${r.sep}`)}
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
