import { Icon } from "./ui";

const tabs = [
  ["home", "video_library", "Library"],
  ["preferences", "settings", "Preferences"],
] as const;

export function AppTabs({ page, onChange }: {
  page: "home" | "preferences";
  onChange: (page: "home" | "preferences") => void;
}) {
  return (
    <nav className="workspace-tabs" role="tablist" aria-label="Main navigation">
      {tabs.map(([id, icon, label], index) => (
        <button key={id} id={`workspace-tab-${id}`} role="tab"
          aria-selected={page === id} aria-controls="workspace-panel"
          tabIndex={page === id ? 0 : -1}
          onClick={() => onChange(id)}
          onKeyDown={(event) => {
            const next = event.key === "ArrowLeft" || event.key === "ArrowRight" ? 1 - index
              : event.key === "Home" ? 0 : event.key === "End" ? 1 : null;
            if (next === null) return;
            event.preventDefault();
            onChange(tabs[next][0]);
            document.getElementById(`workspace-tab-${tabs[next][0]}`)?.focus();
          }}>
          <Icon name={icon} />{label}
        </button>
      ))}
    </nav>
  );
}
