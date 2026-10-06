import { useEffect, useMemo, useRef, useState } from "react";
import {
  api,
  captureFrames,
  formatDate,
  formatSize,
  frameUrl,
  pdfPageUrl,
  plural,
  type ArchiveEntry,
  type ArchiveFlag,
  type ArchiveListing,
  type Entry,
  type FileReport,
  type PdfInfo,
} from "../api";
import { Icon } from "./Icon";

// ------------------------------------------------------------------- PDF

const PAGE_SIZE = 1600;
const STRIP_SIZE = 160;

/**
 * Safe PDF preview: every page is rasterised by CoreGraphics inside the
 * sandboxed worker. The page here only ever shows bitmaps — no PDF viewer,
 * no JavaScript, no links, no forms, no attachments.
 */
export function PdfView({ entry, iso, onFail }: { entry: Entry; iso: boolean; onFail: () => void }) {
  const [info, setInfo] = useState<PdfInfo | null>(null);
  const [page, setPage] = useState(1);
  const [zoom, setZoom] = useState(1);
  const [broken, setBroken] = useState(false);
  const stage = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let alive = true;
    api.pdfInfo(entry.id, iso).then(
      (i) => alive && (i.locked || i.pages === 0 ? onFail() : setInfo(i)),
      () => alive && onFail(),
    );
    return () => {
      alive = false;
    };
  }, [entry.id]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!info) return;
      if (e.key === "PageDown" || (e.key === "ArrowDown" && e.altKey)) {
        e.preventDefault();
        setPage((p) => Math.min(info.pages, p + 1));
      } else if (e.key === "PageUp" || (e.key === "ArrowUp" && e.altKey)) {
        e.preventDefault();
        setPage((p) => Math.max(1, p - 1));
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [info]);

  useEffect(() => {
    setBroken(false);
    stage.current?.scrollTo({ top: 0 });
  }, [page]);

  if (!info) return <div className="spinner" />;
  const active = [info.javascript && "JavaScript", info.openAction && "automatic actions", info.embeddedFiles && "embedded files", info.forms && "form fields"].filter(
    Boolean,
  ) as string[];
  // Strip thumbnails are rendered on demand; keep long documents bounded.
  const strip = Array.from({ length: Math.min(info.pages, 400) }, (_, i) => i + 1);
  return (
    <div className="pdf-view">
      <div className="pdf-strip" aria-label="Pages">
        {strip.map((p) => (
          <button key={p} className={p === page ? "on" : ""} onClick={() => setPage(p)} title={`Page ${p}`}>
            <img src={pdfPageUrl(entry.id, p, STRIP_SIZE, iso)} alt="" loading="lazy" draggable={false} />
            <span>{p}</span>
          </button>
        ))}
        {info.pages > strip.length && <div className="muted small">+{info.pages - strip.length} more</div>}
      </div>
      <div className="pdf-main">
        <div className="pdf-bar">
          <button className="icon-btn" disabled={page <= 1} onClick={() => setPage(page - 1)} title="Previous page (Page Up)">
            <Icon name="left" size={14} />
          </button>
          <span className="pdf-count">
            Page {page} of {info.pages.toLocaleString()}
          </span>
          <button className="icon-btn" disabled={page >= info.pages} onClick={() => setPage(page + 1)} title="Next page (Page Down)">
            <Icon name="right" size={14} />
          </button>
          <span className="divider" />
          <button className="icon-btn" disabled={zoom <= 0.5} onClick={() => setZoom((z) => Math.max(0.5, z / 1.25))} title="Zoom out">
            <Icon name="zoomOut" size={14} />
          </button>
          <span className="zoom-label">{Math.round(zoom * 100)}%</span>
          <button className="icon-btn" disabled={zoom >= 3} onClick={() => setZoom((z) => Math.min(3, z * 1.25))} title="Zoom in">
            <Icon name="zoomIn" size={14} />
          </button>
          {active.length > 0 && (
            <span className="pdf-active" title="Mori renders page content only. None of this is run, opened or followed.">
              <Icon name="warning" size={12} /> Contains {active.join(", ")} — not run
            </span>
          )}
        </div>
        <div className="pdf-stage" ref={stage}>
          {broken ? (
            <div className="unsupported small">
              <div className="note">This page could not be rendered safely.</div>
            </div>
          ) : (
            <img
              key={page}
              className="pdf-page"
              src={pdfPageUrl(entry.id, page, zoom > 1.5 ? 2400 : PAGE_SIZE, iso)}
              alt={`Page ${page}`}
              draggable={false}
              style={{ height: `${Math.round(zoom * 100)}%` }}
              onError={() => setBroken(true)}
            />
          )}
        </div>
      </div>
    </div>
  );
}

// --------------------------------------------------------------- archive

const FLAG_LABEL: Record<ArchiveFlag, string> = {
  traversal: "Escapes the folder (../)",
  absolute: "Absolute path",
  "drive-letter": "Drive-letter path",
  "control-chars": "Hidden characters in name",
  "nested-archive": "Archive inside",
  "high-ratio": "Extreme compression",
};

/** Listing only. Nothing is extracted, written or opened. */
export function ArchiveView({ entry, onFail }: { entry: Entry; onFail: () => void }) {
  const [listing, setListing] = useState<ArchiveListing | null>(null);
  useEffect(() => {
    let alive = true;
    api.archiveListing(entry.id).then(
      (l) => alive && setListing(l),
      () => alive && onFail(),
    );
    return () => {
      alive = false;
    };
  }, [entry.id]);
  if (!listing) return <div className="spinner" />;
  return (
    <div className="archive-view">
      <div className="archive-head">
        <div>
          <strong>{listing.format}</strong> · {plural(listing.totalEntries, "entry", "entries")} · {formatSize(listing.totalSize)} uncompressed
          {listing.totalCompressed > 0 && <> · {formatSize(listing.totalCompressed)} stored</>}
          {listing.nestedDepth > 0 && <> · nested {listing.nestedDepth} level{listing.nestedDepth > 1 ? "s" : ""} deep</>}
        </div>
        <div className="muted small">Listing only — nothing was extracted or opened.</div>
      </div>
      {listing.findings.length > 0 && (
        <ul className="findings compact">
          {listing.findings.map((f, i) => (
            <li key={i} className={`finding ${f.level}`}>
              <div className="finding-head">
                <span className="finding-title">{f.title}</span>
              </div>
              <p>{f.detail}</p>
            </li>
          ))}
        </ul>
      )}
      <ArchiveTable listing={listing} depth={0} />
      {listing.truncated && <p className="muted small">The listing stopped at a safety limit; totals are partial.</p>}
    </div>
  );
}

function ArchiveTable({ listing, depth }: { listing: ArchiveListing; depth: number }) {
  return (
    <div className={`archive-table ${depth ? "nested" : ""}`} role="table">
      {listing.entries.map((e, i) => (
        <ArchiveRow key={i} e={e} depth={depth} />
      ))}
      {listing.entries.length < listing.totalEntries && (
        <div className="muted small archive-more">+{(listing.totalEntries - listing.entries.length).toLocaleString()} more entries not listed</div>
      )}
    </div>
  );
}

function ArchiveRow({ e, depth }: { e: ArchiveEntry; depth: number }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <div className={`archive-row ${e.flags.length ? "flagged" : ""}`} role="row">
        <span className="archive-name mono" title={e.path}>
          {e.nested ? (
            <button className="icon-btn tiny" onClick={() => setOpen(!open)} aria-label={open ? "Collapse" : "Expand"}>
              <Icon name={open ? "chevronDown" : "chevron"} size={11} />
            </button>
          ) : (
            <Icon name={e.dir ? "folder" : e.symlink ? "link" : "other"} size={12} />
          )}
          <span className="truncate">{e.path}</span>
        </span>
        <span className="archive-flags">
          {e.encrypted && <span className="tag">Encrypted</span>}
          {e.symlink && <span className="tag attn">Link</span>}
          {e.flags.map((f) => (
            <span key={f} className={`tag ${f === "nested-archive" ? "" : "attn"}`}>
              {FLAG_LABEL[f] ?? f}
            </span>
          ))}
        </span>
        <span className="archive-size">{e.dir ? "—" : formatSize(e.size)}</span>
      </div>
      {open && e.nested && <ArchiveTable listing={e.nested} depth={depth + 1} />}
    </>
  );
}

// ----------------------------------------------------------- video frames

const FRAMES = 8;

/**
 * Isolated video view: no <video> element is shown. Frames are sampled once,
 * re-encoded by the sandboxed worker, and displayed as still images.
 */
export function FrameView({ entry, onFail }: { entry: Entry; onFail: () => void }) {
  const [ready, setReady] = useState<number[]>([]);
  const [done, setDone] = useState(false);
  const [current, setCurrent] = useState(0);
  useEffect(() => {
    const ctrl = new AbortController();
    captureFrames(entry.id, FRAMES, true, ctrl.signal, (k) => setReady((r) => [...r, k].sort((a, b) => a - b))).then((n) => {
      if (ctrl.signal.aborted) return;
      setDone(true);
      if (!n) onFail();
    });
    return () => ctrl.abort();
  }, [entry.id]);
  const shown = ready.includes(current) ? current : ready[0];
  return (
    <div className="frame-view">
      <div className="frame-main">
        {shown === undefined ? <div className="spinner" /> : <img src={frameUrl(entry.id, shown, true)} alt={`Frame ${shown + 1}`} draggable={false} />}
      </div>
      <div className="frame-strip">
        {Array.from({ length: FRAMES }, (_, k) => (
          <button key={k} className={k === shown ? "on" : ""} disabled={!ready.includes(k)} onClick={() => setCurrent(k)} title={`Frame ${k + 1} of ${FRAMES}`}>
            {ready.includes(k) ? <img src={frameUrl(entry.id, k, true)} alt="" draggable={false} /> : !done && <span className="dot-spinner" />}
          </button>
        ))}
      </div>
      <div className="muted small">Still frames sampled across the video. The video itself is not played.</div>
    </div>
  );
}

// ------------------------------------------------------------- metadata

/** What Mori knows without rendering anything: the factual file report. */
export function FactsPanel({ entry }: { entry: Entry }) {
  const [r, setR] = useState<FileReport | null>(null);
  useEffect(() => {
    let alive = true;
    api.fileReport(entry.id).then((x) => alive && setR(x), () => {});
    return () => {
      alive = false;
    };
  }, [entry.id]);
  const flagged = useMemo(() => r?.findings.filter((f) => f.level !== "info") ?? [], [r]);
  if (!r) return null;
  return (
    <div className="facts">
      <dl>
        <dt>Detected</dt>
        <dd>{r.detected?.label ?? "Unknown"}</dd>
        <dt>Extension</dt>
        <dd>
          {r.ext ? `.${r.ext}` : "none"}
          {r.extMatches === false && <span className="attn"> · does not match content</span>}
        </dd>
        <dt>Size</dt>
        <dd>{formatSize(r.size)}</dd>
        <dt>Modified</dt>
        <dd>{formatDate(r.modified)}</dd>
      </dl>
      {flagged.length > 0 && (
        <ul className="findings compact">
          {flagged.map((f, i) => (
            <li key={i} className={`finding ${f.level}`}>
              <div className="finding-head">
                <span className="finding-title">{f.title}</span>
              </div>
              <p>{f.detail}</p>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
