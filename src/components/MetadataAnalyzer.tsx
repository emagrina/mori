import { listen } from "@tauri-apps/api/event";
import { useEffect, useMemo, useState } from "react";
import {
  api,
  CATEGORY_LABEL,
  formatSize,
  plural,
  setPreviewOpen,
  type Entry,
  type LocationInfo,
  type MetaCategory,
  type MetaHit,
  type MetaProgress,
  type MetaView,
  type Places,
  type SanitizeOutcome,
} from "../api";
import { LocationList, ProgressBar } from "./Analyzer";
import { Icon } from "./Icon";
import { ModalFrame } from "./Modal";
import { PlacesMap } from "./PlacesMap";
import { Preview } from "./Preview";
import { Thumb } from "./Thumb";

const PAGE = 200;
const CATEGORIES: MetaCategory[] = ["location", "person", "device", "software", "comment", "identifier"];

const hitEntry = (h: MetaHit): Entry => ({ id: h.id, name: h.name, path: h.path, ext: h.ext, kind: h.kind, size: h.size, modified: h.modified, created: h.created });

/**
 * Sensitive Metadata: which photos, videos and audio files carry a location,
 * names, device details, software, comments or unique IDs. Results (and the
 * places on the offline map) stay in memory. Nothing is changed unless the
 * user creates sanitized copies, which are new files.
 */
export function MetadataAnalyzer({
  active,
  view: mode,
  onToast,
  onInfo,
  onSwitch,
}: {
  active: boolean;
  view: "list" | "map";
  onToast: (msg: string, ms?: number) => void;
  onInfo: (id: string) => void;
  onSwitch: (to: "list" | "map") => void;
}) {
  const [phase, setPhase] = useState<"setup" | "running" | "results">("setup");
  const [locations, setLocations] = useState<LocationInfo[]>([]);
  const [chosen, setChosen] = useState<Set<string>>(() => new Set(["library"]));
  const [recursive, setRecursive] = useState(true);
  const [progress, setProgress] = useState<MetaProgress | null>(null);
  const [cancelling, setCancelling] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [category, setCategory] = useState<MetaCategory | null>(null);
  const [view, setView] = useState<MetaView | null>(null);
  const [hits, setHits] = useState<MetaHit[]>([]);
  const [places, setPlaces] = useState<Places | null>(null);
  const [selected, setSelected] = useState<Set<string>>(() => new Set());
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const [outcome, setOutcome] = useState<SanitizeOutcome[] | null>(null);
  const [preview, setPreview] = useState<{ items: Entry[]; index: number } | null>(null);

  const load = async (cat: MetaCategory | null) => {
    const v = await api.metaResults(cat, 0, PAGE).catch(() => null);
    setView(v);
    setHits(v?.hits ?? []);
    if (v) setPlaces(await api.metaPlaces().catch(() => null));
    return v;
  };

  useEffect(() => {
    const un = [
      listen<MetaProgress>("meta-progress", (e) => setProgress(e.payload)),
      listen<{ status: string; message: string | null }>("meta-done", (e) => {
        setCancelling(false);
        setProgress(null);
        if (e.payload.status === "done") {
          setCategory(null);
          setSelected(new Set());
          load(null).then((v) => setPhase(v ? "results" : "setup"));
        } else {
          setPhase("setup");
          if (e.payload.status === "cancelled") onToast("Scan cancelled");
          else setError(e.payload.message ?? "The scan failed.");
        }
      }),
    ];
    load(null).then((v) => v && setPhase("results"));
    return () => un.forEach((p) => p.then((f) => f()));
  }, []);

  useEffect(() => {
    if (active && phase === "setup") api.analysisLocations().then(setLocations);
  }, [active, phase]);

  useEffect(() => setPreviewOpen(preview !== null), [preview]);

  const start = async () => {
    setError(null);
    try {
      await api.metaStart(
        locations.filter((l) => chosen.has(l.key)).map((l) => l.key),
        recursive,
      );
      setPhase("running");
    } catch (e) {
      setError(String(e));
    }
  };

  const filter = async (c: MetaCategory | null) => {
    setCategory(c);
    await load(c);
  };

  const more = async () => {
    const v = await api.metaResults(category, hits.length, PAGE).catch(() => null);
    if (v) setHits((h) => [...h, ...v.hits]);
  };

  const sanitizable = useMemo(() => hits.filter((h) => h.sanitizable), [hits]);
  const toggle = (id: string) =>
    setSelected((s) => {
      const n = new Set(s);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });

  const runSanitize = async () => {
    setBusy(true);
    const r = await api.sanitizeCopies([...selected]).catch((e) => [...selected].map((id) => ({ id, name: id, newName: null, error: String(e) })));
    setBusy(false);
    setConfirm(false);
    setSelected(new Set());
    setOutcome(r);
  };

  if (!active) return null;
  const scopeNote = view?.locations.map((l) => l.label).join(", ");

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
            {mode === "map" ? "Places" : "Sensitive Metadata"}
            {phase === "results" && scopeNote && <span className="scope-note">in {scopeNote}</span>}
          </h1>
          {phase === "results" && (
            <>
              <div className="segmented small" role="tablist">
                <button className={mode === "list" ? "on" : ""} onClick={() => onSwitch("list")}>
                  Files
                </button>
                <button className={mode === "map" ? "on" : ""} onClick={() => onSwitch("map")}>
                  Map
                </button>
              </div>
              <button
                className="btn"
                onClick={() => {
                  api.metaClear();
                  setView(null);
                  setHits([]);
                  setPlaces(null);
                  setPhase("setup");
                }}
                title="Forget these results (they are only kept in memory)"
              >
                Clear Scan
              </button>
              <button className="btn" onClick={() => setPhase("setup")}>
                New Scan
              </button>
            </>
          )}
        </div>
      </div>

      <div className="analyzer-body">
        {phase === "setup" && (
          <div className="setup">
            <p className="lead">
              Finds photos, videos and audio whose embedded metadata reveals where they were made, who made them, which device or software was used,
              personal comments, or unique IDs. Positions are shown on an offline map. Nothing is changed.
            </p>
            <section>
              <h3>Where to look</h3>
              <LocationList
                locations={locations}
                chosen={chosen}
                setChosen={setChosen}
                onAddFolder={async () => {
                  const loc = await api.analysisChooseFolder().catch(() => null);
                  if (!loc) return;
                  setLocations((ls) => (ls.some((l) => l.key === loc.key) ? ls : [...ls, loc]));
                  setChosen((c) => new Set(c).add(loc.key));
                }}
              />
              <label className="toggle">
                <input type="checkbox" checked={recursive} onChange={(e) => setRecursive(e.target.checked)} />
                <span className="switch" aria-hidden />
                Include subfolders
              </label>
            </section>
            {error && <p className="field-error">{error}</p>}
            <div className="setup-actions">
              {view && (
                <button className="btn" onClick={() => setPhase("results")}>
                  Back to Results
                </button>
              )}
              <button className="btn primary large" disabled={!locations.some((l) => chosen.has(l.key))} onClick={start}>
                Scan Metadata
              </button>
            </div>
            <p className="fineprint">
              Metadata is read by Mori's sandboxed worker (EXIF, XMP, IPTC, QuickTime/MP4, ID3, FLAC). Images are not decoded, nothing is uploaded,
              and results stay in memory until you clear them or quit. Private folders inside the chosen locations are skipped.
            </p>
          </div>
        )}

        {phase === "running" && (
          <div className="analyzer-center">
            <div className="progress-card">
              <h2>{progress?.paused ? "Paused" : progress?.stage === "reading" ? "Reading metadata" : "Finding media"}…</h2>
              <ProgressBar value={progress && progress.total ? progress.done / progress.total : 0} indeterminate={!progress || progress.stage === "collecting"} />
              <dl className="facts">
                <div>
                  <dt>Files</dt>
                  <dd>
                    {progress
                      ? progress.stage === "collecting"
                        ? progress.total.toLocaleString()
                        : `${progress.done.toLocaleString()} of ${progress.total.toLocaleString()}`
                      : "—"}
                  </dd>
                </div>
                <div>
                  <dt>With sensitive fields</dt>
                  <dd>{progress ? progress.found.toLocaleString() : "—"}</dd>
                </div>
                <div>
                  <dt>Unreadable</dt>
                  <dd>{progress ? progress.failed.toLocaleString() : "—"}</dd>
                </div>
              </dl>
              <div className="dialog-actions">
                <button className="btn" onClick={() => api.metaPause(!progress?.paused)} disabled={cancelling || !progress}>
                  {progress?.paused ? "Resume" : "Pause"}
                </button>
                <button
                  className="btn"
                  disabled={cancelling}
                  onClick={() => {
                    setCancelling(true);
                    api.metaCancel();
                  }}
                >
                  {cancelling ? "Cancelling…" : "Cancel"}
                </button>
              </div>
            </div>
          </div>
        )}

        {phase === "results" && view && mode === "list" && (
          <>
            <div className="dup-summary">
              <strong>{plural(view.stats.withSensitive, "file")} with sensitive metadata</strong>
              <span>{plural(view.stats.places, "place")}</span>
              <span className="muted">
                {plural(view.stats.scanned, "media file")} read
                {view.stats.failed + view.stats.unreadable > 0 && ` · ${plural(view.stats.failed + view.stats.unreadable, "file")} unreadable`}
              </span>
            </div>
            <div className="chips meta-filters">
              <button className={`chip ${category === null ? "on" : ""}`} onClick={() => filter(null)}>
                All
              </button>
              {CATEGORIES.filter((c) => view.counts[c]).map((c) => (
                <button key={c} className={`chip ${category === c ? "on" : ""}`} onClick={() => filter(c)}>
                  {CATEGORY_LABEL[c]} <span className="count">{view.counts[c]!.toLocaleString()}</span>
                </button>
              ))}
            </div>
            {hits.length === 0 ? (
              <div className="empty">
                <Icon name="check" size={34} stroke={1.3} />
                <p>No location, people, device, software, comment or ID fields were found.</p>
              </div>
            ) : (
              <div className="meta-list">
                {hits.map((h, i) => (
                  <div key={h.id} className={`meta-row ${selected.has(h.id) ? "on" : ""}`}>
                    <label className="meta-check" title={h.sanitizable ? "Select for a sanitized copy" : "Sanitized copies: JPEG, PNG and WebP only"}>
                      <input type="checkbox" disabled={!h.sanitizable} checked={selected.has(h.id)} onChange={() => toggle(h.id)} />
                    </label>
                    <button className="meta-thumb" onClick={() => setPreview({ items: hits.map(hitEntry), index: i })} title="Preview">
                      <Thumb entry={hitEntry(h)} fit="cover" iconSize={18} />
                    </button>
                    <div className="meta-main">
                      <div className="truncate meta-name">{h.name}</div>
                      <div className="truncate muted small">
                        {h.location} · {h.path}
                      </div>
                      <div className="meta-fieldline truncate">
                        {h.fields.slice(0, 3).map((f, k) => (
                          <span key={k}>
                            <span className="muted">{f.name}:</span> {f.value}
                          </span>
                        ))}
                      </div>
                    </div>
                    <div className="meta-tags">
                      {h.categories.map((c) => (
                        <span key={c} className="tag">
                          {CATEGORY_LABEL[c]}
                        </span>
                      ))}
                    </div>
                    <span className="muted small meta-size">{formatSize(h.size)}</span>
                    <button className="icon-btn" onClick={() => onInfo(h.id)} title="Get Info (all metadata)">
                      <Icon name="info" size={14} />
                    </button>
                  </div>
                ))}
                {hits.length < view.total && (
                  <button className="btn more" onClick={more}>
                    Show more ({(view.total - hits.length).toLocaleString()} left)
                  </button>
                )}
              </div>
            )}
            {(selected.size > 0 || sanitizable.length > 0) && (
              <div className="selection-bar">
                <span>
                  {selected.size ? (
                    <>
                      <strong>{plural(selected.size, "image")}</strong> selected
                    </>
                  ) : (
                    <span className="muted">Select JPEG, PNG or WebP images to make metadata-free copies.</span>
                  )}
                </span>
                <div className="spacer" />
                <button className="btn" onClick={() => setSelected(selected.size ? new Set() : new Set(sanitizable.map((h) => h.id)))}>
                  {selected.size ? "Clear Selection" : "Select All Shown"}
                </button>
                <button className="btn primary" disabled={!selected.size} onClick={() => setConfirm(true)}>
                  Create Sanitized Copies…
                </button>
              </div>
            )}
          </>
        )}

        {phase === "results" && view && mode === "map" &&
          (places && places.ids.length ? (
            <PlacesMap
              places={places}
              onOpen={(i, members) => {
                const items = members.slice(0, 300).map((m) => ({ id: places.ids[m], name: places.names[m], path: places.names[m], ext: "", kind: "photo" as const, size: 0, modified: 0, created: null }));
                setPreview({ items, index: Math.max(0, members.indexOf(i)) });
              }}
            />
          ) : (
            <div className="empty">
              <Icon name="check" size={34} stroke={1.3} />
              <p>None of the scanned files contains a position.</p>
            </div>
          ))}

        {phase === "setup" && mode === "map" && !view && (
          <p className="fineprint center">Places come from a metadata scan: choose where to look, then Scan Metadata.</p>
        )}
      </div>

      {confirm && (
        <ModalFrame onCancel={() => !busy && setConfirm(false)}>
          <div className="dialog-icon">
            <Icon name="copy" size={20} />
          </div>
          <h2>Create {plural(selected.size, "sanitized copy", "sanitized copies")}?</h2>
          <p>
            For each image Mori writes a <strong>new file</strong> (“name-sanitized.jpg”) in the same folder, with the same pixels and no metadata
            (only the orientation is kept). Each copy is verified before it is written. <strong>The originals are not changed.</strong>
          </p>
          <p className="dialog-note">Copies can't be written into protected folders or while Read-only Mode is on.</p>
          <div className="dialog-actions">
            <button className="btn" onClick={() => setConfirm(false)} disabled={busy}>
              Cancel
            </button>
            <button className="btn primary" onClick={runSanitize} disabled={busy} autoFocus>
              {busy ? "Creating…" : "Create Copies"}
            </button>
          </div>
        </ModalFrame>
      )}

      {outcome && (
        <ModalFrame onCancel={() => setOutcome(null)}>
          <h2>
            {plural(outcome.filter((o) => o.newName).length, "sanitized copy", "sanitized copies")} created
          </h2>
          {outcome.some((o) => o.error) && (
            <ul className="outcome-list">
              {outcome
                .filter((o) => o.error)
                .slice(0, 20)
                .map((o) => (
                  <li key={o.id}>
                    <strong>{o.name}</strong>: {o.error}
                  </li>
                ))}
            </ul>
          )}
          <p className="dialog-note">The originals were not changed. Run the scan again to check the copies.</p>
          <div className="dialog-actions">
            <button className="btn primary" onClick={() => setOutcome(null)} autoFocus>
              Done
            </button>
          </div>
        </ModalFrame>
      )}

      {preview && (
        <Preview
          items={preview.items}
          index={preview.index}
          onIndex={(i) => setPreview((p) => p && { ...p, index: i })}
          onClose={() => setPreview(null)}
          onCopyPath={async (e) => {
            const t = await api.copyPath(e.id).catch(() => null);
            if (t) navigator.clipboard.writeText(t).then(() => onToast("Path copied"));
          }}
          onError={(m) => onToast(m)}
        />
      )}
    </main>
  );
}
