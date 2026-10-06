import { listen } from "@tauri-apps/api/event";
import { useEffect, useMemo, useState } from "react";
import {
  api,
  dupEntry,
  formatShortDate,
  formatSize,
  isMac,
  parentOf,
  plural,
  setPreviewOpen,
  type AnalysisDone,
  type AnalysisKind,
  type AnalysisProgress,
  type AnalysisView,
  type CleanupOutcome,
  type DupFile,
  type DupGroup,
  type DupMember,
  type Entry,
  type LocationInfo,
  type PlanItem,
} from "../api";
import { Icon, type IconName } from "./Icon";
import { ModalFrame } from "./Modal";
import { Preview } from "./Preview";
import { Thumb } from "./Thumb";

/**
 * Duplicate Analyzer: SELECT LOCATION → TYPES → ANALYZE (size → partial
 * fingerprint → full hash, all in Rust) → REVIEW → SELECT KEEP → FINAL REVIEW
 * → MOVE TO TRASH. Nothing on disk changes before the very last button, and
 * the backend refuses any plan that would leave a group without a copy.
 */

type Phase = "setup" | "running" | "results" | "review" | "cleaning" | "summary";

const KINDS: { kind: AnalysisKind; label: string; icon: IconName }[] = [
  { kind: "images", label: "Images", icon: "photo" },
  { kind: "videos", label: "Videos", icon: "video" },
  { kind: "documents", label: "Documents", icon: "document" },
  { kind: "audio", label: "Audio", icon: "audio" },
  { kind: "other", label: "Other", icon: "other" },
];

const STAGES: Record<AnalysisProgress["stage"], string> = {
  collecting: "Finding files",
  comparingSizes: "Comparing sizes",
  fingerprinting: "Comparing fingerprints",
  verifying: "Verifying full content",
  done: "Finishing",
};

const LOCATION_ICON: Record<string, IconName> = {
  library: "drive",
  home: "folder",
  pictures: "photo",
  videos: "video",
  downloads: "down",
  documents: "document",
};

/** A copy is identified by its first file's id (stable across result refreshes). */
const keyOf = (m: DupMember) => m.files[0].id;
const PAGE = 60;

export function Analyzer({
  active,
  version,
  onToast,
  onSwitch,
}: {
  active: boolean;
  version: number;
  onToast: (msg: string, ms?: number) => void;
  onSwitch: () => void;
}) {
  const [phase, setPhase] = useState<Phase>("setup");
  const [locations, setLocations] = useState<LocationInfo[]>([]);
  const [chosen, setChosen] = useState<Set<string>>(() => new Set(["library"]));
  /** Empty = all files. */
  const [kinds, setKinds] = useState<Set<AnalysisKind>>(() => new Set());
  const [recursive, setRecursive] = useState(true);
  const [progress, setProgress] = useState<AnalysisProgress | null>(null);
  const [cancelling, setCancelling] = useState(false);
  const [view, setView] = useState<AnalysisView | null>(null);
  /** Copies marked for the Trash. Only a suggestion until the final confirmation. */
  const [marks, setMarks] = useState<Set<string>>(() => new Set());
  const [shown, setShown] = useState(PAGE);
  const [cleanup, setCleanup] = useState<{ done: number; total: number } | null>(null);
  const [outcome, setOutcome] = useState<CleanupOutcome | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; group: DupGroup; member: number; file: DupFile } | null>(null);
  const [preview, setPreview] = useState<{ items: Entry[]; index: number } | null>(null);

  const refreshResults = () =>
    api.analysisResults().then((v) => {
      setView(v);
      if (v) {
        // Drop marks for copies that no longer exist.
        const keys = new Set(v.groups.flatMap((g) => g.members.map(keyOf)));
        setMarks((m) => new Set([...m].filter((k) => keys.has(k))));
      }
      return v;
    });

  // Events from the analysis and cleanup threads.
  useEffect(() => {
    const un = [
      listen<AnalysisProgress>("analysis-progress", (e) => setProgress(e.payload)),
      listen<AnalysisDone>("analysis-done", (e) => {
        setCancelling(false);
        if (e.payload.status === "done") {
          setMarks(new Set());
          setShown(PAGE);
          refreshResults().then((v) => setPhase(v ? "results" : "setup"));
        } else {
          setPhase("setup");
          if (e.payload.status === "cancelled") onToast("Analysis cancelled — nothing was changed");
          else setError(e.payload.message ?? "The analysis failed.");
        }
      }),
      listen<{ done: number; total: number }>("cleanup-progress", (e) => setCleanup(e.payload)),
    ];
    // Results survive a UI reload for as long as the app keeps them in memory.
    refreshResults().then((v) => v && setPhase("results"));
    return () => un.forEach((p) => p.then((f) => f()));
  }, []);

  useEffect(() => {
    if (active && phase === "setup") api.analysisLocations().then(setLocations);
  }, [active, phase]);

  // Files were trashed from the browser: results may have shrunk.
  useEffect(() => {
    if (version && view) refreshResults();
  }, [version]);

  useEffect(() => setPreviewOpen(preview !== null), [preview]);

  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    window.addEventListener("click", close);
    window.addEventListener("blur", close);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("blur", close);
    };
  }, [menu]);

  const groups = view?.groups ?? [];

  // ------------------------------------------------------------ selection

  const marked = (g: DupGroup) => g.members.filter((m) => marks.has(keyOf(m))).length;
  const update = (fn: (next: Set<string>) => void) =>
    setMarks((m) => {
      const next = new Set(m);
      fn(next);
      return next;
    });

  const keepAll = (g: DupGroup) => update((n) => g.members.forEach((m) => n.delete(keyOf(m))));
  const keepOnly = (g: DupGroup, keep: number) =>
    update((n) =>
      g.members.forEach((m, i) => {
        if (i === keep || m.locked) n.delete(keyOf(m));
        else n.add(keyOf(m));
      }),
    );
  const keepCopy = (g: DupGroup, i: number) => update((n) => n.delete(keyOf(g.members[i])));
  const selectForTrash = (g: DupGroup, i: number) => {
    const m = g.members[i];
    if (m.locked) return onToast("This file is half of a Live Photo whose other half isn't duplicated, so it's kept.", 4000);
    if (marks.has(keyOf(m))) return;
    if (marked(g) + 1 >= g.members.length) return onToast("At least one copy must be kept.", 3000);
    update((n) => n.add(keyOf(m)));
  };
  const toggle = (g: DupGroup, i: number) => (marks.has(keyOf(g.members[i])) ? keepCopy(g, i) : selectForTrash(g, i));

  const autoSelect = () => {
    const next = new Set<string>();
    for (const g of groups) g.members.forEach((m, i) => i !== g.suggested && !m.locked && next.add(keyOf(m)));
    setMarks(next);
    setPhase("review");
  };

  // What the final review shows and what is sent to Rust.
  const plan = useMemo(() => {
    const items: PlanItem[] = [];
    const trash: DupFile[] = [];
    let keep = 0;
    let bytes = 0;
    let invalid = false;
    for (const g of groups) {
      const idx = g.members.flatMap((m, i) => (marks.has(keyOf(m)) ? [i] : []));
      if (!idx.length) continue;
      if (idx.length >= g.members.length) invalid = true;
      items.push({ group: g.index, trash: idx });
      g.members.forEach((m, i) => {
        if (idx.includes(i)) {
          trash.push(...m.files);
          bytes += g.unitSize;
        } else keep += m.files.length;
      });
    }
    return { items, trash, keep, bytes, invalid };
  }, [groups, marks]);

  // -------------------------------------------------------------- actions

  const start = async () => {
    setError(null);
    setProgress(null);
    setOutcome(null);
    try {
      await api.analysisStart([...chosen], [...kinds], recursive);
      setPhase("running");
    } catch (e) {
      setError(String(e));
    }
  };

  const cancel = () => {
    setCancelling(true);
    api.analysisCancel();
  };

  const clear = async () => {
    await api.analysisClear();
    setView(null);
    setMarks(new Set());
    setOutcome(null);
    setPhase("setup");
    onToast("Analysis cleared");
  };

  const runCleanup = async () => {
    if (plan.invalid || !plan.items.length) return;
    setCleanup({ done: 0, total: plan.trash.length });
    setCancelling(false);
    setPhase("cleaning");
    try {
      setOutcome(await api.analysisCleanup(plan.items));
    } catch (e) {
      // Rejected before anything was touched (e.g. a group without a copy to keep).
      setOutcome({ trashedFiles: 0, trashedBytes: 0, keptFiles: 0, groupsCleaned: 0, groupsSkipped: 0, failures: [{ path: "", reason: String(e) }], cancelled: false });
    }
    setMarks(new Set());
    await refreshResults();
    setPhase("summary");
  };

  const copyPath = async (id: string) => {
    const text = await api.copyPath(id).catch(() => null);
    if (!text) return onToast("Path unavailable");
    await navigator.clipboard.writeText(text).catch(() => {});
    onToast("Path copied");
  };

  const openPreview = (g: DupGroup, file: DupFile) => {
    const items = g.members.flatMap((m) => m.files).map(dupEntry);
    setPreview({ items, index: Math.max(0, items.findIndex((e) => e.id === file.id)) });
  };

  // --------------------------------------------------------------- render

  if (!active) return null;

  const scopeNote = view?.locations.map((l) => l.label).join(", ");
  const recoverable = groups.reduce((n, g) => n + g.recoverable, 0);
  const duplicateFiles = groups.reduce((n, g) => n + g.members.slice(1).reduce((k, m) => k + m.files.length, 0), 0);

  return (
    <main className="content analyzer">
      <header className="topbar" data-tauri-drag-region>
        <div className="spacer" data-tauri-drag-region />
      </header>
      <div className="page-head">
        <div className="crumbs">
          <span className="crumb">Analyze</span>
        </div>
        <div className="title-row">
          <h1 className="page-title">
            Exact Duplicates
            {phase !== "setup" && phase !== "running" && scopeNote && <span className="scope-note">in {scopeNote}</span>}
          </h1>
          {(phase === "results" || phase === "summary") && view && (
            <>
              <button className="btn" onClick={clear} title="Forget these results (they are only kept in memory)">
                Clear Analysis
              </button>
              <button className="btn" onClick={() => setPhase("setup")}>
                New Analysis
              </button>
            </>
          )}
          {phase === "results" && groups.length > 0 && (
            <button className="btn primary" onClick={autoSelect} title="Keep the suggested copy in every group and select the rest for review">
              Auto-select Duplicates
            </button>
          )}
        </div>
      </div>

      <div className="analyzer-body">
        {phase === "setup" && (
          <Setup
            onSwitch={onSwitch}
            locations={locations}
            chosen={chosen}
            setChosen={setChosen}
            kinds={kinds}
            setKinds={setKinds}
            recursive={recursive}
            setRecursive={setRecursive}
            error={error}
            hasResults={!!view}
            onBackToResults={() => setPhase("results")}
            onAddFolder={async () => {
              const loc = await api.analysisChooseFolder().catch(() => null);
              if (!loc) return;
              setLocations((ls) => (ls.some((l) => l.key === loc.key) ? ls : [...ls, loc]));
              setChosen((c) => new Set(c).add(loc.key));
            }}
            onStart={start}
          />
        )}

        {phase === "running" && <Running progress={progress} cancelling={cancelling} onCancel={cancel} />}

        {phase === "results" && view && (
          <>
            <div className="dup-summary">
              {groups.length ? (
                <>
                  <strong>{plural(groups.length, "duplicate group")}</strong>
                  <span>{plural(duplicateFiles, "extra copy", "extra copies")}</span>
                  <span>{formatSize(recoverable)} recoverable</span>
                </>
              ) : (
                <strong>No exact duplicates found</strong>
              )}
              <span className="muted">
                {plural(view.stats.scanned, "file")} compared byte for byte
                {view.stats.livePairs > 0 && ` · ${plural(view.stats.livePairs, "Live Photo")} kept as pairs`}
                {view.stats.unreadable + view.stats.changed > 0 && ` · ${plural(view.stats.unreadable + view.stats.changed, "file")} skipped (unreadable or changed)`}
              </span>
            </div>
            {groups.length === 0 ? (
              <div className="empty">
                <Icon name="check" size={34} stroke={1.3} />
                <p>Every file in these locations is unique. Similar-looking files are never treated as duplicates.</p>
              </div>
            ) : (
              <div className="dup-list">
                {groups.slice(0, shown).map((g) => (
                  <GroupCard
                    key={keyOf(g.members[0])}
                    group={g}
                    marks={marks}
                    onKeepAll={() => keepAll(g)}
                    onKeepOnly={(i) => keepOnly(g, i)}
                    onToggle={(i) => toggle(g, i)}
                    onPreview={(f) => openPreview(g, f)}
                    onMenu={(ev, i, f) => {
                      ev.preventDefault();
                      ev.stopPropagation();
                      setMenu({ x: ev.clientX, y: ev.clientY, group: g, member: i, file: f });
                    }}
                  />
                ))}
                {shown < groups.length && (
                  <button className="btn more" onClick={() => setShown((n) => n + PAGE)}>
                    Show more ({(groups.length - shown).toLocaleString()} groups left)
                  </button>
                )}
              </div>
            )}
            {plan.items.length > 0 && (
              <div className="selection-bar">
                <span>
                  <strong>{plural(plan.trash.length, "file")}</strong> selected for Trash · {formatSize(plan.bytes)}
                </span>
                <div className="spacer" />
                <button className="btn" onClick={() => setMarks(new Set())}>
                  Keep All
                </button>
                <button className="btn primary" onClick={() => setPhase("review")}>
                  Review…
                </button>
              </div>
            )}
          </>
        )}

        {phase === "cleaning" && cleanup && (
          <div className="analyzer-center">
            <div className="progress-card">
              <h2>Moving duplicates to Trash…</h2>
              <ProgressBar value={cleanup.total ? cleanup.done / cleanup.total : 0} />
              <p className="muted">
                {cleanup.done.toLocaleString()} of {plural(cleanup.total, "file")} · each kept copy is re-checked first
              </p>
              <div className="dialog-actions">
                <button
                  className="btn"
                  disabled={cancelling}
                  onClick={() => {
                    setCancelling(true);
                    api.analysisCleanupCancel();
                  }}
                >
                  {cancelling ? "Stopping…" : "Stop"}
                </button>
              </div>
            </div>
          </div>
        )}

        {phase === "summary" && outcome && (
          <Summary outcome={outcome} remaining={groups.length} onDone={() => setPhase(groups.length ? "results" : "setup")} />
        )}
      </div>

      {phase === "review" && (
        <FinalReview
          plan={plan}
          onCancel={() => {
            setMarks(new Set());
            setPhase("results");
          }}
          onReview={() => setPhase("results")}
          onConfirm={runCleanup}
        />
      )}

      {menu && (
        <div
          className="menu"
          style={{ left: Math.min(menu.x, window.innerWidth - 230), top: Math.min(menu.y, window.innerHeight - 200) }}
          onContextMenu={(e) => e.preventDefault()}
        >
          <button onClick={() => openPreview(menu.group, menu.file)}>
            <Icon name="gallery" size={14} /> Preview
          </button>
          <button onClick={() => api.revealFile(menu.file.id).catch(() => onToast("File unavailable"))}>
            <Icon name="reveal" size={14} /> {isMac ? "Show in Finder" : "Show in Folder"}
          </button>
          <button onClick={() => copyPath(menu.file.id)}>
            <Icon name="copy" size={14} /> Copy Path
          </button>
          <div className="sep" />
          <button onClick={() => keepCopy(menu.group, menu.member)}>
            <Icon name="keep" size={14} /> Keep This Copy
          </button>
          <button className="danger" disabled={menu.group.members[menu.member].locked} onClick={() => selectForTrash(menu.group, menu.member)}>
            <Icon name="trash" size={14} /> Select for Trash
          </button>
        </div>
      )}

      {preview && (
        <Preview
          items={preview.items}
          index={preview.index}
          onIndex={(i) => setPreview((p) => p && { ...p, index: i })}
          onClose={() => setPreview(null)}
          onCopyPath={(e) => copyPath(e.id)}
          onError={(m) => onToast(m)}
        />
      )}
    </main>
  );
}

// ------------------------------------------------------------------ setup

function Setup(p: {
  onSwitch: () => void;
  locations: LocationInfo[];
  chosen: Set<string>;
  setChosen: (s: Set<string>) => void;
  kinds: Set<AnalysisKind>;
  setKinds: (s: Set<AnalysisKind>) => void;
  recursive: boolean;
  setRecursive: (v: boolean) => void;
  error: string | null;
  hasResults: boolean;
  onBackToResults: () => void;
  onAddFolder: () => void;
  onStart: () => void;
}) {
  const toggleKind = (k: AnalysisKind) => {
    const next = new Set(p.kinds);
    if (next.has(k)) next.delete(k);
    else next.add(k);
    p.setKinds(next.size === KINDS.length ? new Set() : next);
  };
  const ready = p.locations.some((l) => p.chosen.has(l.key));
  return (
    <div className="setup">
      <ToolSwitch current="exact" onSwitch={p.onSwitch} />
      <p className="lead">
        Finds files whose content is byte-for-byte identical, whatever their names or dates. Nothing is changed until you review the results and confirm.
      </p>

      <section>
        <h3>Where to look</h3>
        <LocationList locations={p.locations} chosen={p.chosen} setChosen={p.setChosen} onAddFolder={p.onAddFolder} />
      </section>

      <section>
        <h3>File types</h3>
        <div className="chips">
          <button className={`chip ${p.kinds.size === 0 ? "on" : ""}`} onClick={() => p.setKinds(new Set())}>
            <Icon name="all" size={14} /> All Files
          </button>
          {KINDS.map((k) => (
            <button key={k.kind} className={`chip ${p.kinds.has(k.kind) ? "on" : ""}`} onClick={() => toggleKind(k.kind)}>
              <Icon name={k.icon} size={14} /> {k.label}
            </button>
          ))}
        </div>
        <label className="toggle">
          <input type="checkbox" checked={p.recursive} onChange={(e) => p.setRecursive(e.target.checked)} />
          <span className="switch" aria-hidden />
          Include subfolders
        </label>
      </section>

      {p.error && <p className="field-error">{p.error}</p>}

      <div className="setup-actions">
        {p.hasResults && (
          <button className="btn" onClick={p.onBackToResults}>
            Back to Results
          </button>
        )}
        <button className="btn primary large" disabled={!ready} onClick={p.onStart}>
          Analyze
        </button>
      </div>
      <p className="fineprint">
        Files are read locally and compared by size, then a partial fingerprint, then a full BLAKE3 hash. Media is never decoded, nothing is uploaded,
        and results are kept only in memory until you clear them or quit Mori. Private folders inside the chosen locations are skipped; choose a
        private folder directly to analyze it.
      </p>
    </div>
  );
}

/** The two Analyze tools, side by side: deterministic vs estimated. */
export function ToolSwitch({ current, onSwitch }: { current: "exact" | "similar"; onSwitch: () => void }) {
  const tools = [
    { key: "exact", icon: "duplicate" as IconName, title: "Exact Duplicates", text: "Find byte-identical files.", note: "Verified by content hash" },
    {
      key: "similar",
      icon: "gallery" as IconName,
      title: "Similar Photos & Videos",
      text: "Find media that appears to contain the same visual content.",
      note: "Estimated — always review",
    },
  ];
  return (
    <div className="tool-switch" role="tablist" aria-label="Analyze">
      {tools.map((t) => (
        <button
          key={t.key}
          role="tab"
          aria-selected={current === t.key}
          className={`tool-card ${current === t.key ? "on" : ""}`}
          onClick={() => current !== t.key && onSwitch()}
        >
          <Icon name={t.icon} size={18} />
          <span className="tool-text">
            <span className="tool-title">{t.title}</span>
            <span className="muted">{t.text}</span>
            <span className="tool-note">{t.note}</span>
          </span>
        </button>
      ))}
    </div>
  );
}

export function LocationList(p: { locations: LocationInfo[]; chosen: Set<string>; setChosen: (s: Set<string>) => void; onAddFolder: () => void }) {
  const toggle = (key: string) => {
    const next = new Set(p.chosen);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    p.setChosen(next);
  };
  return (
    <div className="loc-list">
      {p.locations.map((l) => (
        <label key={l.key} className={`loc ${p.chosen.has(l.key) ? "on" : ""}`} title={l.path}>
          <input type="checkbox" checked={p.chosen.has(l.key)} onChange={() => toggle(l.key)} />
          <Icon name={LOCATION_ICON[l.key] ?? "folder"} size={16} />
          <span className="loc-text">
            <span className="truncate">{l.key === "library" ? `${l.label} (current drive)` : l.label}</span>
            <span className="truncate muted">
              {l.path} · {l.drive}
            </span>
          </span>
          <span className="check-box" aria-hidden>
            <Icon name="check" size={12} stroke={2.2} />
          </span>
        </label>
      ))}
      <button className="loc add" onClick={p.onAddFolder}>
        <Icon name="folder" size={16} />
        <span className="loc-text">
          <span>Choose a Folder…</span>
          <span className="muted">Only folders you pick are read</span>
        </span>
      </button>
    </div>
  );
}

function Running({ progress: p, cancelling, onCancel }: { progress: AnalysisProgress | null; cancelling: boolean; onCancel: () => void }) {
  const hashing = p && (p.stage === "fingerprinting" || p.stage === "verifying");
  const fraction = !p ? 0 : hashing && p.bytesTotal ? p.bytesDone / p.bytesTotal : p.filesTotal ? p.filesDone / p.filesTotal : 0;
  return (
    <div className="analyzer-center">
      <div className="progress-card">
        <h2>{p ? STAGES[p.stage] : "Starting"}…</h2>
        <ProgressBar value={fraction} indeterminate={!p || p.stage === "collecting"} />
        <dl className="facts">
          <div>
            <dt>Files</dt>
            <dd>{p ? (p.stage === "collecting" ? p.filesTotal.toLocaleString() : `${p.filesDone.toLocaleString()} of ${p.filesTotal.toLocaleString()}`) : "—"}</dd>
          </div>
          <div>
            <dt>Data read</dt>
            <dd>{p && hashing ? `${formatSize(p.bytesDone)} of ${formatSize(p.bytesTotal)}` : "—"}</dd>
          </div>
          <div>
            <dt>Duplicate groups</dt>
            <dd>{p ? p.groups.toLocaleString() : "—"}</dd>
          </div>
          <div>
            <dt>Recoverable</dt>
            <dd>{p ? formatSize(p.recoverable) : "—"}</dd>
          </div>
        </dl>
        <div className="dialog-actions">
          <button className="btn" onClick={onCancel} disabled={cancelling}>
            {cancelling ? "Cancelling…" : "Cancel"}
          </button>
        </div>
      </div>
    </div>
  );
}

export function ProgressBar({ value, indeterminate }: { value: number; indeterminate?: boolean }) {
  return (
    <div className={`progress ${indeterminate ? "indeterminate" : ""}`} role="progressbar" aria-valuenow={Math.round(value * 100)}>
      <div style={{ width: indeterminate ? undefined : `${Math.min(100, value * 100)}%` }} />
    </div>
  );
}

// ---------------------------------------------------------------- results

function GroupCard({
  group: g,
  marks,
  onKeepAll,
  onKeepOnly,
  onToggle,
  onPreview,
  onMenu,
}: {
  group: DupGroup;
  marks: Set<string>;
  onKeepAll: () => void;
  onKeepOnly: (i: number) => void;
  onToggle: (i: number) => void;
  onPreview: (f: DupFile) => void;
  onMenu: (ev: React.MouseEvent, member: number, f: DupFile) => void;
}) {
  const lead = g.members[g.suggested].files[0];
  const trashCount = g.members.filter((m) => marks.has(keyOf(m))).length;
  return (
    <section className="dup-group">
      <header>
        <button className="dup-thumb" onClick={() => onPreview(lead)} title="Preview">
          <Thumb entry={dupEntry(lead)} fit="cover" iconSize={22} />
        </button>
        <div className="dup-title">
          <div className="truncate">{lead.name}</div>
          <div className="muted">
            <span className="tag">Exact duplicate</span>
            {g.live && <span className="tag">Live Photo</span>}
            {plural(g.members.length, "copy", "copies")} · {formatSize(g.unitSize)} each
          </div>
        </div>
        <div className="dup-saving">
          <span>{formatSize(trashCount ? g.unitSize * trashCount : g.recoverable)}</span>
          <span className="muted">{trashCount ? "selected" : "potential saving"}</span>
        </div>
        <button className="btn small" onClick={onKeepAll} disabled={!trashCount}>
          Keep All
        </button>
      </header>
      <Reasons reasons={g.reasons} />
      <div className="dup-members">
        {g.members.map((m, i) => {
          const trash = marks.has(keyOf(m));
          const f = m.files[0];
          return (
            <div key={keyOf(m)} className={`dup-row ${trash ? "trash" : ""}`} onContextMenu={(ev) => onMenu(ev, i, f)}>
              <button className="mini-thumb" onClick={() => onPreview(f)} title="Preview">
                <Thumb entry={dupEntry(f)} fit="cover" iconSize={16} />
              </button>
              <div className="dup-info">
                <div className="truncate name">
                  {m.files.map((x) => x.name).join(" + ")}
                  {i === g.suggested && <span className="tag suggested">Suggested</span>}
                  {m.locked && (
                    <span className="tag" title="Half of a Live Photo whose other half isn't duplicated: kept so the pair isn't broken">
                      <Icon name="lock" size={10} /> Live Photo part
                    </span>
                  )}
                </div>
                <div className="truncate muted" title={`${f.location}/${f.path}`}>
                  {[f.location, ...parentOf(f.path).split("/").filter(Boolean)].join(" / ")}
                </div>
              </div>
              <div className="dup-drive muted" title="Storage location">
                <Icon name="drive" size={13} /> <span className="truncate">{f.drive}</span>
              </div>
              <div className="dup-date muted" title="Created">
                {formatShortDate(f.created ?? f.modified)}
              </div>
              <button
                className={`state-pill ${trash ? "trash" : "keep"}`}
                onClick={() => onToggle(i)}
                disabled={m.locked}
                title={trash ? "Selected for Trash — click to keep" : "Kept — click to select for Trash"}
              >
                <Icon name={trash ? "trash" : "keep"} size={12} />
                {trash ? "Trash" : "Keep"}
              </button>
              <button className="btn small" onClick={() => onKeepOnly(i)} title="Keep this copy and select the others for Trash">
                Keep This One
              </button>
            </div>
          );
        })}
      </div>
    </section>
  );
}

// ----------------------------------------------------- final review & summary

export function FinalReview({
  plan,
  onCancel,
  onReview,
  onConfirm,
  reviewed,
  ignored,
  title,
  confirmLabel,
}: {
  plan: { items: PlanItem[]; trash: DupFile[]; keep: number; bytes: number; invalid: boolean };
  onCancel: () => void;
  onReview: () => void;
  onConfirm: () => void;
  /** Similar media: groups reviewed and groups set aside. */
  reviewed?: number;
  ignored?: number;
  title?: string;
  confirmLabel?: string;
}) {
  const drives = [...new Set(plan.trash.map((f) => f.drive))];
  return (
    <ModalFrame onCancel={onReview} wide>
      <h2>{title ?? "Ready to clean"}</h2>
      <dl className="facts review">
        <div>
          <dt>{reviewed !== undefined ? "Reviewed groups" : "Duplicate groups"}</dt>
          <dd>{(reviewed ?? plan.items.length).toLocaleString()}</dd>
        </div>
        <div>
          <dt>Keep</dt>
          <dd>{plural(plan.keep, "file")}</dd>
        </div>
        <div>
          <dt>Move to Trash</dt>
          <dd>{plural(plan.trash.length, "file")}</dd>
        </div>
        {ignored !== undefined && (
          <div>
            <dt>Ignored</dt>
            <dd>{plural(ignored, "group")}</dd>
          </div>
        )}
        <div>
          <dt>Space recovered</dt>
          <dd>{formatSize(plan.bytes)}</dd>
        </div>
      </dl>
      {plan.invalid ? (
        <p className="field-error">At least one copy must be kept.</p>
      ) : (
        <p>
          The selected files go to the {isMac ? "Trash" : "Recycle Bin"}, where you can restore them. Every kept copy is checked again just before
          cleaning; if one has changed or its drive is gone, that group is left untouched.
          {drives.length > 1 && ` Files on ${drives.join(", ")}.`}
        </p>
      )}
      <ul className="review-list">
        {plan.trash.slice(0, 300).map((f) => (
          <li key={f.id} className="truncate" title={`${f.location}/${f.path}`}>
            <Icon name="trash" size={12} /> {f.location} / {f.path}
          </li>
        ))}
        {plan.trash.length > 300 && <li className="muted">…and {plural(plan.trash.length - 300, "more file")}</li>}
      </ul>
      <div className="dialog-actions">
        <button className="btn" onClick={onCancel}>
          Cancel
        </button>
        <button className="btn" onClick={onReview}>
          Review Selection
        </button>
        <button className="btn primary danger-fill" onClick={onConfirm} disabled={plan.invalid || !plan.items.length}>
          {confirmLabel ?? "Move Selected Duplicates to Trash"}
        </button>
      </div>
    </ModalFrame>
  );
}

export function Summary({ outcome: o, remaining, onDone }: { outcome: CleanupOutcome; remaining: number; onDone: () => void }) {
  return (
    <div className="analyzer-center">
      <div className="progress-card summary">
        <h2>{o.cancelled ? "Cleanup stopped" : o.trashedFiles || !o.failures.length ? "Cleanup complete" : "Nothing was moved"}</h2>
        <dl className="facts">
          <div>
            <dt>Moved to Trash</dt>
            <dd>{plural(o.trashedFiles, "file")}</dd>
          </div>
          <div>
            <dt>Recovered</dt>
            <dd>{formatSize(o.trashedBytes)}</dd>
          </div>
          <div>
            <dt>Originals preserved</dt>
            <dd>{o.keptFiles.toLocaleString()}</dd>
          </div>
        </dl>
        {o.groupsSkipped > 0 && (
          <p className="muted">
            {plural(o.groupsSkipped, "group")} left untouched because a kept copy changed or became unavailable.
          </p>
        )}
        {o.failures.length > 0 && (
          <>
            <h3>{plural(o.failures.length, "file")} couldn't be moved</h3>
            <ul className="review-list">
              {o.failures.slice(0, 200).map((f, i) => (
                <li key={i}>
                  {f.path && <span className="truncate">{f.path}</span>}
                  <span className="muted">{f.reason}</span>
                </li>
              ))}
            </ul>
          </>
        )}
        <div className="dialog-actions">
          <button className="btn primary" onClick={onDone}>
            {remaining ? "Back to Results" : "Done"}
          </button>
        </div>
      </div>
    </div>
  );
}

/** Why Mori suggests keeping one copy: only differences it actually found. */
export function Reasons({ reasons }: { reasons: string[] }) {
  if (!reasons.length) return null;
  return (
    <div className="keep-reasons">
      <span className="muted">★ Suggested because:</span> {reasons.join(" · ")}
    </div>
  );
}
