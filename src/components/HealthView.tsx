import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import { api, formatSize, type Entry, type HealthCategory, type HealthItem, type HealthProgress, type HealthView as View } from "../api";
import { ProgressBar } from "./Analyzer";
import { Icon } from "./Icon";
import { fmtNum, msg, t, tn } from "../i18n";

const PAGE = 200;

export const HEALTH_LABEL: Record<HealthCategory, { title: string; note: string }> = {
  risk: { title: msg("Risk-flagged"), note: msg("The content contradicts the name or extension. Mori doesn't decode these further.") },
  broken: { title: msg("Broken"), note: msg("Damaged or incomplete data: empty, truncated, or rejected by the decoder.") },
  unsupported: { title: msg("Unsupported"), note: msg("Real media in a format Mori has no safe decoder or player for (RAW, TIFF, AVI…).") },
  failed: { title: msg("Decode failed"), note: msg("The sandboxed decoder timed out, crashed or hit a safety limit.") },
};
const ORDER: HealthCategory[] = ["risk", "broken", "unsupported", "failed"];

/**
 * Media Health: the photos, videos and audio of the browsed drive that Mori
 * can't show, one result per file, with the reason. Checks run in the
 * sandboxed worker; nothing is changed.
 */
export function HealthView({
  active,
  rootName,
  onOpen,
  onInfo,
  onToast,
}: {
  active: boolean;
  rootName: string;
  onOpen: (items: Entry[], index: number) => void;
  onInfo: (id: string) => void;
  onToast: (msg: string, ms?: number) => void;
}) {
  const [phase, setPhase] = useState<"setup" | "running" | "results">("setup");
  const [progress, setProgress] = useState<HealthProgress | null>(null);
  const [view, setView] = useState<View | null>(null);
  const [items, setItems] = useState<HealthItem[]>([]);
  const [category, setCategory] = useState<HealthCategory | null>(null);
  const [cancelling, setCancelling] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = async (c: HealthCategory | null) => {
    const v = await api.healthResults(c, 0, PAGE).catch(() => null);
    setView(v);
    setItems(v?.items ?? []);
    return v;
  };

  useEffect(() => {
    const un = [
      listen<HealthProgress>("health-progress", (e) => setProgress(e.payload)),
      listen<{ status: string }>("health-done", (e) => {
        setCancelling(false);
        setProgress(null);
        if (e.payload.status === "done") {
          setCategory(null);
          load(null).then((v) => setPhase(v ? "results" : "setup"));
        } else {
          setPhase("setup");
          onToast(t("Health check cancelled"));
        }
      }),
    ];
    load(null).then((v) => v && setPhase("results"));
    return () => un.forEach((p) => p.then((f) => f()));
  }, []);

  if (!active) return null;
  const start = async () => {
    setError(null);
    try {
      await api.healthStart();
      setPhase("running");
    } catch (e) {
      setError(String(e));
    }
  };
  return (
    <main className="content analyzer">
      <header className="topbar" data-tauri-drag-region>
        <div className="spacer" data-tauri-drag-region />
      </header>
      <div className="page-head">
        <div className="crumbs">
          <span className="crumb">{t("Analyze")}</span>
        </div>
        <div className="title-row">
          <h1 className="page-title">
            {t("Media Health")}<span className="scope-note">{t("in {name}", { name: rootName })}</span>
          </h1>
          {phase === "results" && (
            <>
              <button
                className="btn"
                onClick={() => {
                  api.healthClear();
                  setView(null);
                  setPhase("setup");
                }}
              >
                {t("Clear Results")}
              </button>
              <button className="btn" onClick={start}>
                {t("Check Again")}
              </button>
            </>
          )}
        </div>
      </div>
      <div className="analyzer-body">
        {phase === "setup" && (
          <div className="setup">
            <p className="lead">
              {t("Checks every photo, video and audio file on {name} and lists the ones Mori can't show, with the reason. Each file gets at most one result.", {
                name: rootName,
              })}
            </p>
            <ul className="health-legend">
              {ORDER.map((c) => (
                <li key={c}>
                  <strong>{t(HEALTH_LABEL[c].title)}</strong> <span className="muted">— {t(HEALTH_LABEL[c].note)}</span>
                </li>
              ))}
            </ul>
            {error && <p className="field-error">{error}</p>}
            <div className="setup-actions">
              <button className="btn primary large" onClick={start}>
                {t("Check Media")}
              </button>
            </div>
            <p className="fineprint">
              {t(
                "Images are test-decoded by Mori's sandboxed worker (already-made thumbnails count as checked); videos get the sandboxed container probe; audio gets type checks only. Private folders are skipped. Nothing is changed, and results stay in memory.",
              )}
            </p>
          </div>
        )}
        {phase === "running" && (
          <div className="analyzer-center">
            <div className="progress-card">
              <h2>{progress?.paused ? t("Paused…") : t("Checking media…")}</h2>
              <ProgressBar value={progress && progress.total ? progress.done / progress.total : 0} indeterminate={!progress} />
              <dl className="facts">
                <div>
                  <dt>{t("Files")}</dt>
                  <dd>{progress ? t("{done} of {total}", { done: progress.done, total: progress.total }) : "—"}</dd>
                </div>
                <div>
                  <dt>{t("Problems")}</dt>
                  <dd>{progress ? fmtNum(progress.found) : "—"}</dd>
                </div>
              </dl>
              <div className="dialog-actions">
                <button className="btn" onClick={() => api.healthPause(!progress?.paused)} disabled={cancelling || !progress}>
                  {progress?.paused ? t("Resume") : t("Pause")}
                </button>
                <button
                  className="btn"
                  disabled={cancelling}
                  onClick={() => {
                    setCancelling(true);
                    api.healthCancel();
                  }}
                >
                  {cancelling ? t("Cancelling…") : t("Cancel")}
                </button>
              </div>
            </div>
          </div>
        )}
        {phase === "results" && view && (
          <>
            <div className="dup-summary">
              <strong>
                {view.total || Object.values(view.counts).reduce((a, b) => a + (b ?? 0), 0)
                  ? tn(Object.values(view.counts).reduce((a, b) => a + (b ?? 0), 0), "{n} file Mori can't show", "{n} files Mori can't show")
                  : t("Every media file can be shown")}
              </strong>
              <span className="muted">{tn(view.checked, "{n} media file checked", "{n} media files checked")}</span>
            </div>
            <div className="chips meta-filters">
              <button className={`chip ${category === null ? "on" : ""}`} onClick={() => (setCategory(null), load(null))}>
                {t("All")}
              </button>
              {ORDER.filter((c) => view.counts[c]).map((c) => (
                <button key={c} className={`chip ${category === c ? "on" : ""}`} onClick={() => (setCategory(c), load(c))} title={t(HEALTH_LABEL[c].note)}>
                  {t(HEALTH_LABEL[c].title)} <span className="count">{fmtNum(view.counts[c]!)}</span>
                </button>
              ))}
            </div>
            {items.length === 0 ? (
              <div className="empty">
                <Icon name="check" size={34} stroke={1.3} />
                <p>{t("Nothing to report.")}</p>
              </div>
            ) : (
              <div className="meta-list">
                {items.map((h, i) => (
                  <div key={h.id} className="meta-row">
                    <span className={`health-dot hd-${h.category}`} title={t(HEALTH_LABEL[h.category].title)} />
                    <button className="meta-main as-button" onClick={() => onOpen(items, i)} title={t("Open (isolated)")}>
                      <div className="truncate meta-name">{h.name}</div>
                      <div className="truncate muted small">{h.path}</div>
                    </button>
                    <span className="tag">{t(HEALTH_LABEL[h.category].title)}</span>
                    <span className="health-reason truncate" title={h.reason}>
                      {h.reason}
                    </span>
                    <span className="muted small meta-size">{formatSize(h.size)}</span>
                    <button className="icon-btn" onClick={() => onInfo(h.id)} title={t("Get Info")}>
                      <Icon name="info" size={14} />
                    </button>
                  </div>
                ))}
                {items.length < view.total && (
                  <button className="btn more" onClick={() => api.healthResults(category, items.length, PAGE).then((v) => v && setItems((x) => [...x, ...v.items]))}>
                    {t("Show more ({count} left)", { count: view.total - items.length })}
                  </button>
                )}
              </div>
            )}
          </>
        )}
      </div>
    </main>
  );
}
