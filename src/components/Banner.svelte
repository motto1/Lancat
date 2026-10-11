<script lang="ts">
  // "Other devices cannot connect to this one" (or the receive folder broke): one line under the
  // header with "去修复" (opens settings at 权限与诊断) and a close button. Closing hides it until
  // the next start or until it happens again (Rust keeps that state).
  import { diag } from "../lib/diag.svelte";
  import Icon from "./Icon.svelte";
</script>

{#if diag.alert}
  <div class="banner" role="alert">
    <span class="ico"><Icon name="alert" size={15} stroke={1.9} /></span>
    <div class="txt">
      <div class="t">{diag.alert.title}</div>
      {#if diag.alert.detail}<div class="d" title={diag.alert.detail}>{diag.alert.detail}</div>{/if}
    </div>
    <button class="go" onclick={() => diag.fix()}>去修复</button>
    <button class="icon-btn close" title="关闭提醒（再次发生或下次启动时会重新提醒）" onclick={() => diag.dismiss()}>
      <Icon name="x" size={13} />
    </button>
  </div>
{/if}

<style>
  .banner {
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 6px 8px 0;
    padding: 6px 6px 6px 9px;
    border-radius: 10px;
    background: color-mix(in srgb, #ff9f0a 14%, transparent);
    border: 0.5px solid color-mix(in srgb, #ff9f0a 45%, transparent);
    animation: banner-in 240ms var(--ease);
  }
  @keyframes banner-in {
    from {
      opacity: 0;
      transform: translateY(-4px);
    }
  }
  .ico {
    flex: none;
    display: grid;
    color: #e08600;
  }
  :global(:root[data-theme="dark"]) .ico {
    color: #ffb340;
  }
  @media (prefers-color-scheme: dark) {
    :global(:root:not([data-theme="light"])) .ico {
      color: #ffb340;
    }
  }
  .txt {
    flex: 1;
    min-width: 0;
    line-height: 1.3;
  }
  .t {
    font-size: 12px;
    font-weight: 600;
  }
  .d {
    font-size: 10.5px;
    color: var(--text-2);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .go {
    flex: none;
    padding: 3px 10px;
    border-radius: 6px;
    font-size: 11.5px;
    font-weight: 500;
    color: var(--accent-contrast);
    background: var(--accent);
    transition:
      transform var(--dur-fast) var(--ease),
      filter var(--dur-fast) var(--ease);
  }
  .go:hover {
    filter: brightness(1.08);
  }
  .go:active {
    transform: scale(0.96);
  }
  .close {
    flex: none;
    width: 22px;
    height: 22px;
  }
</style>
