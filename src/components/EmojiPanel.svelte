<script lang="ts">
  import type { EmojiPayload } from "../lib/api";
  let { onpick, onclose }: { onpick: (e: EmojiPayload) => void; onclose: () => void } = $props();
  const basic = ["😀", "😂", "🥰", "😎", "🤔", "👍", "🙏", "🎉", "❤️", "🔥"];
</script>

<div class="panel">
  {#each basic as c}
    <button onclick={() => (onpick({ kind: "noto", code: c, alt: c }), onclose())}>{c}</button>
  {/each}
</div>

<style>
  .panel {
    position: absolute;
    bottom: 48px;
    left: 6px;
  }
</style>
