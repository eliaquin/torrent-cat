export type Preferences = {
  downloadPath: string;
  videoEngine: "builtin" | "vlc" | "external";
  externalPlayerPath: string;
  soundNotifications: boolean;
  completionNotifications: boolean;
  highestPlaybackPriority: boolean;
  watchFolder: string;
  autoAddTorrents: boolean;
  globalTrackers: string[];
};
export type TorrentFile = { name: string; length: number; included: boolean };
export type Torrent = {
  id: number;
  info_hash: string;
  name: string;
  output_folder: string;
  files?: TorrentFile[];
  stats?: {
    state: string;
    finished: boolean;
    progress_bytes: number;
    total_bytes: number;
    file_progress: number[];
    error?: string;
    metadata_missing?: boolean;
    live?: {
      download_speed: { mbps: number };
      upload_speed: { mbps: number };
      snapshot?: { peer_stats?: { live: number } };
    };
  };
};
export type Snapshot = {
  torrents: Torrent[];
  profile: {
    prefs: Preferences;
    positions: Record<string, number>;
    imported: string[];
  };
  tools: {
    vlc: boolean;
    ffmpeg: boolean;
    ffprobe: boolean;
    legacyProfile: string;
  };
  downloadPathExists: boolean;
  version: string;
};
export type Playing = { id: number; index: number; hash: string; name: string };
export const playable = (name: string) =>
  /\.(mp4|m4v|mov|mkv|webm|avi|mp3|m4a|aac|ogg|oga|opus|flac|wav|m2ts|ts)$/i.test(
    name,
  );
export const isVideo = (name: string) =>
  /\.(mp4|m4v|mov|mkv|webm|avi|m2ts|ts)$/i.test(name);
export function posterFileIndex(torrent: Torrent): number {
  let largest = -1;
  torrent.files?.forEach((file, index, files) => {
    if (isVideo(file.name) && (largest < 0 || file.length > files[largest].length)) {
      largest = index;
    }
  });
  // Wait for the largest video rather than caching a completed sample's image.
  if (largest < 0 || (torrent.stats?.file_progress[largest] ?? 0) < torrent.files![largest].length) return -1;
  return largest;
}
export function bytes(n: number) {
  if (!Number.isFinite(n) || n <= 0) return "0 B";
  const i = Math.min(4, Math.floor(Math.log(n) / Math.log(1024)));
  return `${(n / 1024 ** i).toFixed(i === 0 ? 0 : 1)} ${["B", "KB", "MB", "GB", "TB"][i]}`;
}
export function clock(n: number) {
  if (!Number.isFinite(n) || n < 0) return "0:00";
  const s = Math.floor(n);
  return (
    (s >= 3600 ? `${Math.floor(s / 3600)}:` : "") +
    `${Math.floor(s / 60) % 60}`.padStart(s >= 3600 ? 2 : 1, "0") +
    ":" +
    `${s % 60}`.padStart(2, "0")
  );
}
export function status(t: Torrent) {
  return t.stats?.state === "live"
    ? t.stats.finished
      ? "seeding"
      : "downloading"
    : t.stats?.state === "initializing"
      ? "checking"
      : t.stats?.state || "loading";
}
