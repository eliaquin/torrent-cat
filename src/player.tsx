import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import { Playing, Snapshot, clock } from "./types";
import { Button, Icon } from "./ui";
import { createVideoClickHandler, handlePlayerEscape, resolvePlaybackEngine } from "./player-controls";
import { TrackMenu } from "./track-menu";
import { PlaybackTimeline } from "./playback-timeline";
import { FullName } from "./full-name";
import { usePlayerPresentation } from "./use-player-presentation";
type Track = { id: number; name: string };
type NativeStatus = {
  time: number;
  duration: number;
  state: number;
  audio: Track[];
  subtitles: Track[];
  audioTrack: number;
  subtitleTrack: number;
};
type Probe = {
  streams?: {
    index: number;
    codec_type: string;
    codec_name: string;
    tags?: { language?: string; title?: string };
    disposition?: { default?: number };
    channels?: number;
  }[];
  format?: { tags?: Record<string, string> };
};
export function Player({
  playing,
  data,
  report,
  onNext,
  onBack,
}: {
  playing: Playing;
  data: Snapshot;
  report: (e: unknown) => void;
  onNext: (delta: number) => void;
  onBack: () => void;
}) {
  const [url, setUrl] = useState(""),
    [engine, setEngine] = useState(() => resolvePlaybackEngine(data.profile.prefs.videoEngine, data.tools.vlc)),
    [message, setMessage] = useState("Preparing stream…"),
    [failed, setFailed] = useState(false),
    [paused, setPaused] = useState(false),
    [time, setTime] = useState(0),
    [duration, setDuration] = useState(0),
    [volume, setVolume] = useState(1),
    [rate, setRate] = useState(1),
    [subtitle, setSubtitle] = useState(""),
    [probe, setProbe] = useState<Probe>({}),
    [native, setNative] = useState<NativeStatus | null>(null),
    [working, setWorking] = useState(false),
    [audioTrack, setAudioTrack] = useState<number | null>(null),
    [audioMenuOpen, setAudioMenuOpen] = useState(false),
    [subtitleMenuOpen, setSubtitleMenuOpen] = useState(false),
    [subtitleTrack, setSubtitleTrack] = useState(-1),
    [subtitleFile, setSubtitleFile] = useState<{ url: string; name: string } | null>(null),
    [subtitleWorking, setSubtitleWorking] = useState(false);
  const presentation = usePlayerPresentation(
    paused, !!message || working || !url || engine === "external" || audioMenuOpen || subtitleMenuOpen || subtitleWorking,
    engine === "vlc" && !!url && native !== null && !failed, report,
  );
  const video = useRef<HTMLVideoElement>(null),
    surface = useRef<HTMLDivElement>(null),
    generation = useRef(0),
    session = useRef(0),
    position = useRef(0),
    lastSaved = useRef(0),
    ended = useRef(false),
    nativeReady = useRef(false),
    subtitleRequest = useRef(0);
  const torrent = data.torrents.find((t) => t.id === playing.id);
  const file = torrent?.files?.[playing.index];
  const complete =
    !!file &&
    (torrent?.stats?.file_progress[playing.index] || 0) >= file.length;
  const rect = () => {
    const r = surface.current!.getBoundingClientRect();
    return { x: r.x, y: r.y, width: r.width, height: r.height };
  };
  const control = async (action: string, value: number) => {
    const requestSession = session.current;
    try {
      const status = await invoke<NativeStatus>("native_action", { action, value });
      if (requestSession === session.current) setNative(status);
    } catch (e) {
      report(e);
    }
  };
  const persist = () => {
    void invoke("save_position", {
      key: `${playing.hash}:${playing.index}`,
      position: position.current,
    }).catch(report);
  };
  useEffect(() => {
    let active = true;
    const focus = () => {
      if (!active) return;
      void getCurrentWebview().setFocus().then(() => {
        if (active) surface.current?.focus({ preventScroll: true });
      }).catch(report);
    };
    // Tauri emits a native resize when the fullscreen animation finishes.
    // Restore the webview's first responder then, not during the transition.
    const unlisten = getCurrentWindow().onResized(focus);
    focus();
    return () => {
      active = false;
      void unlisten.then((stop) => stop()).catch(report);
    };
  }, [report]);
  useEffect(() => {
    const gen = ++generation.current;
    session.current++;
    ended.current = false;
    nativeReady.current = false;
    setUrl("");
    setProbe({});
    setAudioTrack(null);
    setWorking(false);
    setNative(null);
    setSubtitle("");
    setSubtitleTrack(-1);
    setSubtitleFile(null);
    setSubtitleWorking(false);
    subtitleRequest.current++;
    setMessage("Preparing stream…");
    setFailed(false);
    setPaused(false);
    setTime(0);
    setDuration(0);
    position.current =
      data.profile.positions[`${playing.hash}:${playing.index}`] || 0;
    void (async () => {
      try {
        await invoke("native_close");
        if (gen !== generation.current) return;
        const next = await invoke<string>("stream_file", {
          id: playing.id,
          index: playing.index,
        });
        if (gen !== generation.current) return;
        setUrl(next);
        setMessage("");
      } catch (e) {
        if (gen === generation.current) {
          setMessage(String(e));
          setFailed(true);
        }
      }
    })();
    return () => {
      generation.current++;
      persist();
      void invoke("native_close").catch(() => {});
    };
  }, [playing.id, playing.index]);
  useEffect(() => {
    if (!url) return;
    const gen = ++session.current;
    nativeReady.current = false;
    setNative(null);
    void (async () => {
      await invoke("native_close").catch(() => {});
      if (gen !== session.current) return;
      try {
        if (engine === "vlc") {
          await invoke("native_open", {
            url,
            rect: rect(),
            position: position.current,
          });
          if (gen === session.current) {
            nativeReady.current = true;
            setMessage("");
            setFailed(false);
            await control("volume", volume);
            await control("rate", rate);
          }
        } else if (engine === "external") {
          await invoke("external_player", { url });
          setMessage(
            "Playing in your external player. Keep this window open while streaming.",
          );
        }
      } catch (e) {
        if (gen === session.current) {
          setMessage(String(e));
          setFailed(true);
        }
      }
    })();
    const resize = () => {
      if (nativeReady.current)
        void invoke("native_resize", { rect: rect() }).catch(report);
    };
    const obs = new ResizeObserver(resize);
    if (surface.current) obs.observe(surface.current);
    window.addEventListener("resize", resize);
    return () => {
      session.current++;
      nativeReady.current = false;
      obs.disconnect();
      window.removeEventListener("resize", resize);
      void invoke("native_close").catch(() => {});
    };
  }, [url, engine]);
  useEffect(() => {
    if (!complete || !data.tools.ffprobe) return;
    const gen = generation.current;
    void invoke<Probe>("media_probe", { id: playing.id, index: playing.index })
      .then((v) => {
        if (gen === generation.current) setProbe(v);
      })
      .catch(() => {});
  }, [complete, playing.id, playing.index]);
  const advance = useRef(onNext);
  advance.current = onNext;
  useEffect(() => {
    let live = true;
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      if (engine === "vlc" && nativeReady.current) {
        try {
          const s = await invoke<NativeStatus>("native_action", {
            action: "status",
            value: 0,
          });
          if (live) {
            setNative(s);
            setTime(s.time);
            setDuration(s.duration);
            position.current = s.time;
            setPaused(s.state === 4 || s.state === 6 || s.state === 5);
            if (s.state === 7) {
              setMessage("VLC could not decode this media.");
              setFailed(true);
            }
            if (s.state === 6 && !ended.current) {
              ended.current = true;
              advance.current(1);
            }
          }
        } catch {}
      }
      if (live) timer = setTimeout(poll, 500);
    }
    void poll();
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [engine, playing.id, playing.index]);
  useEffect(() => {
    if (time - lastSaved.current > 5 || time < lastSaved.current - 2) {
      lastSaved.current = time;
      persist();
    }
  }, [time]);
  const toggle = () => {
    if (engine === "vlc") {
      if (native?.state === 6 || native?.state === 5) {
        ended.current = false;
        void control("replay", 0);
      } else void control("pause", paused ? 0 : 1);
      setPaused(!paused);
    } else if (video.current) {
      // A pending play() rejects with AbortError when a pause, reload or
      // unmount supersedes it; that is expected, not a playback failure.
      if (video.current.paused)
        void video.current.play().catch((e) => {
          if (!(e instanceof DOMException && e.name === "AbortError")) report(e);
        });
      else video.current.pause();
    }
  };
  const seek = (requested: number) => {
    const t = Math.max(0, Math.min(requested, duration || requested));
    position.current = t;
    setTime(t);
    if (engine === "vlc") void control("seek", t);
    else if (video.current) video.current.currentTime = t;
  };
  const escapePending = useRef(false);
  const keyHandler = useRef({ toggle, seek, time, onNext, onBack, presentation, report });
  keyHandler.current = { toggle, seek, time, onNext, onBack, presentation, report };
  const videoClick = useMemo(
    () => createVideoClickHandler(() => keyHandler.current.toggle()),
    [],
  );
  useEffect(() => () => videoClick.cancel(), [videoClick, playing.id, playing.index, engine, url]);
  useEffect(() => {
    const listener = (e: KeyboardEvent) => {
      const h = keyHandler.current;
      if (e.key === "Escape") {
        e.preventDefault();
        if (e.repeat || escapePending.current) return;
        escapePending.current = true;
        void handlePlayerEscape({
          fullscreen: h.presentation.fullscreen,
          isFullscreen: () => getCurrentWindow().isFullscreen(),
          exitFullscreen: () => h.presentation.changeFullscreen(false),
          back: h.onBack,
        }).catch(h.report).finally(() => { escapePending.current = false; });
        return;
      }
      if (
        ["INPUT", "SELECT", "TEXTAREA", "BUTTON"].includes(
          (e.target as HTMLElement).tagName,
        )
      )
        return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (e.key.toLowerCase() === "f") {
        e.preventDefault();
        void h.presentation.changeFullscreen();
      }
      if (e.code === "Space") {
        e.preventDefault();
        h.toggle();
      }
      if (e.key === "ArrowRight") {
        e.preventDefault();
        h.seek(h.time + 10);
      }
      if (e.key === "ArrowLeft") {
        e.preventDefault();
        h.seek(Math.max(0, h.time - 10));
      }
      if (e.key === "n") h.onNext(1);
      if (e.key === "p") h.onNext(-1);
    };
    document.addEventListener("keydown", listener);
    return () => document.removeEventListener("keydown", listener);
  }, []);
  const convert = async (audio?: number) => {
    setWorking(true);
    const gen = generation.current;
    try {
      const next = await invoke<string>("media_convert", {
        id: playing.id,
        index: playing.index,
        audio,
      });
      if (gen === generation.current) {
        setUrl(next);
        if (audio !== undefined) setAudioTrack(audio);
        setMessage("");
        setFailed(false);
      }
    } catch (e) {
      if (gen === generation.current) {
        setMessage(String(e));
        setFailed(true);
      }
    } finally {
      if (gen === generation.current) setWorking(false);
    }
  };
  const addSub = async () => {
    try {
      const path = await open({
        filters: [{ name: "Subtitles", extensions: ["srt", "vtt"] }],
      });
      if (typeof path === "string") {
        const gen = generation.current;
        const next = await invoke<string>("subtitle_file", { path });
        if (gen === generation.current) {
          if (engine === "vlc") {
            await invoke("native_subtitle", { url: next });
          } else {
            subtitleRequest.current++;
            setSubtitleFile({ url: next, name: path.split("/").pop() || "Subtitle file" });
            setSubtitle(next);
            setSubtitleTrack(-2);
          }
        }
      }
    } catch (e) {
      report(e);
    }
  };
  const selectSubtitle = async (id: number) => {
    const request = ++subtitleRequest.current;
    if (id < 0) {
      setSubtitle(id === -2 ? subtitleFile?.url || "" : "");
      setSubtitleTrack(id);
      return;
    }
    const gen = generation.current;
    setSubtitleWorking(true);
    try {
      const next = await invoke<string>("subtitle_embedded", {
        id: playing.id, index: playing.index, stream: id,
      });
      if (gen === generation.current && request === subtitleRequest.current) {
        setSubtitle(next);
        setSubtitleTrack(id);
      }
    } catch (error) {
      if (gen === generation.current && request === subtitleRequest.current) report(error);
    } finally {
      if (gen === generation.current && request === subtitleRequest.current) setSubtitleWorking(false);
    }
  };
  const audioStreams = probe.streams?.filter((stream) => stream.codec_type === "audio") || [];
  return (
    <section
      className={`player-page${presentation.hidden ? " controls-hidden" : ""}`}
      aria-label="Media player"
    >
      <div className="player-toolbar" {...presentation.chromeEvents}>
        {presentation.fullscreen && (
          <Button icon="arrow_back" title="Back to library" onClick={onBack} />
        )}
        <span className="player-filename">
          <FullName name={playing.name} />
        </span>
      </div>
      <div
        className="video-surface" ref={surface} tabIndex={0}
        aria-label="Video; click or press Space to play or pause; double-click or press F to toggle fullscreen"
        onClick={(event) => {
          if (url && !failed && (engine === "builtin" || nativeReady.current))
            videoClick.click(event.detail);
        }}
        onDoubleClick={() => {
          videoClick.cancel();
          void presentation.changeFullscreen();
        }}
      >
        {engine === "builtin" && url ? (
          <video
            key={url}
            ref={video}
            src={url}
            autoPlay
            playsInline
            crossOrigin="anonymous"
            onLoadedMetadata={() => {
              if (video.current) {
                setDuration(video.current.duration);
                if (position.current >= video.current.duration - 1)
                  position.current = 0;
                video.current.currentTime = position.current;
                video.current.volume = volume;
                video.current.playbackRate = rate;
                setMessage("");
              }
            }}
            onTimeUpdate={() => {
              const t = video.current?.currentTime || 0;
              setTime(t);
              position.current = t;
            }}
            onPlay={() => setPaused(false)}
            onPause={() => setPaused(true)}
            onDurationChange={() => setDuration(video.current?.duration || 0)}
            onWaiting={() => setMessage("Buffering…")}
            onPlaying={() => setMessage("")}
            onEnded={() => onNext(1)}
            onError={() => {
              setMessage(
                "This format cannot be played by the built-in player. Try Native VLC or prepare a compatible copy after the file finishes downloading.",
              );
              setFailed(true);
            }}
          >
            {subtitle && (
              <track
                key={subtitle}
                default
                kind="subtitles"
                src={subtitle}
                label="Subtitles"
              />
            )}
          </video>
        ) : engine === "external" ? (
          <div className="player-placeholder">
            <Icon name="open_in_new" />
            <p>External playback</p>
          </div>
        ) : !url ? (
          <div className="player-placeholder">
            <span className="spinner" />
          </div>
        ) : null}
      </div>
      <div className="player-bottom" {...presentation.chromeEvents}>
        {(message || working) && (
          <div
            className={"player-message " + (failed ? "has-error" : "")}
            role="status"
          >
            <span>{working ? "Preparing an MP4 playback copy…" : message}</span>
            {failed && engine !== "vlc" && data.tools.vlc && (
              <Button
                onClick={() => {
                  setEngine("vlc");
                  setFailed(false);
                }}
              >
                Use Native VLC
              </Button>
            )}
            {failed &&
              engine === "builtin" &&
              complete &&
              data.tools.ffmpeg && (
                <Button disabled={working} onClick={() => void convert()}>
                  Prepare compatible copy
                </Button>
              )}
          </div>
        )}
        <div className="seek-row">
          <span>{clock(time)}</span>
          <PlaybackTimeline
            key={`${playing.id}:${playing.index}`}
            id={playing.id}
            index={playing.index}
            time={time}
            duration={duration}
            onSeek={seek}
          />
          <span>{clock(duration)}</span>
        </div>
        <div className="player-controls">
          <div className="transport">
            <Button
              icon="skip_previous"
              title="Previous file"
              onClick={() => onNext(-1)}
            />
            <Button
              icon={paused ? "play_arrow" : "pause"}
              title={paused ? "Play" : "Pause"}
              onClick={toggle}
            />
            <Button
              icon="skip_next"
              title="Next file"
              onClick={() => onNext(1)}
            />
          </div>
          <Icon name="volume_up" />
          <input
            className="volume"
            type="range"
            aria-label="Volume"
            min={0}
            max={1}
            step={0.01}
            value={volume}
            onChange={(e) => {
              const v = Number(e.target.value);
              setVolume(v);
              if (engine === "vlc") void control("volume", v);
              else if (video.current) video.current.volume = v;
            }}
          />
          <select
            aria-label="Playback speed"
            value={rate}
            onChange={(e) => {
              const r = Number(e.target.value);
              setRate(r);
              if (engine === "vlc") void control("rate", r);
              else if (video.current) video.current.playbackRate = r;
            }}
          >
            {[0.25, 0.5, 0.75, 1, 1.25, 1.5, 1.75, 2, 4, 8, 16].map((r) => (
              <option key={r} value={r}>
                {r}×
              </option>
            ))}
          </select>
          <div className="player-spacer" />
          {engine === "vlc" && native?.audio.length ? (
            <TrackMenu
              label="Audio" icon="headset"
              key={`vlc:${playing.hash}:${playing.index}`}
              tracks={native.audio.map((track) => ({ ...track, name: track.id === -1 ? "Off" : track.name || `Track ${track.id}` }))}
              selected={native.audioTrack}
              onOpenChange={setAudioMenuOpen}
              onSelect={(id) => void control("audio", id)}
            />
          ) : null}
          {engine === "builtin" && complete && data.tools.ffmpeg && audioStreams.length > 0 ? (
            <TrackMenu
              label="Audio" icon="headset"
              key={`builtin:${playing.hash}:${playing.index}`}
              tracks={audioStreams.map((track, index) => ({
                id: track.index,
                name: track.tags?.title || audioLanguage(track.tags?.language) || `Track ${index + 1}`,
                detail: [track.tags?.title ? audioLanguage(track.tags?.language) : "",
                  track.codec_name.toUpperCase(), track.channels === 1 ? "Mono" : track.channels === 2 ? "Stereo" : track.channels ? `${track.channels} channels` : ""].filter(Boolean).join(" · "),
              }))}
              selected={audioTrack ?? audioStreams.find((track) => track.disposition?.default)?.index ?? audioStreams[0].index}
              disabled={working}
              onOpenChange={setAudioMenuOpen}
              onSelect={(id) => void convert(id)}
            />
          ) : null}
          {engine === "vlc" && (
            <TrackMenu
              key={`subtitles:vlc:${playing.hash}:${playing.index}`}
              label="Subtitles" icon="subtitles"
              tracks={[
                { id: -1, name: "Off" },
                ...(native?.subtitles || []).filter((track) => track.id !== -1).map((track) => ({
                  ...track, name: track.name || `Track ${track.id}`,
                })),
              ]}
              selected={native?.subtitleTrack ?? -1}
              onOpenChange={setSubtitleMenuOpen}
              onSelect={(id) => void control("subtitle", id)}
              onAddFile={() => void addSub()}
            />
          )}
          {engine === "builtin" && (
            <TrackMenu
              key={`subtitles:builtin:${playing.hash}:${playing.index}`}
              label="Subtitles" icon="subtitles"
              tracks={[
                { id: -1, name: "Off" },
                ...(subtitleFile ? [{ id: -2, name: subtitleFile.name, detail: "Subtitle file" }] : []),
                ...(probe.streams || []).filter((stream) => stream.codec_type === "subtitle").map((track, index) => ({
                  id: track.index,
                  name: track.tags?.title || audioLanguage(track.tags?.language) || `Track ${index + 1}`,
                  detail: track.codec_name.toUpperCase(),
                })),
              ]}
              selected={subtitleTrack}
              disabled={subtitleWorking}
              onOpenChange={setSubtitleMenuOpen}
              onSelect={(id) => void selectSubtitle(id)}
              onAddFile={() => void addSub()}
            />
          )}
          <Button
            icon={presentation.fullscreen ? "fullscreen_exit" : "fullscreen"}
            title={presentation.fullscreen ? "Exit full screen" : "Full screen"}
            aria-pressed={presentation.fullscreen}
            onClick={() => void presentation.changeFullscreen()}
          />
        </div>
      </div>
    </section>
  );
}

function audioLanguage(code?: string) {
  if (!code || code === "und") return "";
  try { return new Intl.DisplayNames([navigator.language], { type: "language" }).of(code) || code; }
  catch { return code; }
}
