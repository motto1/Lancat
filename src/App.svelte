<script lang="ts">
  import "./styles/global.css";
  import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
  import Panel from "./components/Panel.svelte";
  import Settings from "./components/Settings.svelte";
  import MobileApp from "./components/MobileApp.svelte";
  import { isAndroid } from "./lib/platform";

  const label = getCurrentWebviewWindow().label;
  document.documentElement.dataset.window = isAndroid ? "mobile" : label;
</script>

<!-- The dock strip is a native window now (src-tauri/src/native_dock); only panel and settings are web. -->
{#if isAndroid}
  <MobileApp />
{:else if label === "settings"}
  <Settings />
{:else}
  <Panel />
{/if}
