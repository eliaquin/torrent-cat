import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

// The timeline is keyed by file, so this cache lives only for that playback session.
export function useTimelinePreview(id: number, index: number, time: number | null) {
  const cache = useRef(new Map<number, string>());
  const busy = useRef(false);
  const [image, setImage] = useState<{ time: number; url: string } | null>(null);
  useEffect(() => () => {
    for (const url of cache.current.values()) URL.revokeObjectURL(url);
    cache.current.clear();
  }, []);
  useEffect(() => {
    setImage(null);
    if (time === null) return;
    const cached = cache.current.get(time);
    if (cached) { setImage({ time, url: cached }); return; }
    let active = true;
    let timer: ReturnType<typeof setTimeout>;
    const load = async () => {
      if (!active) return;
      if (busy.current) { timer = setTimeout(load, 250); return; }
      busy.current = true;
      try {
        const bytes = await invoke<number[] | null>("media_preview", { id, index, time });
        if (!active) return;
        if (bytes?.length) {
          const url = URL.createObjectURL(new Blob([new Uint8Array(bytes)], { type: "image/jpeg" }));
          cache.current.set(time, url);
          if (cache.current.size > 80) {
            const oldest = cache.current.keys().next().value!;
            URL.revokeObjectURL(cache.current.get(oldest)!);
            cache.current.delete(oldest);
          }
          setImage({ time, url });
          return;
        }
      } catch { /* Unavailable previews leave the timestamp visible. */ }
      finally { busy.current = false; }
      // A missing section can become available while the pointer stays still.
      if (active) timer = setTimeout(load, 1500);
    };
    timer = setTimeout(load, 75);
    return () => { active = false; clearTimeout(timer); };
  }, [id, index, time]);
  return image?.time === time ? image.url : null;
}
