import { msg, t } from "../i18n";
import { Icon, type IconName } from "./Icon";

export type Tool = "analyzer" | "similar" | "metadata" | "places" | "storage" | "health";

const TOOLS: { key: Tool; icon: IconName; title: string; text: string; note: string }[] = [
  { key: "storage", icon: "drive", title: msg("Storage"), text: msg("Where the space goes: types, years, largest items, empty folders."), note: msg("From the index · instant") },
  { key: "analyzer", icon: "duplicate", title: msg("Exact Duplicates"), text: msg("Byte-identical files in any locations."), note: msg("Verified by content hash") },
  { key: "similar", icon: "gallery", title: msg("Similar Media"), text: msg("Photos and videos that look the same, bursts included."), note: msg("Estimated — always review") },
  { key: "metadata", icon: "tag", title: msg("Sensitive Metadata"), text: msg("Locations, names, devices and other revealing metadata."), note: msg("Read in the sandbox") },
  { key: "places", icon: "pin", title: msg("Places"), text: msg("Where photos and videos were taken, on an offline map."), note: msg("No map service") },
  { key: "health", icon: "warning", title: msg("Media Health"), text: msg("Broken, unsupported, risk-flagged and undecodable media."), note: msg("One result per file") },
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
          <h1 className="page-title">{t("Analyze")}</h1>
        </div>
      </div>
      <div className="analyzer-body">
        <div className="center-grid">
          {TOOLS.map((tool) => (
            <button key={tool.key} className="tool-card" onClick={() => onOpen(tool.key)}>
              <Icon name={tool.icon} size={18} />
              <span className="tool-text">
                <span className="tool-title">{t(tool.title)}</span>
                <span className="muted">{t(tool.text)}</span>
                <span className="tool-note">{t(tool.note)}</span>
              </span>
            </button>
          ))}
        </div>
        <p className="fineprint center">
          {t("Analyses read only what you choose, run locally, keep their results in memory, and change nothing until you review and confirm.")}
        </p>
      </div>
    </main>
  );
}
