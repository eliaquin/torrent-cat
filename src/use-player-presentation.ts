import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { shouldHideControls } from "./player-controls";

export function usePlayerPresentation(
  paused: boolean,
  blocked: boolean,
  native: boolean,
  report: (error: unknown) => void,
) {
  const [fullscreen, setFullscreen] = useState(false);
  const [hidden, setHidden] = useState(false);
  const interaction = useRef({
    hovered: false, focused: false, dragging: false, lastActivity: Date.now(),
  });
  const state = useRef({ fullscreen, paused, blocked });
  state.current = { fullscreen, paused, blocked };
  const transitioning = useRef(false);
  const show = useCallback(() => {
    interaction.current.lastActivity = Date.now();
    setHidden(false);
  }, []);
  const setInteraction = (field: "hovered" | "focused", value: boolean) => {
    interaction.current[field] = value;
    show();
  };

  useEffect(() => {
    let alive = true;
    let pending = false;
    const sync = async () => {
      if (pending) return;
      pending = true;
      try {
        const actual = await getCurrentWindow().isFullscreen();
        if (alive) setFullscreen(actual);
      } catch (error) {
        if (alive) report(error);
      } finally {
        pending = false;
      }
    };
    // Also observes the native green button, View menu and macOS Escape exit.
    void sync();
    const timer = window.setInterval(() => void sync(), 500);
    return () => { alive = false; window.clearInterval(timer); };
  }, [report]);

  useEffect(() => {
    document.documentElement.classList.toggle("player-fullscreen", fullscreen);
    show();
    return () => document.documentElement.classList.remove("player-fullscreen");
  }, [fullscreen, show]);
  useEffect(() => {
    document.documentElement.classList.toggle("native-playback", native);
    return () => document.documentElement.classList.remove("native-playback");
  }, [native]);

  useEffect(() => {
    const move = () => show();
    const down = (event: PointerEvent) => {
      interaction.current.dragging = !!(event.target as Element)?.closest(
        ".player-toolbar, .player-bottom",
      );
      show();
    };
    const up = () => { interaction.current.dragging = false; show(); };
    document.addEventListener("pointermove", move);
    document.addEventListener("pointerdown", down);
    document.addEventListener("pointerup", up);
    document.addEventListener("pointercancel", up);
    document.addEventListener("keydown", show);
    window.addEventListener("blur", up);
    const timer = window.setInterval(() => {
      setHidden(shouldHideControls({ ...state.current, ...interaction.current, now: Date.now() }));
    }, 100);
    return () => {
      document.removeEventListener("pointermove", move);
      document.removeEventListener("pointerdown", down);
      document.removeEventListener("pointerup", up);
      document.removeEventListener("pointercancel", up);
      document.removeEventListener("keydown", show);
      window.removeEventListener("blur", up);
      window.clearInterval(timer);
    };
  }, [show]);

  const changeFullscreen = async (value?: boolean) => {
    if (transitioning.current) return;
    transitioning.current = true;
    show();
    try {
      const window = getCurrentWindow();
      await window.setFullscreen(value ?? !(await window.isFullscreen()));
    } catch (error) {
      report(error);
    } finally {
      transitioning.current = false;
    }
  };

  return {
    fullscreen,
    hidden: fullscreen && hidden && !paused && !blocked,
    changeFullscreen,
    show,
    chromeEvents: {
      onPointerEnter: () => setInteraction("hovered", true),
      onPointerLeave: () => setInteraction("hovered", false),
      onFocusCapture: (event: React.FocusEvent<HTMLElement>) =>
        setInteraction("focused", event.target.matches(":focus-visible")),
      onBlurCapture: () => setInteraction("focused", false),
    },
  };
}
