import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import { Playing, Snapshot, clock, bytes } from "./types";
import { Button, Icon } from "./ui";
type Track = { id: number; name: string };
type NativeStatus = {
  time: number;
  duration: number;
  state: number;
  audio: Track[];
  subtitles: Track[];
};
type Probe = {
  streams?: {
    index: number;
    codec_type: string;
    codec_name: string;
    tags?: { language?: string; title?: string };
  }[];
  format?: { tags?: Record<string, string> };
};
export function Player({
  playing,
  data,
  report,
  onNext,
}: {
  playing: Playing;
  data: Snapshot;
  report: (e: unknown) => void;
  onNext: (delta: number) => void;
}) {
  const [url, setUrl] = useState(""),
    [engine, setEngine] = useState(data.profile.prefs.videoEngine),
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
    [working, setWorking] = useState(false);
  const video = useRef<HTMLVideoElement>(null),
    surface = useRef<HTMLDivElement>(null),
    generation = useRef(0),
    session = useRef(0),
    position = useRef(0),
    lastSaved = useRef(0),
    ended = useRef(false),
    nativeReady = useRef(false);
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
    try {
      await invoke("native_action", { action, value });
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
    const gen = ++generation.current;
    session.current++;
    ended.current = false;
    nativeReady.current = false;
    setUrl("");
    setProbe({});
    setNative(null);
    setSubtitle("");
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
      if (video.current.paused) void video.current.play().catch(report);
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
  const keyHandler = useRef({ toggle, seek, time, onNext });
  keyHandler.current = { toggle, seek, time, onNext };
  useEffect(() => {
    const listener = (e: KeyboardEvent) => {
      if (
        ["INPUT", "SELECT", "TEXTAREA", "BUTTON"].includes(
          (e.target as HTMLElement).tagName,
        )
      )
        return;
      const h = keyHandler.current;
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
          } else setSubtitle(next);
        }
      }
    } catch (e) {
      report(e);
    }
  };
  return (
    <section className="player-page" aria-label="Media player">
      <div className="player-toolbar">
        <span className="player-filename" title={playing.name}>
          {playing.name.split("/").pop()}
        </span>
        <select
          aria-label="Playback engine"
          value={engine}
          onChange={(e) => {
            setEngine(e.target.value as typeof engine);
            setFailed(false);
            setMessage("");
          }}
        >
          <option value="builtin">Built-in player</option>
          <option value="vlc" disabled={!data.tools.vlc}>
            Native VLC
          </option>
          <option value="external">External player</option>
        </select>
      </div>
      <div className="video-surface" ref={surface}>
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
      <div className="player-bottom">
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
          <input
            type="range"
            aria-label="Playback position"
            min={0}
            max={Number.isFinite(duration) && duration > 0 ? duration : 1}
            step="0.1"
            value={Math.min(
              time,
              Number.isFinite(duration) && duration > 0 ? duration : 1,
            )}
            onChange={(e) => seek(Number(e.target.value))}
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
            <select
              aria-label="Audio track"
              defaultValue=""
              onChange={(e) => void control("audio", Number(e.target.value))}
            >
              <option value="" disabled>
                Audio track
              </option>
              {native.audio.map((t) => (
                <option key={t.id} value={t.id}>
                  {t.name || `Track ${t.id}`}
                </option>
              ))}
            </select>
          ) : null}
          {engine === "builtin" &&
          complete &&
          probe.streams?.some((s) => s.codec_type === "audio") ? (
            <select
              aria-label="Prepare audio track"
              defaultValue=""
              disabled={working}
              onChange={(e) => void convert(Number(e.target.value))}
            >
              <option value="" disabled>
                Audio track…
              </option>
              {probe.streams
                .filter((s) => s.codec_type === "audio")
                .map((t) => (
                  <option key={t.index} value={t.index}>
                    {t.tags?.title || t.tags?.language || `Track ${t.index}`} (
                    {t.codec_name})
                  </option>
                ))}
            </select>
          ) : null}
          {engine === "vlc" && native?.subtitles.length ? (
            <select
              aria-label="Subtitle track"
              defaultValue=""
              onChange={(e) => void control("subtitle", Number(e.target.value))}
            >
              <option value="" disabled>
                Subtitles
              </option>
              {native.subtitles.map((t) => (
                <option key={t.id} value={t.id}>
                  {t.name || `Track ${t.id}`}
                </option>
              ))}
            </select>
          ) : null}
          {engine === "builtin" &&
          probe.streams?.some((s) => s.codec_type === "subtitle") ? (
            <select
              aria-label="Embedded subtitles"
              defaultValue=""
              onChange={async (e) => {
                if (e.target.value === "off") {
                  setSubtitle("");
                  return;
                }
                const gen = generation.current;
                try {
                  const u = await invoke<string>("subtitle_embedded", {
                    id: playing.id,
                    index: playing.index,
                    stream: Number(e.target.value),
                  });
                  if (gen === generation.current) setSubtitle(u);
                } catch (error) {
                  report(error);
                }
              }}
            >
              <option value="" disabled>
                Subtitles
              </option>
              <option value="off">Off</option>
              {probe.streams
                .filter((s) => s.codec_type === "subtitle")
                .map((t) => (
                  <option key={t.index} value={t.index}>
                    {t.tags?.title || t.tags?.language || `Track ${t.index}`}
                  </option>
                ))}
            </select>
          ) : null}
          <Button
            icon="subtitles"
            title="Add subtitle file"
            onClick={() => void addSub()}
          />
          <Button
            icon="fullscreen"
            title="Full screen"
            onClick={async () => {
              const w = getCurrentWindow();
              await w.setFullscreen(!(await w.isFullscreen()));
            }}
          />
        </div>
        <div className="download-strip">
          <span>
            {torrent?.stats?.finished
              ? "Download complete"
              : `${bytes(torrent?.stats?.progress_bytes || 0)} of ${bytes(torrent?.stats?.total_bytes || 0)} downloaded`}
          </span>
          <progress
            max={torrent?.stats?.total_bytes || 1}
            value={torrent?.stats?.progress_bytes || 0}
          />
        </div>
      </div>
    </section>
  );
}
