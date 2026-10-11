// Transient UI flags that influence the collapse logic.
class UiState {
  pinned = $state(false);
  composerFocused = $state(false);
  emojiOpen = $state(false);
  menuOpen = $state(false);
  /** A native file dialog of the panel is open (it is not part of the page, so the pointer leaves). */
  dialogOpen = $state(false);
  /** The composer holds unsent text. */
  draft = $state(false);
  dragging = $state(false);
  dropKey = $state<string | null>(null);
  dragPaths = $state<string[]>([]);
}

export const ui = new UiState();
