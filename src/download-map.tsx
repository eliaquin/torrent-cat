import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { bytes } from "./types";

type FilePieces = {
  fileLength: number;
  downloadedBytes: number;
  ranges: [number, number][];
};

export function DownloadMap({ id, index }: { id: number; index: number }) {
  const [pieces, setPieces] = useState<FilePieces | null>(null);
  useEffect(() => {
    let active = true;
    let timer: ReturnType<typeof setTimeout>;
    setPieces(null);
    const poll = async () => {
      try {
        const next = await invoke<FilePieces | null>("file_pieces", { id, index });
        if (active) setPieces(next);
      } catch {
        if (active) setPieces(null);
      }
      if (active) timer = setTimeout(poll, 1000);
    };
    void poll();
    return () => { active = false; clearTimeout(timer); };
  }, [id, index]);

  const description = pieces
    ? `${bytes(pieces.downloadedBytes)} of ${bytes(pieces.fileLength)} downloaded in this file. Colored sections are downloaded; gaps are missing.`
    : "Downloaded sections unavailable while the torrent is loading or checking.";
  // One SVG path keeps fragmented torrents cheap to render. Positions are byte
  // offsets within the playing file, not an assumed sequential download prefix.
  const path = pieces && pieces.fileLength > 0
    ? pieces.ranges.map(([start, end]) => {
      const x = start / pieces.fileLength;
      const width = (end - start) / pieces.fileLength;
      return `M${x} 0h${width}v1h${-width}z`;
    }).join("")
    : "";
  return (
    <svg
      className="download-progress"
      viewBox="0 0 1 1"
      preserveAspectRatio="none"
      role="img"
      aria-label={description}
    >
      <title>{description}</title>
      <path d={path} />
    </svg>
  );
}
