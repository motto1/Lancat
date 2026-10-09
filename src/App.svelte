<script lang="ts">
  import "./styles/global.css";
  import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
  import Dock from "./components/Dock.svelte";
  import Panel from "./components/Panel.svelte";
  import Settings from "./components/Settings.svelte";

  const label = getCurrentWebviewWindow().label;
  document.documentElement.dataset.window = label;
</script>

{#if label === "dock"}
  <Dock />
{:else if label === "settings"}
  <Settings />
{:else}
  <Panel />
{/if}
