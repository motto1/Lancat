<script lang="ts">
  import type { EmojiPayload } from "../lib/api";
  let { emoji, size = 160 }: { emoji: EmojiPayload; size?: number } = $props();
</script>

<div class="fallback" style:font-size="{Math.round(size * 0.7)}px" style:width="{size}px" style:height="{size}px">{emoji.alt}</div>

<style>
  .fallback {
    display: grid;
    place-items: center;
    line-height: 1;
  }
</style>
