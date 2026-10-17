import { useEffect, useId, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTimelinePreview } from "./use-timeline-preview";
import { bytes, clock } from "./types";

type FilePieces = {
  fileLength: number;
  downloadedBytes: number;
  ranges: [number, number][];
};

export function PlaybackTimeline({ id, index, time, duration, onSeek, previews = true }: {
  id: number;
  index: number;
  time: number;
  duration: number;
  onSeek: (time: number) => void;
  previews?: boolean;
}) {
  const descriptionId = useId();
  const [hover, setHover] = useState<{ time: number; left: number } | null>(null);
  const previewTime = hover && previews ? Math.floor(hover.time / 5) * 5 : null;
  const image = useTimelinePreview(id, index, previewTime);
  const popupWidth = image ? 240 : 56;
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
    ? `${bytes(pieces.downloadedBytes)} of ${bytes(pieces.fileLength)} downloaded in this file. Shaded sections show downloaded file positions; gaps are missing.`
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
  const length = Number.isFinite(duration) && duration > 0 ? duration : 0;
  const position = Number.isFinite(time) ? Math.max(0, Math.min(time, length)) : 0;
  return (
    <div className="playback-timeline"
      onPointerMove={(e) => {
        if (!length || e.pointerType === "touch") return;
        const rect = e.currentTarget.getBoundingClientRect();
        // Match the range input's 14px thumb travel, including both endpoints.
        const fraction = Math.max(0, Math.min(1, (e.clientX - rect.left - 7) / Math.max(1, rect.width - 14)));
        setHover({ time: fraction * length, left: Math.max(0, Math.min(rect.width, e.clientX - rect.left)) });
      }}
      onPointerLeave={() => setHover(null)}
      onPointerCancel={() => setHover(null)}
    >
      {hover && length > 0 && (
        <div className="timeline-preview" aria-hidden="true"
          style={{ width: popupWidth, left: `clamp(0px, ${hover.left}px - ${popupWidth / 2}px, max(0px, 100% - ${popupWidth}px))` }}>
          {image && <img src={image} alt="" onError={(e) => { e.currentTarget.style.display = "none"; }}
            onLoad={(e) => { e.currentTarget.style.display = "block"; }} />}
          <span>{clock(hover.time)}</span>
        </div>
      )}
      <div className="timeline-track" aria-hidden="true">
        <svg className="download-progress" viewBox="0 0 1 1" preserveAspectRatio="none">
          <path d={path} />
        </svg>
      </div>
      <input
        type="range"
        aria-label="Playback position"
        aria-valuetext={`${clock(position)} of ${clock(length)}`}
        aria-describedby={descriptionId}
        min={0}
        max={length || 1}
        step="0.1"
        value={position}
        disabled={!length}
        onChange={(e) => onSeek(Number(e.target.value))}
      />
      <span id={descriptionId} hidden>{description}</span>
    </div>
  );
}
