// Transient UI flags that influence the collapse logic.

/** How long the composer counts as "recently typed in" after the last key or input. */
const TYPING_IDLE_MS = 8000;

class UiState {
  pinned = $state(false);
  composerFocused = $state(false);
  emojiOpen = $state(false);
  menuOpen = $state(false);
  /** A native file dialog of the panel is open (it is not part of the page, so the pointer leaves). */
  dialogOpen = $state(false);
  /** The composer holds unsent text. */
  draft = $state(false);
  /** A key was pressed / text was entered in the composer less than TYPING_IDLE_MS ago. Keeps a
   *  focused composer holding the panel right after Enter sent the message (the draft is empty
   *  then), so the user can go on typing; an idle focused composer still lets it collapse. */
  recentlyTyped = $state(false);
  dragging = $state(false);
  dropKey = $state<string | null>(null);
  dragPaths = $state<string[]>([]);

  #typingTimer: ReturnType<typeof setTimeout> | undefined;

  /** Composer key or input: (re)start the idle timer. */
  noteTyping() {
    clearTimeout(this.#typingTimer);
    if (!this.recentlyTyped) this.recentlyTyped = true;
    this.#typingTimer = setTimeout(() => {
      this.#typingTimer = undefined;
      this.recentlyTyped = false;
    }, TYPING_IDLE_MS);
  }

  /** Composer blurred, window lost focus or the panel collapsed. */
  endTyping() {
    clearTimeout(this.#typingTimer);
    this.#typingTimer = undefined;
    this.recentlyTyped = false;
  }
}

export const ui = new UiState();
