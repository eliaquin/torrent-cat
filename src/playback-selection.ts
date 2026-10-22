import { playable } from "./types.ts";
import type { Torrent } from "./types.ts";

type SelectionStorage = Pick<Storage, "getItem" | "setItem">;

export function createPlaybackSelection(
  storage: () => SelectionStorage = () => window.localStorage,
) {
  const recent = new Map<string, number>();

  return (torrent: Torrent, requested?: number): number => {
    const key = `torrentcat:last-played:${torrent.info_hash}`;
    let remembered = recent.get(torrent.info_hash);
    if (requested === undefined && remembered === undefined) {
      try {
        const saved = storage().getItem(key);
        if (saved !== null) remembered = Number(saved);
      } catch {
        // Playback still works when persistent webview storage is unavailable.
      }
    }
    const files = torrent.files;
    const valid = (index: number | undefined): index is number =>
      index !== undefined && Number.isInteger(index) && index >= 0 &&
      !!files?.[index] && playable(files[index].name);
    const index = requested !== undefined
      ? (valid(requested) ? requested : -1)
      : valid(remembered)
        ? remembered
        : files?.findIndex((file) => playable(file.name)) ?? -1;
    if (index >= 0) {
      // Keep immediate reopen independent of snapshot polling and storage errors.
      recent.set(torrent.info_hash, index);
      try {
        storage().setItem(key, String(index));
      } catch {
        // Retain the selection in memory for this session.
      }
    }
    return index;
  };
}
