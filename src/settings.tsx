import React, { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";
import {
  isPermissionGranted,
  requestPermission,
} from "@tauri-apps/plugin-notification";
import { Snapshot, Preferences } from "./types";
import { VlcRecommendation } from "./vlc-recommendation";
import { Button, Icon } from "./ui";
const categories = [
  ["general", "tune", "General"],
  ["folders", "folder_open", "Folders"],
  ["playback", "play_circle_outline", "Playback"],
  ["trackers", "wifi", "Trackers"],
] as const;
type Category = typeof categories[number][0];
type DefaultHandlers = { torrentFiles: boolean; magnetLinks: boolean };
export function Settings({
  data,
  refresh,
  report,
}: {
  data: Snapshot;
  refresh: () => Promise<void>;
  report: (e: unknown) => void;
}) {
  const [category, setCategory] = useState<Category>("general");
  const [prefs, setPrefs] = useState(data.profile.prefs),
    [startup, setStartup] = useState(false),
    [saving, setSaving] = useState(false);
  const [handlers, setHandlers] = useState<DefaultHandlers | null>(null);
  const [settingDefault, setSettingDefault] = useState(false);
  useEffect(() => {
    let active = true;
    const refreshDefaults = () => {
      void invoke<DefaultHandlers>("default_handlers")
        .then((value) => { if (active) setHandlers(value); })
        .catch((error) => { if (active) report(error); });
    };
    refreshDefaults();
    window.addEventListener("focus", refreshDefaults);
    return () => { active = false; window.removeEventListener("focus", refreshDefaults); };
  }, [report]);
  useEffect(() => {
    void isEnabled().then(setStartup).catch(report);
  }, []);
  const update = async (next: Preferences) => {
    setSaving(true);
    try {
      await invoke("preferences", { prefs: next });
      setPrefs(next);
      await refresh();
    } catch (e) {
      report(e);
    } finally {
      setSaving(false);
    }
  };
  const folder = async (key: "downloadPath" | "watchFolder") => {
    const path = await open({
      directory: true,
      defaultPath: prefs[key] || undefined,
    });
    if (typeof path === "string") await update({ ...prefs, [key]: path });
  };
  return (
    <section className="preferences-page">
      <aside className="preferences-sidebar">
        <nav className="preferences-index" role="tablist" aria-label="Preferences categories" aria-orientation="vertical">
          {categories.map(([id, icon, label], index) => (
            <button
              key={id}
              id={`preferences-tab-${id}`}
              role="tab"
              aria-selected={category === id}
              aria-controls={`settings-${id}`}
              tabIndex={category === id ? 0 : -1}
              onClick={() => setCategory(id)}
              onKeyDown={(event) => {
                const next = event.key === "ArrowDown" ? (index + 1) % categories.length
                  : event.key === "ArrowUp" ? (index + categories.length - 1) % categories.length
                  : event.key === "Home" ? 0 : event.key === "End" ? categories.length - 1 : null;
                if (next === null) return;
                event.preventDefault();
                setCategory(categories[next][0]);
                document.getElementById(`preferences-tab-${categories[next][0]}`)?.focus();
              }}
            >
              <Icon name={icon} />
              {label}
            </button>
          ))}
        </nav>
        <div className="preferences-footer">
          <span>TorrentCat {data.version}</span>
          <span role="status">{saving ? "Saving…" : "Changes save automatically"}</span>
        </div>
      </aside>
      <div className="preferences-detail">
        <Section
          id="folders"
          title="Folders"
          active={category}
        >
          <div className="preference preference-path">
            <label htmlFor="download-location">Download location</label>
            <div className="path-selector-controls">
              <input
                className="text-input"
                readOnly
                id="download-location"
                title={prefs.downloadPath}
                value={prefs.downloadPath}
              />
              <Button onClick={() => void folder("downloadPath")}>
                Change…
              </Button>
            </div>
            <p>
              New torrents use this location. Existing downloads stay where
              they are.
            </p>
          </div>
          <div className="preference">
            <label className="toggle-row">
              Automatically add torrents
              <input
                type="checkbox"
                checked={prefs.autoAddTorrents}
                onChange={(e) =>
                  void update({ ...prefs, autoAddTorrents: e.target.checked })
                }
              />
            </label>
            <div className="path-selector-controls">
              <input
                className="text-input"
                readOnly
                aria-label="Watched folder"
                title={prefs.watchFolder}
                placeholder="Choose a folder to watch"
                value={prefs.watchFolder}
              />
              <Button onClick={() => void folder("watchFolder")}>
                Choose…
              </Button>
            </div>
          </div>
        </Section>
        <Section
          id="playback"
          title="Playback"
          active={category}
        >
          <div className="preference">
            <label className="preference-select">
              Video player
              <select
                value={prefs.videoEngine}
                onChange={(e) =>
                  void update({
                    ...prefs,
                    videoEngine: e.target.value as Preferences["videoEngine"],
                  })
                }
              >
                <option value="vlc">VLC (recommended)</option>
                <option value="builtin">Built-in player</option>
                <option value="external">External player</option>
              </select>
            </label>
            <p>
              Used for the next file you open.{" "}
              {data.tools.vlc
                ? "VLC is ready. If it becomes unavailable, TorrentCat uses the built-in player."
                : "VLC is not installed. TorrentCat uses the built-in player until VLC is available."}
            </p>
          </div>
          {!data.tools.vlc && <div className="preference"><VlcRecommendation report={report} /></div>}
          <div className="preference preference-path">
            <label htmlFor="external-player">External application</label>
            <div className="path-selector-controls">
              <input
                className="text-input"
                readOnly
                id="external-player"
                title={prefs.externalPlayerPath}
                value={prefs.externalPlayerPath}
              />
              <Button
                onClick={async () => {
                  const path = await open({
                    defaultPath: "/Applications",
                    filters: [{ name: "Application", extensions: ["app"] }],
                  });
                  if (typeof path === "string")
                    void update({ ...prefs, externalPlayerPath: path });
                }}
              >
                Choose…
              </Button>
            </div>
          </div>
          <div className="preference">
            <label className="toggle-row">
              Prioritize streaming
              <input
                type="checkbox"
                checked={prefs.highestPlaybackPriority}
                onChange={(e) =>
                  void update({
                    ...prefs,
                    highestPlaybackPriority: e.target.checked,
                  })
                }
              />
            </label>
            <p>
              Pause other active transfers while streaming an unfinished
              torrent. They resume when you return to the library. Applies to
              the next file you open.
            </p>
          </div>
          <div className="preference">
            <strong>Media tools</strong>
            <p>
              {data.tools.ffmpeg && data.tools.ffprobe
                ? "FFmpeg and FFprobe are available for previews, subtitles and audio conversion."
                : "Install FFmpeg to enable previews, embedded subtitles and audio conversion."}
            </p>
            <p>
              Streaming requests automatically prioritize the pieces needed
              for playback.
            </p>
          </div>
        </Section>
        <Section
          id="general"
          title="General"
          active={category}
        >
          <div className="preference preference-default">
            <strong>Default torrent application</strong>
            <p>Open .torrent files and magnet links with TorrentCat.</p>
            <p role="status">
              {handlers
                ? `.torrent files: ${handlers.torrentFiles ? "TorrentCat" : "Not default"} · Magnet links: ${handlers.magnetLinks ? "TorrentCat" : "Not default"}`
                : "Checking macOS defaults…"}
            </p>
            <Button
              disabled={settingDefault || !!(handlers?.torrentFiles && handlers?.magnetLinks)}
              onClick={async () => {
                setSettingDefault(true);
                try {
                  setHandlers(await invoke<DefaultHandlers>("set_default_handlers"));
                } catch (error) {
                  report(error);
                } finally {
                  await invoke<DefaultHandlers>("default_handlers").then(setHandlers).catch(report);
                  setSettingDefault(false);
                }
              }}
            >
              {settingDefault ? "Updating…" : handlers?.torrentFiles && handlers?.magnetLinks ? "Default application" : "Make default"}
            </Button>
          </div>
          <div className="preference">
            <label className="toggle-row">
              Open TorrentCat at login
              <input
                type="checkbox"
                checked={startup}
                onChange={async (e) => {
                  try {
                    if (e.target.checked) await enable();
                    else await disable();
                    setStartup(await isEnabled());
                  } catch (error) {
                    report(error);
                  }
                }}
              />
            </label>
          </div>
          <div className="preference">
            <label className="toggle-row">
              Notification sounds
              <input
                type="checkbox"
                checked={prefs.soundNotifications}
                onChange={(e) =>
                  void update({
                    ...prefs,
                    soundNotifications: e.target.checked,
                  })
                }
              />
            </label>
          </div>
          <div className="preference">
            <label className="toggle-row">
              Download completion notifications
              <input
                type="checkbox"
                checked={prefs.completionNotifications}
                onChange={async (e) => {
                  const enabled = e.target.checked;
                  try {
                    if (
                      enabled &&
                      !(await isPermissionGranted()) &&
                      (await requestPermission()) !== "granted"
                    ) {
                      report(
                        "Allow notifications for TorrentCat in macOS System Settings to enable completion alerts.",
                      );
                      return;
                    }
                    await update({
                      ...prefs,
                      completionNotifications: enabled,
                    });
                  } catch (error) {
                    report(error);
                  }
                }}
              />
            </label>
            <p>Show a macOS notification when a new download finishes.</p>
          </div>
        </Section>
        <Section
          id="trackers"
          title="Trackers"
          active={category}
        >
          <div className="preference">
            <label htmlFor="global-trackers">Global trackers</label>
            <p>One tracker URL per line. Applied when adding new torrents.</p>
            <textarea
              id="global-trackers"
              aria-label="Global trackers"
              spellCheck={false}
              className="text-input"
              rows={5}
              defaultValue={prefs.globalTrackers.join("\n")}
              onBlur={(e) =>
                void update({
                  ...prefs,
                  globalTrackers: e.target.value
                    .split("\n")
                    .map((v) => v.trim())
                    .filter(Boolean),
                })
              }
            />
          </div>
        </Section>
      </div>
    </section>
  );
}
function Section({
  id,
  title,
  active,
  children,
}: {
  id: Category;
  title: string;
  active: Category;
  children: React.ReactNode;
}) {
  return (
    <section className="preferences-section" id={"settings-" + id}
      role="tabpanel" aria-labelledby={`preferences-tab-${id}`} hidden={active !== id}
      tabIndex={0}>
      <div className="preferences-section-heading">
        <h2>{title}</h2>
      </div>
      <div className="preferences-card">{children}</div>
    </section>
  );
}
export function Create({
  prefs,
  done,
  work,
}: {
  prefs: Preferences;
  done: () => void;
  work: (label: string, task: () => Promise<unknown>) => Promise<void>;
}) {
  const [path, setPath] = useState(""),
    [comment, setComment] = useState(""),
    [privateTorrent, setPrivate] = useState(false),
    [trackers, setTrackers] = useState(prefs.globalTrackers.join("\n"));
  return (
    <section className="create-page">
      <h1>Create torrent</h1>
      <p className="muted">
        Share a file or folder. Your original files stay in place.
      </p>
      <div className="preferences-card">
        <div className="preference">
          <strong>Files to share</strong>
          <div className="path-selector-controls">
            <input
              className="text-input"
              readOnly
              value={path}
              placeholder="Choose a file or folder"
            />
            <Button
              onClick={async () => {
                const p = await open({ directory: false });
                if (typeof p === "string") setPath(p);
              }}
            >
              File…
            </Button>
            <Button
              onClick={async () => {
                const p = await open({ directory: true });
                if (typeof p === "string") setPath(p);
              }}
            >
              Folder…
            </Button>
          </div>
        </div>
        <div className="preference">
          <label className="toggle-row">
            Private torrent
            <input
              type="checkbox"
              checked={privateTorrent}
              onChange={(e) => setPrivate(e.target.checked)}
            />
          </label>
        </div>
        <div className="preference">
          <label>
            Trackers
            <textarea
              className="text-input"
              rows={4}
              value={trackers}
              onChange={(e) => setTrackers(e.target.value)}
              placeholder="One tracker URL per line"
            />
          </label>
        </div>
        <div className="preference">
          <label>
            Comment
            <textarea
              className="text-input"
              rows={3}
              value={comment}
              onChange={(e) => setComment(e.target.value)}
              placeholder="Optionally describe your torrent…"
            />
          </label>
        </div>
      </div>
      <div className="dialog-actions">
        <Button onClick={done}>Cancel</Button>
        <Button
          className="shelf-primary"
          disabled={!path}
          onClick={async () => {
            const destination = await save({
              defaultPath: (path.split("/").pop() || "share") + ".torrent",
              filters: [{ name: "Torrent", extensions: ["torrent"] }],
            });
            if (destination)
              await work("Hashing files and creating torrent…", async () => {
                await invoke("create_torrent", {
                  path,
                  destination,
                  private: privateTorrent,
                  comment,
                  trackers: trackers
                    .split("\n")
                    .map((t) => t.trim())
                    .filter(Boolean),
                });
                done();
              });
          }}
        >
          Create and seed
        </Button>
      </div>
    </section>
  );
}
