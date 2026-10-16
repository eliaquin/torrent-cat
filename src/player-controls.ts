export const CONTROL_IDLE_MS = 2000;

export function createVideoClickHandler(toggle: () => void) {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const cancel = () => {
    clearTimeout(timer);
    timer = undefined;
  };
  return {
    cancel,
    click(detail: number) {
      cancel();
      // Let the second click claim the gesture for fullscreen.
      if (detail > 1) return;
      timer = setTimeout(() => {
        timer = undefined;
        toggle();
      }, 300);
    },
  };
}

export function resolvePlaybackEngine(
  preferred: "builtin" | "vlc" | "external",
  vlcAvailable: boolean,
): "builtin" | "vlc" | "external" {
  return preferred === "vlc" && !vlcAvailable ? "builtin" : preferred;
}

export function shouldHideControls(state: {
  fullscreen: boolean;
  paused: boolean;
  blocked: boolean;
  hovered: boolean;
  focused: boolean;
  dragging: boolean;
  lastActivity: number;
  now: number;
}) {
  return state.fullscreen && !state.paused && !state.blocked &&
    !state.hovered && !state.focused && !state.dragging &&
    state.now - state.lastActivity >= CONTROL_IDLE_MS;
}
