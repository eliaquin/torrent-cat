import React, { useEffect, useState, useRef, useCallback } from "react";
import { createRoot } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open, save } from "@tauri-apps/plugin-dialog";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import { Settings, Create } from "./settings";
import { Button, Icon, Dialog } from "./ui";
import {
  Snapshot,
  Torrent,
  Playing,
  bytes,
  status,
  playable,
  isVideo,
} from "./types";
import { VlcRecommendation } from "./vlc-recommendation";
import { Player } from "./player";
import "./style.css";
import { playSound } from "./sound";
const pickTorrent = () =>
  open({
    multiple: true,
    filters: [{ name: "Torrent", extensions: ["torrent"] }],
  });
function App() {
  const [data, setData] = useState<Snapshot | null>(null),
    [page, setPage] = useState("home"),
    [playing, setPlaying] = useState<Playing | null>(null);
  const [query, setQuery] = useState(""),
    [filter, setFilter] = useState("all"),
    [expanded, setExpanded] = useState<number | null>(null),
    [more, setMore] = useState<number | null>(null);
  const [modal, setModal] = useState<"add" | "import" | null>(null),
    [address, setAddress] = useState(""),
    [notice, setNotice] = useState(""),
    [busy, setBusy] = useState(""),
    [remove, setRemove] = useState<Torrent | null>(null),
    [deleteData, setDeleteData] = useState(false);
  const [posters, setPosters] = useState<Record<string, string>>({});
  const [completion, setCompletion] = useState<{ name: string } | null>(null);
  useEffect(() => {
    if (!completion) return;
    const timer = window.setTimeout(() => setCompletion(null), 2500);
    return () => window.clearTimeout(timer);
  }, [completion]);
  const postersRequested = useRef(new Set<string>());
  const current = useRef({ data, page, playing });
  current.current = { data, page, playing };
  const refresh = useCallback(async () => {
    setData(await invoke<Snapshot>("snapshot"));
  }, []);
  const chime = (name: Parameters<typeof playSound>[0]) =>
    playSound(
      name,
      current.current.data?.profile.prefs.soundNotifications ?? false,
    );
  const report = useCallback((error: unknown) => {
    setNotice(String(error));
    chime("error");
  }, []);
  const work = async (label: string, task: () => Promise<unknown>) => {
    setBusy(label);
    try {
      await task();
      await refresh();
    } catch (e) {
      report(e);
    } finally {
      setBusy("");
    }
  };
  const add = async (a: string) => {
    if (!a.trim()) return;
    setModal(null);
    setAddress("");
    await work("Adding torrent…", async () => {
      await invoke("add_torrent", { address: a });
      chime("add");
    });
  };
  const files = async () => {
    const paths = await pickTorrent();
    if (paths)
      for (const p of Array.isArray(paths) ? paths : [paths]) await add(p);
  };
  const back = async () => {
    await getCurrentWindow().setFullscreen(false).catch(report);
    await invoke("native_close").catch(() => {});
    await invoke("stop_stream").catch(report);
    setPlaying(null);
    setPage("home");
    postersRequested.current.clear();
    setPosters({});
  };
  const play = (t: Torrent, index?: number) => {
    const i = index ?? t.files?.findIndex((f) => playable(f.name)) ?? -1;
    if (i < 0) {
      report("No supported media file found in this torrent.");
      return;
    }
    setPlaying({
      id: t.id,
      index: i,
      hash: t.info_hash,
      name: t.files![i].name,
    });
    chime("play");
    setPage("player");
    setMore(null);
  };
  useEffect(() => {
    let alive = true;
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const d = await invoke<Snapshot>("snapshot");
        if (alive) setData(d);
      } catch (e) {
        if (alive) report(e);
      }
      if (alive) timer = setTimeout(poll, 1000);
    }
    poll();
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [report]);
  const actions = useRef({ add, files, back, work });
  actions.current = { add, files, back, work };
  useEffect(() => {
    const cleanups: Promise<() => void>[] = [];
    cleanups.push(
      listen<string>("download-complete", ({ payload }) => {
        setCompletion({ name: payload });
        if (!current.current.playing) chime("done");
      }),
    );
    cleanups.push(
      listen<string>("menu", async ({ payload }) => {
        if (payload === "add") setModal("add");
        if (payload === "open") await actions.current.files();
        if (payload === "create") {
          await actions.current.back();
          setPage("create");
        }
        if (payload === "preferences") {
          await actions.current.back();
          setPage("preferences");
        }
        if (payload === "library") await actions.current.back();
        if (payload === "pause-all" || payload === "resume-all")
          await actions.current.work("Updating transfers…", async () => {
            for (const t of current.current.data?.torrents || [])
              await invoke("torrent_action", {
                id: t.id,
                action: payload === "pause-all" ? "pause" : "resume",
              });
          });
      }),
    );
    cleanups.push(
      listen<string>("open-input", ({ payload }) => {
        void actions.current.add(payload);
      }),
    );
    cleanups.push(
      listen<string>("notice", ({ payload }) => setNotice(payload)),
    );
    cleanups.push(
      getCurrentWindow().onDragDropEvent(({ payload }) => {
        if (payload.type === "drop") {
          for (const path of payload.paths.filter((p) =>
            p.toLowerCase().endsWith(".torrent"),
          ))
            void actions.current.add(path);
        }
      }),
    );
    void Promise.all(cleanups)
      .then(() => invoke<string[]>("take_inputs"))
      .then((inputs) => inputs.forEach((a) => void actions.current.add(a)));
    return () => {
      cleanups.forEach((p) => void p.then((fn) => fn()));
    };
  }, []);
  useEffect(() => {
    const fn = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement;
      if (["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName)) return;
      if (e.key === "Escape") {
        // The player owns Escape: fullscreen first, then back to the library.
        if (current.current.page === "player") return;
        setModal(null);
        setMore(null);
        if (current.current.page !== "home") void actions.current.back();
      }
      if ((e.metaKey || e.ctrlKey) && e.key === "v") {
        e.preventDefault();
        void readText()
          .then((a) => actions.current.add(a))
          .catch(report);
      }
    };
    document.addEventListener("keydown", fn);
    return () => document.removeEventListener("keydown", fn);
  }, [report]);
  useEffect(() => {
    if (!data || page === "player" || !data.tools.ffmpeg) return;
    for (const t of data.torrents) {
      const index =
        t.files?.findIndex(
          (f, i) =>
            isVideo(f.name) && (t.stats?.file_progress[i] || 0) >= f.length,
        ) ?? -1;
      if (index < 0 || postersRequested.current.has(t.info_hash)) continue;
      postersRequested.current.add(t.info_hash);
      void invoke<string>("media_poster", { id: t.id, index })
        .then((url) => setPosters((p) => ({ ...p, [t.info_hash]: url })))
        .catch(() => {});
    }
  }, [data, page]);
  const exportTorrent = async (t: Torrent) => {
    const path = await save({
      defaultPath: `${t.name}.torrent`,
      filters: [{ name: "Torrent", extensions: ["torrent"] }],
    });
    if (path)
      await work("Saving torrent…", () =>
        invoke("export_torrent", { id: t.id, path }),
      );
  };
  const visible =
    data?.torrents.filter(
      (t) =>
        (filter === "all" ||
          status(t) === filter ||
          (filter === "downloading" && status(t) === "checking")) &&
        t.name?.toLocaleLowerCase().includes(query.toLocaleLowerCase()),
    ) || [];
  return (
    <div className={`app view-${page} is-darwin is-focused`}>
      <header className="header" data-tauri-drag-region>
        <nav className="app-navigation" aria-label="Main navigation">
          {["home", "preferences"].includes(page) ? (
            <>
              <button
                aria-current={page === "home" ? "page" : undefined}
                onClick={() => setPage("home")}
              >
                <Icon name="video_library" />
                Library
              </button>
              <button
                aria-current={page === "preferences" ? "page" : undefined}
                onClick={() => setPage("preferences")}
              >
                <Icon name="settings" />
                Preferences
              </button>
            </>
          ) : (
            <button onClick={() => void back()}>
              <Icon name="arrow_back" />
              Back to library
            </button>
          )}
        </nav>
        <div className="title" data-tauri-drag-region>
          {page === "player"
            ? playing?.name
            : page === "create"
              ? "Create torrent"
              : ""}
        </div>
      </header>
      {notice && (
        <div className="notice" role="alert">
          <span>{notice}</span>
          <button aria-label="Dismiss message" onClick={() => setNotice("")}>
            ×
          </button>
        </div>
      )}
      {busy && (
        <div className="activity" role="status">
          <span className="spinner" />
          {busy}
        </div>
      )}
      {completion && (
        <div className="completion-toast" role="status">
          <Icon name="check_circle" />
          <div><strong>Download complete</strong><span>{completion.name}</span></div>
          <button aria-label="Dismiss download notification" onClick={() => setCompletion(null)}>
            <Icon name="close" />
          </button>
        </div>
      )}
      <main className="content">
        {!data ? (
          <div className="shelf-empty">
            <span className="spinner" />
            <h2>Opening your library…</h2>
          </div>
        ) : page === "home" ? (
          <section
            className="torrent-list media-shelf"
            aria-label="Torrent library"
          >
            <div className="shelf-heading">
              <div>
                <h1>Watch something</h1>
                <p>Your media, ready when you are.</p>
              </div>
              <Button
                icon="add"
                className="shelf-primary"
                onClick={() => setModal("add")}
              >
                Add torrent
              </Button>
            </div>
            {!data.tools.vlc && <VlcRecommendation report={report} />}
            <div className="shelf-toolbar">
              <div
                className="shelf-filters"
                role="group"
                aria-label="Filter torrents"
              >
                {["all", "downloading", "seeding", "paused"].map((f) => (
                  <button
                    key={f}
                    aria-pressed={filter === f}
                    onClick={() => setFilter(f)}
                  >
                    {f[0].toUpperCase() + f.slice(1)}
                  </button>
                ))}
              </div>
              <label className="shelf-search">
                <Icon name="search" />
                <input
                  aria-label="Search library"
                  placeholder="Search library"
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                />
              </label>
            </div>
            {!data.downloadPathExists && (
              <div className="shelf-warning">
                <strong>Download folder unavailable</strong>
                <p>Connect your drive or choose a new download location.</p>
                <Button onClick={() => setPage("preferences")}>
                  Choose download folder
                </Button>
              </div>
            )}
            <div className="shelf-grid">
              {visible.map((t) => {
                const st = status(t),
                  progress = t.stats?.total_bytes
                    ? Math.min(
                        100,
                        (t.stats.progress_bytes / t.stats.total_bytes) * 100,
                      )
                    : 0,
                  selected = expanded === t.id;
                return (
                  <article
                    key={t.info_hash}
                    className={"torrent " + (selected ? "selected" : "")}
                  >
                    <button
                      className="torrent-cover"
                      aria-label={"Show files for " + t.name}
                      aria-expanded={selected}
                      onClick={() => setExpanded(selected ? null : t.id)}
                    >
                      {posters[t.info_hash] ? (
                        <img src={posters[t.info_hash]} alt="" />
                      ) : (
                        <Icon
                          name={
                            t.files?.some((f) => isVideo(f.name))
                              ? "movie"
                              : "folder"
                          }
                        />
                      )}
                    </button>
                    <div className="torrent-body">
                      <h2 className="name" title={t.name}>
                        {t.name || "Getting torrent metadata…"}
                      </h2>
                      <div
                        className={
                          "torrent-status " +
                          (t.stats?.error ? "has-error" : "")
                        }
                      >
                        <strong>{st[0].toUpperCase() + st.slice(1)}</strong>
                        <span>{progress.toFixed(1)}%</span>
                        <span>{bytes(t.stats?.total_bytes || 0)}</span>
                      </div>
                      <progress
                        className="ui-progress"
                        aria-label="Download progress"
                        value={progress}
                        max={100}
                      />
                      <div className="torrent-transfer">
                        {t.stats?.error || (
                          <>
                            <span>
                              ↓{" "}
                              {bytes(
                                (t.stats?.live?.download_speed?.mbps || 0) *
                                  1024 *
                                  1024,
                              )}
                              /s
                            </span>{" "}
                            ·{" "}
                            <span>
                              ↑{" "}
                              {bytes(
                                (t.stats?.live?.upload_speed?.mbps || 0) *
                                  1024 *
                                  1024,
                              )}
                              /s
                            </span>
                            <br />
                            {bytes(t.stats?.progress_bytes || 0)} downloaded
                          </>
                        )}
                      </div>
                      <div className="torrent-controls">
                        <Button
                          icon="play_arrow"
                          disabled={!t.files?.some((f) => playable(f.name))}
                          onClick={() => play(t)}
                        >
                          Stream
                        </Button>
                        <Button
                          icon={st === "paused" ? "play_arrow" : "pause"}
                          title={
                            st === "paused"
                              ? "Resume transfer"
                              : "Pause transfer"
                          }
                          onClick={() =>
                            void work("Updating transfer…", () =>
                              invoke("torrent_action", {
                                id: t.id,
                                action: st === "paused" ? "resume" : "pause",
                              }),
                            )
                          }
                        />
                        <Button
                          icon="list"
                          title="Show files"
                          aria-expanded={selected}
                          onClick={() => setExpanded(selected ? null : t.id)}
                        />
                        <div className="more-wrap">
                          <Button
                            icon="more_horiz"
                            title="More actions"
                            onClick={() => setMore(more === t.id ? null : t.id)}
                          />
                          {more === t.id && (
                            <div className="action-menu">
                              <button
                                onClick={() => {
                                  setMore(null);
                                  void invoke("reveal_torrent", {
                                    id: t.id,
                                  }).catch(report);
                                }}
                              >
                                Show in Finder
                              </button>
                              <button
                                onClick={() => {
                                  setMore(null);
                                  void writeText(
                                    `magnet:?xt=urn:btih:${t.info_hash}&dn=${encodeURIComponent(t.name)}`,
                                  ).catch(report);
                                }}
                              >
                                Copy magnet link
                              </button>
                              <button
                                onClick={() => {
                                  setMore(null);
                                  void exportTorrent(t);
                                }}
                              >
                                Save torrent file…
                              </button>
                              <button
                                className="danger"
                                onClick={() => {
                                  setMore(null);
                                  setDeleteData(false);
                                  setRemove(t);
                                }}
                              >
                                Remove…
                              </button>
                            </div>
                          )}
                        </div>
                      </div>
                    </div>
                    {selected && (
                      <div className="torrent-details">
                        <table>
                          <tbody>
                            {t.files?.map((f, i) => (
                              <tr key={i}>
                                <td className="col-icon">
                                  <Icon
                                    name={
                                      playable(f.name)
                                        ? "play_arrow"
                                        : "insert_drive_file"
                                    }
                                  />
                                </td>
                                <td className="col-name">
                                  <button
                                    className="file-open"
                                    title={f.name}
                                    disabled={!playable(f.name)}
                                    onClick={() => play(t, i)}
                                  >
                                    {f.name}
                                  </button>
                                </td>
                                <td className="col-progress">
                                  {Math.min(
                                    100,
                                    Math.round(
                                      ((t.stats?.file_progress[i] || 0) /
                                        Math.max(1, f.length)) *
                                        100,
                                    ),
                                  )}
                                  %
                                </td>
                                <td className="col-size">{bytes(f.length)}</td>
                                <td className="col-select">
                                  <input
                                    type="checkbox"
                                    aria-label={"Download " + f.name}
                                    checked={f.included}
                                    onChange={() =>
                                      void work("Updating selection…", () =>
                                        invoke("select_files", {
                                          id: t.id,
                                          files: t.files!.flatMap((file, j) =>
                                            (
                                              j === i
                                                ? !file.included
                                                : file.included
                                            )
                                              ? [j]
                                              : [],
                                          ),
                                        }),
                                      )
                                    }
                                  />
                                </td>
                              </tr>
                            ))}
                          </tbody>
                        </table>
                      </div>
                    )}
                  </article>
                );
              })}
            </div>
            {!visible.length && (
              <div className="shelf-empty">
                <Icon
                  name={data.torrents.length ? "search" : "video_library"}
                />
                <h2>
                  {data.torrents.length
                    ? "No matching torrents"
                    : "Your shelf is ready"}
                </h2>
                <p>
                  {data.torrents.length
                    ? "Try another search or status filter."
                    : "Add a torrent file or magnet link to start streaming."}
                </p>
                {data.torrents.length ? (
                  <Button
                    onClick={() => {
                      setFilter("all");
                      setQuery("");
                    }}
                  >
                    Clear filters
                  </Button>
                ) : (
                  <div className="empty-actions">
                    <Button icon="add" onClick={() => setModal("add")}>
                      Add your first torrent
                    </Button>
                    <Button
                      icon="folder_open"
                      onClick={() => setModal("import")}
                    >
                      Import existing library
                    </Button>
                  </div>
                )}
              </div>
            )}
            <div className="torrent-placeholder">
              Drop a torrent file anywhere or paste a magnet link
            </div>
            <footer className="shelf-footer">
              <span>
                {data.torrents.length}{" "}
                {data.torrents.length === 1 ? "torrent" : "torrents"} in your
                library
              </span>
              <button onClick={() => setPage("create")}>Create torrent</button>
            </footer>
          </section>
        ) : page === "preferences" ? (
          <Settings
            data={data}
            refresh={refresh}
            report={report}
          />
        ) : page === "create" ? (
          <Create
            prefs={data.profile.prefs}
            done={() => setPage("home")}
            work={work}
          />
        ) : playing ? (
          <Player
            onBack={() => void back()}
            playing={playing}
            data={data}
            report={report}
            onNext={(delta) => {
              const t = data.torrents.find((t) => t.id === playing.id);
              if (!t) return;
              const indices =
                t.files?.flatMap((f, i) => (playable(f.name) ? [i] : [])) || [];
              const i = indices[indices.indexOf(playing.index) + delta];
              if (i !== undefined) play(t, i);
            }}
          />
        ) : null}
      </main>
      {modal === "add" && (
        <Dialog title="Open torrent address" close={() => setModal(null)}>
          <p>Paste a magnet link, torrent URL or info hash.</p>
          <form
            onSubmit={(e) => {
              e.preventDefault();
              void add(address);
            }}
          >
            <input
              className="text-input"
              aria-label="Torrent address"
              placeholder="magnet:?xt=urn:btih:…"
              autoFocus
              value={address}
              onChange={(e) => setAddress(e.target.value)}
            />
            <div className="dialog-actions">
              <Button
                type="button"
                icon="folder_open"
                onClick={() => void files()}
              >
                Choose torrent files
              </Button>
              <Button
                type="submit"
                className="shelf-primary"
                disabled={!address.trim()}
              >
                Add torrent
              </Button>
            </div>
          </form>
        </Dialog>
      )}
      {remove && (
        <Dialog title="Remove torrent" close={() => setRemove(null)}>
          <p>
            Remove <strong>{remove.name}</strong> from your library?
          </p>
          <label className="check-row">
            <input
              type="checkbox"
              checked={deleteData}
              onChange={(e) => setDeleteData(e.target.checked)}
            />
            Also permanently delete downloaded data
          </label>
          <div className="dialog-actions">
            <Button onClick={() => setRemove(null)}>Cancel</Button>
            <Button
              className={deleteData ? "danger" : "shelf-primary"}
              onClick={() => {
                const id = remove.id;
                setRemove(null);
                void work("Removing torrent…", () =>
                  invoke("torrent_action", {
                    id,
                    action: deleteData ? "delete" : "remove",
                  }),
                );
              }}
            >
              {deleteData ? "Delete data and remove" : "Remove from library"}
            </Button>
          </div>
        </Dialog>
      )}
      {modal === "import" && data && (
        <Dialog
          title="Import your existing library"
          close={() => setModal(null)}
        >
          <p>
            Quit the original TorrentCat first so both apps don’t write to the
            same downloads. Imported torrents start paused and verify their
            existing data when resumed.
          </p>
          <p className="muted">Your original profile remains unchanged.</p>
          <div className="dialog-actions">
            <Button onClick={() => setModal(null)}>Cancel</Button>
            <Button
              className="shelf-primary"
              onClick={() => {
                setModal(null);
                void work("Importing library…", async () => {
                  const result = await invoke<{
                    imported: number;
                    errors: string[];
                  }>("import_legacy", { path: data.tools.legacyProfile });
                  setNotice(
                    `Imported ${result.imported} torrents, paused.${result.errors.length ? " " + result.errors.join("; ") : ""}`,
                  );
                });
              }}
            >
              Original app is closed — import
            </Button>
          </div>
        </Dialog>
      )}
    </div>
  );
}
createRoot(document.getElementById("root")!).render(<App />);
