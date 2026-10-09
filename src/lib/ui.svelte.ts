// Transient UI flags that influence the collapse logic.
class UiState {
  pinned = $state(false);
  composerFocused = $state(false);
  emojiOpen = $state(false);
  menuOpen = $state(false);
  dragging = $state(false);
  dropKey = $state<string | null>(null);
  dragPaths = $state<string[]>([]);
}

export const ui = new UiState();
