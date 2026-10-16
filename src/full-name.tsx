import { useEffect, useId, useState } from "react";
import { createPortal } from "react-dom";

/** A wrapping tooltip stays readable even when its title is clipped by a card. */
export function FullName({ name }: { name: string }) {
  const id = useId();
  const [anchor, setAnchor] = useState<{ left: number; top: number; above: boolean } | null>(null);
  useEffect(() => {
    if (!anchor) return;
    const hide = () => setAnchor(null);
    window.addEventListener("scroll", hide, true);
    window.addEventListener("resize", hide);
    return () => {
      window.removeEventListener("scroll", hide, true);
      window.removeEventListener("resize", hide);
    };
  }, [anchor]);
  return <span className="full-name" aria-describedby={anchor ? id : undefined}
    onPointerEnter={(event) => {
      const box = event.currentTarget.getBoundingClientRect();
      const width = Math.min(640, window.innerWidth - 24);
      const above = box.bottom + 160 > window.innerHeight;
      setAnchor({ left: Math.max(12, Math.min(box.left, window.innerWidth - width - 12)),
        top: above ? box.top - 8 : box.bottom + 8, above });
    }}
    onPointerLeave={() => setAnchor(null)}>
    {name}
    {anchor && createPortal(<span id={id} role="tooltip" className="full-name-popup"
      style={{ left: anchor.left, top: anchor.top, transform: anchor.above ? "translateY(-100%)" : undefined }}>
      {name}
    </span>, document.body)}
  </span>;
}
