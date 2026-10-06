import { useEffect, useMemo, useRef, useState } from "react";
import { api, formatSize, plural, type EmptyFolder, type Entry, type StorageReport, type StorageTile } from "../api";
import { ProgressBar } from "./Analyzer";
import { Icon } from "./Icon";
import { ModalFrame } from "./Modal";

type Tab = "overview" | "largest" | "empty";

/**
 * Storage of the browsed drive, computed from Mori's index (no file is
 * read): by type and year, largest items, a folder treemap, and empty
 * folders (verified on disk, moved to the Trash only after review).
 */
export function StorageView({
  active,
  version,
  rootName,
  onOpenFile,
  onOpenFolder,
  onToast,
}: {
  active: boolean;
  version: number;
  rootName: string;
  onOpenFile: (e: Entry) => void;
  onOpenFolder: (id: string) => void;
  onToast: (msg: string, ms?: number) => void;
}) {
  const [tab, setTab] = useState<Tab>("overview");
  const [folder, setFolder] = useState<{ id: string; name: string }[]>([]);
  const [report, setReport] = useState<StorageReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const current = folder.length ? folder[folder.length - 1].id : "";

  useEffect(() => {
    if (!active) return;
    let alive = true;
    api.storageReport(current).then(
      (r) => alive && (setReport(r), setError(null)),
      (e) => alive && setError(String(e)),
    );
    return () => {
      alive = false;
    };
  }, [active, current, version]);

  if (!active) return null;
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
            Storage<span className="scope-note">in {rootName}</span>
          </h1>
          <div className="segmented small" role="tablist">
            {(
              [
                ["overview", "Overview"],
                ["largest", "Largest"],
                ["empty", "Empty Folders"],
              ] as const
            ).map(([t, label]) => (
              <button key={t} className={tab === t ? "on" : ""} onClick={() => setTab(t)} role="tab" aria-selected={tab === t}>
                {label}
              </button>
            ))}
          </div>
        </div>
      </div>
      <div className="analyzer-body storage">
        {error && <p className="field-error">{error}</p>}
        {!report && !error && <div className="spinner" />}
        {report && tab === "overview" && (
          <Overview
            report={report}
            trail={folder}
            onDrill={(t) => t.kind === "folder" && setFolder((f) => [...f, { id: t.id, name: t.name }])}
            onCrumb={(n) => setFolder((f) => f.slice(0, n))}
            onOpenFolder={onOpenFolder}
            rootName={rootName}
          />
        )}
        {report && tab === "largest" && <Largest report={report} onOpenFile={onOpenFile} onOpenFolder={onOpenFolder} />}
        {tab === "empty" && <EmptyFolders version={version} onToast={onToast} />}
      </div>
    </main>
  );
}

const KIND_ORDER = ["photo", "video", "gif", "audio", "document", "other"] as const;
const KIND_NAMES: Record<string, string> = { photo: "Photos", video: "Videos", gif: "GIFs", audio: "Audio", document: "Documents", other: "Other" };

function Overview({
  report: r,
  trail,
  onDrill,
  onCrumb,
  onOpenFolder,
  rootName,
}: {
  report: StorageReport;
  trail: { id: string; name: string }[];
  onDrill: (t: StorageTile) => void;
  onCrumb: (n: number) => void;
  onOpenFolder: (id: string) => void;
  rootName: string;
}) {
  const maxYear = Math.max(1, ...r.byYear.map((y) => y.bytes));
  const kinds = KIND_ORDER.map((k) => r.byKind.find((b) => b.key === k)).filter(Boolean) as StorageReport["byKind"];
  return (
    <div className="storage-overview">
      <div className="storage-summary">
        <strong>{formatSize(r.totalBytes)}</strong> in {plural(r.totalFiles, "file")}
        {r.privateFolders > 0 && <span className="muted"> · {plural(r.privateFolders, "private folder")} not measured</span>}
      </div>
      <div className="kind-bar" aria-label="By type">
        {kinds.map((k) => (
          <div key={k.key} className={`kind-seg k-${k.key}`} style={{ flexGrow: k.bytes }} title={`${KIND_NAMES[k.key] ?? k.key}: ${formatSize(k.bytes)}`} />
        ))}
      </div>
      <div className="kind-legend">
        {kinds.map((k) => (
          <span key={k.key}>
            <i className={`k-${k.key}`} /> {KIND_NAMES[k.key] ?? k.key}{" "}
            <span className="muted">
              {formatSize(k.bytes)} · {k.count.toLocaleString()}
            </span>
          </span>
        ))}
      </div>

      <section className="treemap-section">
        <div className="treemap-head">
          <div className="crumbs">
            <button className="crumb" onClick={() => onCrumb(0)}>
              {rootName}
            </button>
            {trail.map((c, i) => (
              <span key={c.id} className="crumb-wrap">
                <Icon name="chevron" size={11} className="crumb-sep" />
                <button className="crumb" onClick={() => onCrumb(i + 1)}>
                  {c.name}
                </button>
              </span>
            ))}
          </div>
          <span className="muted">{formatSize(r.folderBytes)}</span>
          {trail.length > 0 && (
            <button className="btn small" onClick={() => onOpenFolder(trail[trail.length - 1].id)}>
              Open Folder
            </button>
          )}
        </div>
        <Treemap tiles={r.tiles} onPick={onDrill} />
      </section>

      <section>
        <h3>By year</h3>
        <div className="year-bars">
          {r.byYear.slice(0, 30).map((y) => (
            <div key={y.key} className="year-row">
              <span className="year">{y.key}</span>
              <div className="bar">
                <div style={{ width: `${(y.bytes / maxYear) * 100}%` }} />
              </div>
              <span className="muted">
                {formatSize(y.bytes)} · {y.count.toLocaleString()}
              </span>
            </div>
          ))}
        </div>
        <p className="fineprint">Year of creation (or modification when the drive doesn't record creation).</p>
      </section>
    </div>
  );
}

/** Squarified treemap (Bruls et al.) of the folder's children. */
function Treemap({ tiles, onPick }: { tiles: StorageTile[]; onPick: (t: StorageTile) => void }) {
  const box = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ w: 800, h: 320 });
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setSize({ w: el.clientWidth, h: el.clientHeight }));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  const rects = useMemo(() => squarify(tiles.filter((t) => t.bytes > 0), size.w, size.h), [tiles, size]);
  const privates = tiles.filter((t) => t.kind === "private");
  return (
    <>
      <div className="treemap" ref={box}>
        {rects.map(({ t, x, y, w, h }) => (
          <button
            key={t.kind + t.id + t.name}
            className={`tm-tile ${t.kind}`}
            style={{ left: x, top: y, width: w, height: h }}
            onClick={() => onPick(t)}
            title={`${t.name} — ${formatSize(t.bytes)}${t.kind === "folder" ? ` · ${plural(t.files, "file")}` : ""}`}
            disabled={t.kind !== "folder"}
          >
            {w > 60 && h > 30 && (
              <>
                <span className="tm-name truncate">
                  {t.kind === "folder" && <Icon name="folder" size={11} />} {t.name}
                </span>
                <span className="tm-size">{formatSize(t.bytes)}</span>
              </>
            )}
          </button>
        ))}
        {!rects.length && <div className="empty small-empty">Nothing measurable here.</div>}
      </div>
      {privates.length > 0 && (
        <p className="fineprint">
          <Icon name="lock" size={11} /> Private, not measured: {privates.map((p) => p.name).join(", ")}
        </p>
      )}
    </>
  );
}

type Rect = { t: StorageTile; x: number; y: number; w: number; h: number };

function squarify(tiles: StorageTile[], W: number, H: number): Rect[] {
  const total = tiles.reduce((n, t) => n + t.bytes, 0);
  if (!total || W <= 0 || H <= 0) return [];
  const scale = (W * H) / total;
  const items = tiles.map((t) => ({ t, a: t.bytes * scale }));
  const out: Rect[] = [];
  let x = 0;
  let y = 0;
  let w = W;
  let h = H;
  let row: typeof items = [];
  const worst = (r: typeof items, side: number) => {
    const s = r.reduce((n, i) => n + i.a, 0);
    const max = Math.max(...r.map((i) => i.a));
    const min = Math.min(...r.map((i) => i.a));
    return Math.max((side * side * max) / (s * s), (s * s) / (side * side * min));
  };
  const layout = (r: typeof items) => {
    const s = r.reduce((n, i) => n + i.a, 0);
    if (w >= h) {
      const cw = s / h;
      let cy = y;
      for (const i of r) {
        const ch = i.a / cw;
        out.push({ t: i.t, x, y: cy, w: cw, h: ch });
        cy += ch;
      }
      x += cw;
      w -= cw;
    } else {
      const ch = s / w;
      let cx = x;
      for (const i of r) {
        const cw = i.a / ch;
        out.push({ t: i.t, x: cx, y, w: cw, h: ch });
        cx += cw;
      }
      y += ch;
      h -= ch;
    }
  };
  for (const item of items) {
    const side = Math.min(w, h);
    if (!row.length || worst([...row, item], side) <= worst(row, side)) row.push(item);
    else {
      layout(row);
      row = [item];
    }
  }
  if (row.length) layout(row);
  return out;
}

function Largest({ report: r, onOpenFile, onOpenFolder }: { report: StorageReport; onOpenFile: (e: Entry) => void; onOpenFolder: (id: string) => void }) {
  const [list, setList] = useState<"files" | "videos" | "images" | "folders">("files");
  const items = list === "files" ? r.largestFiles : list === "videos" ? r.largestVideos : r.largestImages;
  return (
    <div className="largest">
      <div className="chips">
        {(
          [
            ["files", "Files"],
            ["videos", "Videos"],
            ["images", "Images"],
            ["folders", "Folders"],
          ] as const
        ).map(([k, label]) => (
          <button key={k} className={`chip ${list === k ? "on" : ""}`} onClick={() => setList(k)}>
            {label}
          </button>
        ))}
      </div>
      <div className="size-list">
        {list === "folders"
          ? r.largestFolders.map((f) => (
              <button key={f.id} className="size-row" onClick={() => onOpenFolder(f.id)}>
                <Icon name="folder" size={14} />
                <span className="truncate size-name">{f.path}</span>
                <span className="muted">{plural(f.files, "file")}</span>
                <span className="size-bytes">{formatSize(f.bytes)}</span>
              </button>
            ))
          : items.map((e) => (
              <button key={e.id} className="size-row" onClick={() => onOpenFile(e)}>
                <Icon name={e.kind} size={14} />
                <span className="truncate size-name" title={e.path}>
                  {e.name} <span className="muted">· {e.path.includes("/") ? e.path.slice(0, e.path.lastIndexOf("/")) : "/"}</span>
                </span>
                <span className="size-bytes">{formatSize(e.size)}</span>
              </button>
            ))}
        {list !== "folders" && !items.length && <p className="muted">Nothing here.</p>}
      </div>
    </div>
  );
}

function EmptyFolders({ version, onToast }: { version: number; onToast: (msg: string, ms?: number) => void }) {
  const [list, setList] = useState<EmptyFolder[] | null>(null);
  const [chosen, setChosen] = useState<Set<string>>(() => new Set());
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const load = () => {
    setList(null);
    api.emptyFolders().then(setList, (e) => onToast(String(e), 4000));
  };
  useEffect(load, [version]);
  if (!list) return <ProgressBar value={0} indeterminate />;
  const eligible = list.filter((f) => !f.note && !f.protected);
  const run = async () => {
    setBusy(true);
    const r = await api.trashEmptyFolders([...chosen]).catch((e) => ({ trashed: [] as string[], bytes: 0, failed: [{ path: "", reason: String(e) }] }));
    setBusy(false);
    setConfirm(false);
    setChosen(new Set());
    onToast(
      r.failed.length ? `Moved ${plural(r.trashed.length, "folder")} to Trash · ${plural(r.failed.length, "folder")} skipped (${r.failed[0].reason})` : `Moved ${plural(r.trashed.length, "folder")} to Trash`,
      5000,
    );
    load();
  };
  return (
    <div className="empty-folders">
      <p className="lead">
        Folders with no files. Each one is checked on disk, including hidden items, and nothing is removed until you review and confirm. They go to the
        Trash, so they can be restored.
      </p>
      {list.length === 0 ? (
        <div className="empty">
          <Icon name="check" size={34} stroke={1.3} />
          <p>No empty folders.</p>
        </div>
      ) : (
        <div className="size-list">
          {list.map((f) => {
            const ok = !f.note && !f.protected;
            return (
              <label key={f.id} className={`size-row ${ok ? "" : "disabled"}`}>
                <input
                  type="checkbox"
                  disabled={!ok}
                  checked={chosen.has(f.id)}
                  onChange={() =>
                    setChosen((c) => {
                      const n = new Set(c);
                      if (n.has(f.id)) n.delete(f.id);
                      else n.add(f.id);
                      return n;
                    })
                  }
                />
                <Icon name="folder" size={14} />
                <span className="truncate size-name">{f.path}</span>
                <span className="muted">
                  {f.protected ? "Never Modify" : f.note ? `Not empty: ${f.note}` : f.nested ? `+ ${plural(f.nested, "empty subfolder")}` : "Empty"}
                </span>
              </label>
            );
          })}
        </div>
      )}
      {list.length > 0 && (
        <div className="selection-bar">
          <span>{chosen.size ? <strong>{plural(chosen.size, "folder")} selected</strong> : <span className="muted">{plural(eligible.length, "folder")} can be removed</span>}</span>
          <div className="spacer" />
          <button className="btn" onClick={() => setChosen(chosen.size ? new Set() : new Set(eligible.map((f) => f.id)))}>
            {chosen.size ? "Clear Selection" : "Select All"}
          </button>
          <button className="btn primary danger-fill" disabled={!chosen.size} onClick={() => setConfirm(true)}>
            Move to Trash…
          </button>
        </div>
      )}
      {confirm && (
        <ModalFrame onCancel={() => !busy && setConfirm(false)}>
          <div className="dialog-icon danger">
            <Icon name="trash" size={20} />
          </div>
          <h2>Move {plural(chosen.size, "empty folder")} to Trash?</h2>
          <p>Each folder is checked again right before it moves. Anything that is no longer empty is skipped.</p>
          <div className="dialog-actions">
            <button className="btn" onClick={() => setConfirm(false)} disabled={busy}>
              Cancel
            </button>
            <button className="btn primary danger-fill" onClick={run} disabled={busy} autoFocus>
              {busy ? "Moving…" : "Move to Trash"}
            </button>
          </div>
        </ModalFrame>
      )}
    </div>
  );
}
