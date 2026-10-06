import { Icon, type IconName } from "./Icon";

export type Tool = "analyzer" | "similar" | "metadata" | "places" | "storage" | "health";

const TOOLS: { key: Tool; icon: IconName; title: string; text: string; note: string }[] = [
  { key: "storage", icon: "drive", title: "Storage", text: "Where the space goes: types, years, largest items, empty folders.", note: "From the index · instant" },
  { key: "analyzer", icon: "duplicate", title: "Exact Duplicates", text: "Byte-identical files in any locations.", note: "Verified by content hash" },
  { key: "similar", icon: "gallery", title: "Similar Media", text: "Photos and videos that look the same, bursts included.", note: "Estimated — always review" },
  { key: "metadata", icon: "tag", title: "Sensitive Metadata", text: "Locations, names, devices and other revealing metadata.", note: "Read in the sandbox" },
  { key: "places", icon: "pin", title: "Places", text: "Where photos and videos were taken, on an offline map.", note: "No map service" },
  { key: "health", icon: "warning", title: "Media Health", text: "Broken, unsupported, risk-flagged and undecodable media.", note: "One result per file" },
];

/** Every analysis in one place. Each runs only when you start it, and changes nothing until you confirm. */
export function AnalysisCenter({ active, onOpen }: { active: boolean; onOpen: (t: Tool) => void }) {
  if (!active) return null;
  return (
    <main className="content analyzer">
      <header className="topbar" data-tauri-drag-region>
        <div className="spacer" data-tauri-drag-region />
      </header>
      <div className="page-head">
        <div className="title-row">
          <h1 className="page-title">Analyze</h1>
        </div>
      </div>
      <div className="analyzer-body">
        <div className="center-grid">
          {TOOLS.map((t) => (
            <button key={t.key} className="tool-card" onClick={() => onOpen(t.key)}>
              <Icon name={t.icon} size={18} />
              <span className="tool-text">
                <span className="tool-title">{t.title}</span>
                <span className="muted">{t.text}</span>
                <span className="tool-note">{t.note}</span>
              </span>
            </button>
          ))}
        </div>
        <p className="fineprint center">
          Analyses read only what you choose, run locally, keep their results in memory, and change nothing until you review and confirm.
        </p>
      </div>
    </main>
  );
}
