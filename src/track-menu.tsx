import { useEffect, useId, useRef, useState } from "react";
import { Icon } from "./ui";

export type TrackOption = { id: number; name: string; detail?: string };

export function TrackMenu({ tracks, selected, disabled, onSelect, onOpenChange, label, icon, onAddFile, triggerLabel, className = "" }: {
  label: "Audio" | "Subtitles" | "Playback speed";
  triggerLabel?: string;
  className?: string;
  icon?: string;
  onAddFile?: () => void;
  tracks: TrackOption[];
  selected: number;
  disabled?: boolean;
  onSelect: (id: number) => void;
  onOpenChange: (open: boolean) => void;
}) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const id = useId();
  const active = tracks.find((track) => track.id === selected);
  const heading = label === "Audio" ? "Audio tracks" : label;
  const controlLabel = label === "Audio" ? "Audio track" : label === "Subtitles" ? "Subtitle track" : label;
  const close = () => { setOpen(false); trigger.current?.focus(); };

  useEffect(() => {
    onOpenChange(open);
    return () => onOpenChange(false);
  }, [open, onOpenChange]);

  useEffect(() => {
    if (!open) return;
    const items = menu.current?.querySelectorAll<HTMLButtonElement>("[role=menuitemradio], [role=menuitem]");
    const index = Math.max(0, tracks.findIndex((track) => track.id === selected));
    items?.[index]?.focus();
    const outside = (event: PointerEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", outside);
    return () => document.removeEventListener("pointerdown", outside);
  }, [open]);

  return (
    <div className={`audio-track-control ${className}`} ref={root}>
      <button
        ref={trigger} className="audio-track-trigger" type="button"
        aria-label={`${controlLabel}: ${active?.name || "Off"}`}
        aria-haspopup="menu" aria-expanded={open} aria-controls={open ? id : undefined}
        disabled={disabled}
        onClick={() => setOpen(!open)}
        onKeyDown={(event) => {
          if (event.key === "ArrowDown" || event.key === "ArrowUp") {
            event.preventDefault(); setOpen(true);
          }
        }}
      >
        {disabled ? <span className="spinner" /> : icon ? <Icon name={icon} /> : null}
        <span>{disabled ? "Preparing…" : triggerLabel ?? label}</span>
        <Icon name="expand_less" />
      </button>
      {open && (
        <div id={id} ref={menu} className="audio-track-menu" role="menu" aria-label={heading}
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              event.preventDefault(); event.stopPropagation(); close();
            } else if (["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
              event.preventDefault(); event.stopPropagation();
              const items = Array.from(menu.current?.querySelectorAll<HTMLButtonElement>("[role=menuitemradio], [role=menuitem]") || []);
              const current = items.indexOf(document.activeElement as HTMLButtonElement);
              const next = event.key === "Home" ? 0 : event.key === "End" ? items.length - 1
                : (current + (event.key === "ArrowDown" ? 1 : -1) + items.length) % items.length;
              items[next]?.focus();
            } else if (event.key === "Tab") close();
          }}
        >
          <div className="audio-track-heading" role="presentation"><span>{heading}</span><span>{tracks.length}</span></div>
          {tracks.map((track) => (
            <button key={track.id} type="button" role="menuitemradio" aria-checked={track.id === selected}
              tabIndex={-1} onClick={() => { close(); onSelect(track.id); }}>
              <span className="audio-track-copy"><strong>{track.name}</strong>{track.detail && <small>{track.detail}</small>}</span>
              {track.id === selected && <Icon name="check" />}
            </button>
          ))}
          {onAddFile && <div className="track-menu-footer" role="presentation">
            <button role="menuitem" tabIndex={-1} type="button" onClick={() => { close(); onAddFile(); }}>
              <Icon name="add" /><span>Add subtitle file…</span>
            </button>
          </div>}
        </div>
      )}
    </div>
  );
}
