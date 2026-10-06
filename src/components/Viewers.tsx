import { useEffect, useMemo, useRef, useState } from "react";
import {
  api,
  audioUrl,
  beginMediaSession,
  captureFrames,
  SCRUB_FRAMES,
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

const FRAMES = SCRUB_FRAMES;

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

// ------------------------------------------------------------------ audio

/** Waveforms are computed only for files up to this size. */
const WAVE_MAX_BYTES = 32 * 1024 * 1024;
const WAVE_BINS = 600;

/**
 * Audio: played by the system engine from verified byte ranges, under the
 * same media-session watchdog as video. The waveform is decoded at a low
 * sample rate and only for files up to 32 MB, so memory stays bounded.
 */
export function AudioView({ entry, iso, onFail }: { entry: Entry; iso: boolean; onFail: () => void }) {
  const audio = useRef<HTMLAudioElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const [peaks, setPeaks] = useState<Float32Array | "skipped" | "failed" | null>(null);
  const [pos, setPos] = useState(0);

  useEffect(() => {
    const a = audio.current;
    if (!a) return;
    let end: () => void = () => {};
    let disposed = false;
    beginMediaSession(entry.id).then(
      (e) => {
        if (disposed) return e();
        end = e;
        a.src = audioUrl(entry.id, iso);
      },
      () => onFail(),
    );
    const onTime = () => a.duration && setPos(a.currentTime / a.duration);
    a.addEventListener("timeupdate", onTime);
    a.addEventListener("error", onFail);
    return () => {
      disposed = true;
      a.removeEventListener("timeupdate", onTime);
      a.removeEventListener("error", onFail);
      a.pause();
      a.removeAttribute("src");
      a.load();
      end();
    };
  }, [entry.id]);

  useEffect(() => {
    if (entry.size > WAVE_MAX_BYTES) {
      setPeaks("skipped");
      return;
    }
    const ctrl = new AbortController();
    (async () => {
      try {
        // Whole file in bounded chunks through the verified audio route.
        const parts: Uint8Array[] = [];
        let got = 0;
        while (got < entry.size) {
          const r = await fetch(audioUrl(entry.id, iso), { headers: { Range: `bytes=${got}-` }, signal: ctrl.signal });
          if (!r.ok) throw new Error("range");
          const b = new Uint8Array(await r.arrayBuffer());
          if (!b.length) break;
          parts.push(b);
          got += b.length;
        }
        const all = new Uint8Array(got);
        let o = 0;
        for (const p of parts) {
          all.set(p, o);
          o += p.length;
        }
        const ctx = new OfflineAudioContext(1, 1, 3000);
        const buf = await ctx.decodeAudioData(all.buffer);
        const data = buf.getChannelData(0);
        const bins = new Float32Array(WAVE_BINS);
        const per = Math.max(1, Math.floor(data.length / WAVE_BINS));
        for (let b = 0; b < WAVE_BINS; b++) {
          let m = 0;
          for (let i = b * per; i < Math.min(data.length, (b + 1) * per); i++) m = Math.max(m, Math.abs(data[i]));
          bins[b] = m;
        }
        if (!ctrl.signal.aborted) setPeaks(bins);
      } catch {
        if (!ctrl.signal.aborted) setPeaks("failed");
      }
    })();
    return () => ctrl.abort();
  }, [entry.id]);

  useEffect(() => {
    const c = canvas.current;
    if (!c || !(peaks instanceof Float32Array)) return;
    const w = (c.width = c.clientWidth * devicePixelRatio);
    const h = (c.height = c.clientHeight * devicePixelRatio);
    const g = c.getContext("2d")!;
    g.clearRect(0, 0, w, h);
    const max = Math.max(0.0001, ...peaks);
    const bw = w / peaks.length;
    const style = getComputedStyle(c);
    for (let i = 0; i < peaks.length; i++) {
      const v = (peaks[i] / max) * (h / 2 - 2);
      g.fillStyle = i / peaks.length <= pos ? style.getPropertyValue("--wave-played") || "#fff" : style.getPropertyValue("--wave") || "#666";
      g.fillRect(i * bw, h / 2 - v, Math.max(1, bw - 1), v * 2 || 1);
    }
  }, [peaks, pos]);

  return (
    <div className="audio-view">
      <Icon name="audio" size={56} />
      <div className="name">{entry.name}</div>
      <div
        className="wave"
        onClick={(e) => {
          const a = audio.current;
          if (!a?.duration) return;
          const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
          a.currentTime = ((e.clientX - r.left) / r.width) * a.duration;
        }}
      >
        {peaks instanceof Float32Array ? <canvas ref={canvas} /> : <div className="wave-note muted small">{peaks === null ? "Reading waveform…" : peaks === "skipped" ? "Waveform shown for files up to 32 MB" : "Waveform unavailable"}</div>}
      </div>
      <audio ref={audio} controls preload="metadata" controlsList="nodownload noremoteplayback" />
    </div>
  );
}

// -------------------------------------------------------------- filmstrip

/**
 * Frames along a video under the player. Uses frames already sampled
 * (sanitized by the worker); sampling more is an explicit click.
 */
export function Filmstrip({ entry, onSeek }: { entry: Entry; onSeek: (fraction: number) => void }) {
  const [ready, setReady] = useState<Set<number>>(() => new Set());
  const [state, setState] = useState<"checking" | "none" | "busy" | "done">("checking");
  useEffect(() => {
    let alive = true;
    setReady(new Set());
    setState("checking");
    // Probe the cache: an image that loads means the frame exists.
    Promise.all(
      Array.from(
        { length: SCRUB_FRAMES },
        (_, k) =>
          new Promise<number | null>((r) => {
            const i = new Image();
            i.onload = () => r(k);
            i.onerror = () => r(null);
            i.src = frameUrl(entry.id, k);
          }),
      ),
    ).then((ks) => {
      if (!alive) return;
      const s = new Set(ks.filter((k): k is number => k !== null));
      setReady(s);
      setState(s.size ? "done" : "none");
    });
    return () => {
      alive = false;
    };
  }, [entry.id]);
  const sample = () => {
    setState("busy");
    const ctrl = new AbortController();
    captureFrames(entry.id, SCRUB_FRAMES, false, ctrl.signal, (k) => setReady((r) => new Set(r).add(k))).then(() => setState("done"));
  };
  if (state === "checking") return null;
  if (state === "none") {
    return (
      <button className="btn small filmstrip-btn" onClick={sample}>
        Show Filmstrip
      </button>
    );
  }
  return (
    <div className="filmstrip">
      {Array.from({ length: SCRUB_FRAMES }, (_, k) => (
        <button key={k} disabled={!ready.has(k)} onClick={() => onSeek((k + 0.5) / SCRUB_FRAMES)} title={`Jump to ${Math.round(((k + 0.5) / SCRUB_FRAMES) * 100)}%`}>
          {ready.has(k) ? <img src={frameUrl(entry.id, k)} alt="" draggable={false} /> : state === "busy" && <span className="dot-spinner" />}
        </button>
      ))}
    </div>
  );
}
