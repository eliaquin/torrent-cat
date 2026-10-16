import test from "node:test";
import assert from "node:assert/strict";
import { posterFileIndex } from "../src/types.ts";

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
