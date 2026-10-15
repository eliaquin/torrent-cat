import React, { useEffect, useRef } from "react";
export function Icon({ name }: { name: string }) {
  return (
    <i className="icon" aria-hidden="true">
      {name}
    </i>
  );
}
export function Button({
  icon,
  children,
  ...props
}: React.ButtonHTMLAttributes<HTMLButtonElement> & { icon?: string }) {
  return (
    <button {...props} className={"shelf-button " + (props.className || "")}>
      {icon && <Icon name={icon} />}
      {children}
    </button>
  );
}
export function Dialog({
  title,
  close,
  children,
}: {
  title: string;
  close: () => void;
  children: React.ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const previous = document.activeElement as HTMLElement;
    const el = ref.current;
    const first = el?.querySelector<HTMLElement>("input,button");
    first?.focus();
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        close();
      }
      if (e.key === "Tab") {
        const list = Array.from(
          el?.querySelectorAll<HTMLElement>(
            "button:not(:disabled),input,select,textarea",
          ) || [],
        );
        if (!list.length) return;
        const index = list.indexOf(document.activeElement as HTMLElement);
        if (e.shiftKey && index <= 0) {
          e.preventDefault();
          list.at(-1)?.focus();
        }
        if (!e.shiftKey && index === list.length - 1) {
          e.preventDefault();
          list[0]?.focus();
        }
      }
    };
    el?.addEventListener("keydown", key);
    return () => {
      el?.removeEventListener("keydown", key);
      previous?.focus();
    };
  }, []);
  return (
    <div
      className="dialog-backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) close();
      }}
    >
      <div
        className="dialog"
        ref={ref}
        role="dialog"
        aria-modal="true"
        aria-label={title}
      >
        <div className="dialog-heading">
          <h2>{title}</h2>
          <button aria-label="Close dialog" onClick={close}>
            ×
          </button>
        </div>
        {children}
      </div>
    </div>
  );
}
