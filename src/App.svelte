<script lang="ts">
  import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
  const label = getCurrentWebviewWindow().label;
</script>

<main>Lancat · {label}</main>
