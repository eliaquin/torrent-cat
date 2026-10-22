import test from "node:test";
import assert from "node:assert/strict";
import { posterFileIndex } from "../src/types.ts";
import { createPlaybackSelection } from "../src/playback-selection.ts";

const torrent = (files, progress = files.map((file) => file.length)) => ({
  files,
  stats: { file_progress: progress },
});
const file = (name, length) => ({ name, length, included: true });

test("poster uses the largest video, ignoring larger non-video files", () => {
  const files = [file("sample.mp4", 10), file("movie.MKV", 100), file("extras.mov", 30), file("archive.zip", 1000)];
  for (const ordered of [files, [...files].reverse()]) {
    assert.equal(ordered[posterFileIndex(torrent(ordered))].name, "movie.MKV");
  }
});

test("a completed sample cannot supply the poster while the largest video downloads", () => {
  const files = [file("sample.mp4", 10), file("movie.mkv", 100)];
  assert.equal(posterFileIndex(torrent(files, [10, 99])), -1);
  assert.equal(posterFileIndex(torrent(files, [10, 100])), 1);
});

test("poster waits for metadata and progress and ignores audio-only torrents", () => {
  assert.equal(posterFileIndex({}), -1);
  assert.equal(posterFileIndex(torrent([])), -1);
  assert.equal(posterFileIndex(torrent([file("album.flac", 100)])), -1);
  assert.equal(posterFileIndex({ files: [file("movie.mp4", 100)] }), -1);
});

test("single-video torrents work and equal-sized videos keep their original order", () => {
  assert.equal(posterFileIndex(torrent([file("movie.mp4", 100)])), 0);
  assert.equal(posterFileIndex(torrent([file("first.mp4", 100), file("second.mkv", 100)])), 0);
});

const seriesTorrent = (hash = "series", id = 1) => ({
  id,
  info_hash: hash,
  files: ["readme.txt", "episode-1.mkv", "episode-2.mp4"].map((name) => ({ name })),
});
const storage = () => {
  const values = new Map();
  return {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
  };
};

test("reopening a multi-video torrent selects the last played file", () => {
  const select = createPlaybackSelection(storage);
  const series = seriesTorrent();
  assert.equal(select(series), 1);
  assert.equal(select(series, 2), 2);
  assert.equal(select(series), 2);
  assert.equal(select(series, 1), 1);
  assert.equal(select(series), 1);
});

test("last played file survives restart and torrent ID changes", () => {
  const saved = storage();
  createPlaybackSelection(() => saved)(seriesTorrent(), 2);
  const reopened = createPlaybackSelection(() => saved);
  assert.equal(reopened(seriesTorrent("series", 99)), 2);
});

test("each torrent remembers its own selection, including index zero", () => {
  const select = createPlaybackSelection(storage);
  const first = seriesTorrent();
  const second = { ...seriesTorrent("other"), files: [{ name: "movie.mp4" }] };
  select(first, 2);
  assert.equal(select(second, 0), 0);
  assert.equal(select(first), 2);
  assert.equal(select(second), 0);
});

test("missing, invalid, or no longer playable saved files fall back safely", () => {
  for (const value of [null, "NaN", "Infinity", "-1", "1.5", "99", "0"]) {
    const saved = storage();
    if (value !== null) saved.setItem("torrentcat:last-played:series", value);
    assert.equal(createPlaybackSelection(() => saved)(seriesTorrent()), 1, String(value));
  }
  const select = createPlaybackSelection(storage);
  assert.equal(select({ info_hash: "loading" }), -1);
  assert.equal(select({ info_hash: "empty", files: [] }), -1);
  assert.equal(select({ info_hash: "text", files: [{ name: "readme.txt" }] }), -1);
});

test("storage failures do not prevent playback or immediate resume", () => {
  for (const unavailable of [
    () => { throw new Error("Storage unavailable"); },
    () => ({
      getItem() { throw new Error("Cannot read"); },
      setItem() { throw new Error("Quota exceeded"); },
    }),
  ]) {
    const select = createPlaybackSelection(unavailable);
    assert.equal(select(seriesTorrent(), 2), 2);
    assert.equal(select(seriesTorrent()), 2);
  }
});

test("invalid explicit selection does not replace the last played file", () => {
  const select = createPlaybackSelection(storage);
  select(seriesTorrent(), 2);
  for (const invalid of [-1, 0, 1.5, 99, NaN]) {
    assert.equal(select(seriesTorrent(), invalid), -1);
    assert.equal(select(seriesTorrent()), 2);
  }
});
