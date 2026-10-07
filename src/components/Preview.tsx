import { useEffect, useRef, useState } from "react";
import {
  api,
  beginMediaSession,
  formatDate,
  formatSize,
  isMac,
  mediaUrl,
  parentOf,
  previewUrl,
  typeLabel,
  type Entry,
  type Inspection,
  type VideoInfo,
} from "../api";
import { aspectOf } from "../filmstrip";
import { Icon } from "./Icon";
import { ArchiveView, AudioView, FactsPanel, Filmstrip, FrameView, PdfView } from "./Viewers";

const MIN_ZOOM = 1;
const MAX_ZOOM = 8;

interface Props {
  items: Entry[];
  index: number;
  onIndex: (i: number) => void;
  onClose: () => void;
  onCopyPath: (e: Entry) => void;
  onError: (msg: string) => void;
  /**
   * Open in Isolation: only worker-rendered copies are shown (video as still
   * frames), and nothing offers to open the original in another app.
   */
  isolated?: boolean;
  /** Mori Quick Look (Space): a floating panel instead of the full window. */
  quick?: boolean;
  /** File management from inside the preview (the `…` menu). */
  actions?: PreviewActions;
  /** False while a dialog or menu is on top: the preview leaves the keyboard alone. */
  active?: boolean;
}

export interface PreviewActions {
  onTrash: (e: Entry) => void;
  onMove: (e: Entry) => void;
  onCopy: (e: Entry) => void;
  onRename: (e: Entry) => void;
  onInfo: (e: Entry) => void;
  onChecksum: (e: Entry) => void;
  onFavorite: (e: Entry) => void;
  /** Whether changes to files are allowed right now (Read-only Mode, Private Inspection). */
  canModify: boolean;
}

/** Seconds per photo in a slideshow. */
const SLIDE_MS = 4000;

export function Preview({ items, index, onIndex, onClose, onCopyPath, onError, isolated = false, quick = false, actions, active = true }: Props) {
  const entry = items[index];
  const [menu, setMenu] = useState(false);
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const [dims, setDims] = useState<string>("");
  const [info, setInfo] = useState<Inspection | null>(null);
  // Chrome fades out after a moment without pointer movement, so media floats alone.
  const [idle, setIdle] = useState(false);
  const idleTimer = useRef(0);
  const overChrome = useRef(false);
  const wake = () => {
    setIdle(false);
    window.clearTimeout(idleTimer.current);
    idleTimer.current = window.setTimeout(() => !overChrome.current && setIdle(true), 2500);
  };
  useEffect(() => {
    wake();
    window.addEventListener("keydown", wake);
    return () => {
      window.removeEventListener("keydown", wake);
      window.clearTimeout(idleTimer.current);
    };
  }, []);
  const chromeHandlers = {
    onPointerEnter: () => {
      overChrome.current = true;
      setIdle(false);
    },
    onPointerLeave: () => {
      overChrome.current = false;
      wake();
    },
  };
  const videoRef = useRef<HTMLVideoElement>(null);
  const hasPrev = index > 0;
  const hasNext = index < items.length - 1;
  // Slideshow: still images only, advancing every few seconds; any key or click stops it.
  const [slideshow, setSlideshow] = useState(false);
  const stills = items.filter((e) => e.kind === "photo" || e.kind === "gif");
  useEffect(() => {
    if (!slideshow) return;
    const t = window.setTimeout(() => {
      const next = items.findIndex((e, i) => i > index && (e.kind === "photo" || e.kind === "gif"));
      if (next < 0) setSlideshow(false);
      else onIndex(next);
    }, SLIDE_MS);
    const stop = (e: Event) => {
      if (e instanceof KeyboardEvent && (e.key === "ArrowLeft" || e.key === "ArrowRight")) return;
      setSlideshow(false);
    };
    window.addEventListener("keydown", stop, true);
    window.addEventListener("pointerdown", stop, true);
    return () => {
      window.clearTimeout(t);
      window.removeEventListener("keydown", stop, true);
      window.removeEventListener("pointerdown", stop, true);
    };
  }, [slideshow, index, items]);

  useEffect(() => {
    setZoom(1);
    setPan({ x: 0, y: 0 });
    setDims("");
    setInfo(null);
    let alive = true;
    api.inspect(entry.id).then(
      (i) => alive && setInfo(i),
      () => alive && setInfo({ detected: "unknown", preview: "none", canOpen: false, mismatch: false, video: null, previewsOff: false }),
    );
    return () => {
      alive = false;
    };
  }, [entry?.id]);

  const zoomBy = (factor: number) =>
    setZoom((z) => {
      const nz = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, z * factor));
      if (nz === 1) setPan({ x: 0, y: 0 });
      return nz;
    });

  useEffect(() => {
    if (!active) return;
    const onKey = (e: KeyboardEvent) => {
      const mod = isMac ? e.metaKey : e.ctrlKey;
      if (menu) {
        if (e.key === "Escape") {
          e.preventDefault();
          setMenu(false);
        }
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      } else if (e.key === "ArrowLeft" && !mod) {
        e.preventDefault();
        if (hasPrev) onIndex(index - 1);
      } else if (e.key === "ArrowRight" && !mod) {
        e.preventDefault();
        if (hasNext) onIndex(index + 1);
      } else if (e.key === " ") {
        e.preventDefault();
        const v = videoRef.current;
        if (v) v.paused ? v.play() : v.pause();
        else onClose();
      } else if (mod && (e.key === "=" || e.key === "+")) {
        e.preventDefault();
        zoomBy(1.25);
      } else if (mod && e.key === "-") {
        e.preventDefault();
        zoomBy(0.8);
      } else if (mod && e.key === "0") {
        e.preventDefault();
        setZoom(1);
        setPan({ x: 0, y: 0 });
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [index, hasPrev, hasNext, onClose, onIndex, active, menu]);
  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(false);
    window.addEventListener("click", close);
    return () => window.removeEventListener("click", close);
  }, [menu]);
  useEffect(() => setMenu(false), [entry?.id]);

  if (!entry) return null;
  // Safe Inspection Mode: opening a file is the explicit request, and it is
  // always shown isolated.
  const iso = isolated || !!info?.previewsOff;
  const isImage = info?.preview === "image";
  const openFile = () => api.openFile(entry.id).catch((e) => onError(String(e)));

  return (
    <div
      className={`preview ${idle || slideshow ? "idle" : ""} ${quick ? "quick" : ""}`}
      role="dialog"
      aria-label={`${quick ? "Quick Look" : "Preview"} of ${entry.name}`}
      onPointerMove={wake}
      onMouseDown={(e) => quick && e.target === e.currentTarget && onClose()}
    >
      <div className="preview-panel">
      <header className="preview-bar" data-tauri-drag-region {...chromeHandlers}>
        <button className="icon-btn" onClick={onClose} title="Close (Esc)">
          <Icon name="close" size={16} />
        </button>
        <div className="preview-title" data-tauri-drag-region>
          <div className="truncate" data-tauri-drag-region>
            {entry.name}
          </div>
          <div className="sub" data-tauri-drag-region>
            {index + 1} of {items.length}
          </div>
        </div>
        <div className="preview-actions">
          {isImage && (
            <>
              <button className="icon-btn" onClick={() => zoomBy(0.8)} disabled={zoom <= 1} title="Zoom out (⌘−)">
                <Icon name="zoomOut" />
              </button>
              <span className="zoom-label">{Math.round(zoom * 100)}%</span>
              <button className="icon-btn" onClick={() => zoomBy(1.25)} disabled={zoom >= MAX_ZOOM} title="Zoom in (⌘+)">
                <Icon name="zoomIn" />
              </button>
              <span className="divider" />
            </>
          )}
          {stills.length > 1 && !quick && (
            <button className="icon-btn" onClick={() => setSlideshow(true)} title="Slideshow (photos)">
              <Icon name="play" />
            </button>
          )}
          <button className="icon-btn" onClick={() => onCopyPath(entry)} title="Copy path">
            <Icon name="copy" />
          </button>
          <button className="icon-btn" onClick={() => api.revealFile(entry.id)} title={isMac ? "Show in Finder" : "Show in folder"}>
            <Icon name="reveal" />
          </button>
          {info?.canOpen && !iso && (
            <button className="btn" onClick={openFile}>
              <Icon name="external" size={14} /> Open
            </button>
          )}
          {actions && (
            <div className="preview-more">
              <button
                className={`icon-btn ${menu ? "on" : ""}`}
                onClick={(ev) => {
                  ev.stopPropagation();
                  setMenu((m) => !m);
                }}
                title="More actions"
                aria-label="More actions"
                aria-haspopup="menu"
                aria-expanded={menu}
              >
                <Icon name="more" />
              </button>
              {menu && <PreviewMenu entry={entry} actions={actions} close={() => setMenu(false)} />}
            </div>
          )}
        </div>
      </header>

      {iso && info && (
        <div className="iso-banner" {...chromeHandlers}>
          <Icon name="shield" size={12} />
          <span>
            <strong>Isolated view</strong> · {info.previewsOff && !isolated ? "Safe Inspection Mode · " : ""}the original is never opened or run — only copies
            rendered by Mori's sandboxed worker are shown.
          </span>
        </div>
      )}

      <div className="preview-stage">
        {info === null ? (
          <div className="spinner" />
        ) : (
          <PreviewBody
            key={entry.id}
            entry={entry}
            info={info}
            zoom={zoom}
            pan={pan}
            setPan={setPan}
            zoomBy={zoomBy}
            resetZoom={() => {
              setZoom((z) => (z > 1 ? 1 : 2));
              setPan({ x: 0, y: 0 });
            }}
            onDims={setDims}
            videoRef={videoRef}
            onOpen={iso ? undefined : openFile}
            iso={iso}
          />
        )}
        {hasPrev && (
          <button className="nav prev" onClick={() => onIndex(index - 1)} title="Previous (←)">
            <Icon name="left" size={22} />
          </button>
        )}
        {hasNext && (
          <button className="nav next" onClick={() => onIndex(index + 1)} title="Next (→)">
            <Icon name="right" size={22} />
          </button>
        )}
      </div>

      <footer className="preview-info" {...chromeHandlers}>
        <span>{typeLabel(entry)}</span>
        {dims && <span>{dims}</span>}
        <span>{formatSize(entry.size)}</span>
        <span>Modified {formatDate(entry.modified)}</span>
        <span className="truncate">{parentOf(entry.path) || "/"}</span>
      </footer>
      </div>
    </div>
  );
}


/** The preview's `…` menu: manage the file being viewed. */
function PreviewMenu({ entry, actions, close }: { entry: Entry; actions: PreviewActions; close: () => void }) {
  const item = (label: string, icon: Parameters<typeof Icon>[0]["name"], run: (e: Entry) => void, opts: { keys?: string; danger?: boolean; modify?: boolean } = {}) => (
    <button
      role="menuitem"
      className={opts.danger ? "danger" : ""}
      disabled={opts.modify && !actions.canModify}
      title={opts.modify && !actions.canModify ? "Read-only: Mori won't change files right now" : undefined}
      onClick={() => {
        close();
        run(entry);
      }}
    >
      <Icon name={icon} size={14} /> {label}
      {opts.keys && <kbd className="menu-kbd">{opts.keys}</kbd>}
    </button>
  );
  return (
    <div className="menu preview-menu" role="menu" onClick={(e) => e.stopPropagation()}>
      {item("Get Info", "info", actions.onInfo, { keys: "I" })}
      {item(entry.favorite ? "Remove from Favorites" : "Add to Favorites", "star", actions.onFavorite)}
      {item("Calculate Checksum", "check", actions.onChecksum)}
      <div className="sep" />
      {item("Rename…", "rename", actions.onRename, { keys: "F2", modify: true })}
      {item("Move to…", "folder", actions.onMove, { keys: "M", modify: true })}
      {item("Copy to…", "copy", actions.onCopy, { keys: "⇧M", modify: true })}
      <div className="sep" />
      {item("Move to Trash", "trash", actions.onTrash, { keys: isMac ? "⌘⌫" : "Delete", danger: true, modify: true })}
    </div>
  );
}

export interface BodyProps {
  entry: Entry;
  info: Inspection;
  zoom: number;
  pan: { x: number; y: number };
  setPan: (p: { x: number; y: number }) => void;
  zoomBy: (f: number) => void;
  resetZoom: () => void;
  onDims: (d: string) => void;
  videoRef: React.RefObject<HTMLVideoElement | null>;
  /** Absent in the isolated view: there is no way to open the original there. */
  onOpen?: () => void;
  iso: boolean;
}

type Failure = "image" | "text" | "pdf" | "archive" | "frames" | "audio" | VideoFailure;

/** The media itself (shared with Quick Cleanup): the same safe preview paths. */
export function PreviewBody({ entry, info, zoom, pan, setPan, zoomBy, resetZoom, onDims, videoRef, onOpen, iso }: BodyProps) {
  const [failed, setFailed] = useState<Failure | null>(null);
  const [text, setText] = useState<string | null>(null);
  const drag = useRef<{ x: number; y: number; px: number; py: number } | null>(null);
  // Body is keyed by file id, so this only guards against late async results
  // arriving after the user has moved on (the component is gone by then).
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true; // StrictMode remounts: re-arm after a simulated unmount
    return () => {
      alive.current = false;
    };
  }, []);

  useEffect(() => {
    if (info.preview === "text") api.readText(entry.id).then((t) => alive.current && setText(t), () => alive.current && setFailed("text"));
  }, [entry.id, info.preview]);

  const warning = info.mismatch ? <MismatchNote entry={entry} info={info} /> : null;

  if (failed && isVideoFailure(failed)) {
    return <VideoUnavailable entry={entry} info={info} reason={failed} onOpen={onOpen} />;
  }
  if (failed) {
    return <Unavailable entry={entry} info={info} title="Preview unavailable — file could not be safely processed." onOpen={onOpen} />;
  }
  const fail = (f: Failure) => () => alive.current && setFailed(f);
  if (info.preview === "pdf") return <PdfView entry={entry} iso={iso} onFail={fail("pdf")} />;
  if (info.preview === "audio") {
    // The isolated view only shows worker-made copies; audio would be the original bytes.
    return iso ? (
      <Unavailable entry={entry} info={info} title="Audio isn't played in the isolated view." />
    ) : (
      <AudioView entry={entry} iso={false} onFail={fail("audio")} />
    );
  }
  if (info.preview === "archive") return <ArchiveView entry={entry} onFail={fail("archive")} />;
  if (iso && info.preview === "video") return <FrameView entry={entry} aspect={info.video ? aspectOf(info.video.width, info.video.height) : null} onFail={fail("frames")} />;
  if (info.video && info.video.status !== "playable") {
    const reason: VideoFailure = { unsupportedCodec: "codec", noVideo: "noVideo", damaged: "damaged", blocked: "blocked", playable: "damaged" }[
      info.video.status
    ] as VideoFailure;
    return <VideoUnavailable entry={entry} info={info} reason={reason} onOpen={onOpen} />;
  }

  if (info.preview === "image") {
    return (
      <>
        {warning}
        <div
          className={`image-stage ${zoom > 1 ? "zoomed" : ""}`}
          onWheel={(e) => {
            if (e.ctrlKey || e.metaKey) zoomBy(e.deltaY < 0 ? 1.1 : 0.9);
            else if (zoom > 1) setPan({ x: pan.x - e.deltaX, y: pan.y - e.deltaY });
          }}
          onPointerDown={(e) => {
            if (zoom <= 1) return;
            (e.target as HTMLElement).setPointerCapture(e.pointerId);
            drag.current = { x: e.clientX, y: e.clientY, px: pan.x, py: pan.y };
          }}
          onPointerMove={(e) => {
            const d = drag.current;
            if (d) setPan({ x: d.px + e.clientX - d.x, y: d.py + e.clientY - d.y });
          }}
          onPointerUp={() => (drag.current = null)}
          onDoubleClick={resetZoom}
        >
          {/* A size-limited copy decoded and re-encoded by the sandboxed worker, never the original bytes. */}
          <img
            src={previewUrl(entry.id, iso)}
            alt={entry.name}
            draggable={false}
            style={{ transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom})` }}
            onLoad={() => api.previewInfo(entry.id).then((d) => alive.current && d && onDims(`${d[0]} × ${d[1]}`))}
            onError={() => setFailed("image")}
          />
        </div>
      </>
    );
  }

  if (info.preview === "video") {
    return (
      <>
        {warning}
        <div className="video-stack">
          <VideoPlayer id={entry.id} videoRef={videoRef} onDims={onDims} onFail={(f) => alive.current && setFailed(f)} />
          <Filmstrip
            entry={entry}
            aspect={info.video ? aspectOf(info.video.width, info.video.height) : null}
            onSeek={(f) => {
              const v = videoRef.current;
              if (v?.duration) v.currentTime = f * v.duration;
            }}
          />
        </div>
      </>
    );
  }

  if (info.preview === "text") {
    // Rendered strictly as text: React escapes it, nothing becomes a link or markup.
    return text === null ? <div className="spinner" /> : <pre className="text-preview">{text}</pre>;
  }

  return (
    <Unavailable
      entry={entry}
      info={info}
      title={info.mismatch ? "This file type cannot be safely previewed." : "Preview not supported"}
      onOpen={onOpen}
    />
  );
}

// ------------------------------------------------------------------ video

type VideoFailure = "codec" | "noVideo" | "damaged" | "blocked" | "timeout" | "stalled";
const VIDEO_FAILURES: string[] = ["codec", "noVideo", "damaged", "blocked", "timeout", "stalled"];
const isVideoFailure = (f: Failure): f is VideoFailure => VIDEO_FAILURES.includes(f);

/** Nothing decoded within this long counts as a failed load. */
const LOAD_TIMEOUT_MS = 20_000;
/** Buffering without progress for this long counts as stalled. */
const STALL_TIMEOUT_MS = 25_000;

/**
 * One video, one session. The source is attached imperatively so teardown is
 * fully under our control: on unmount (switching files, closing the preview)
 * the element is paused, its source detached and `load()`ed to release the
 * decoder, timers cleared, listeners removed and the Rust session ended.
 * Exactly one VideoPlayer exists at a time, so A → B → C leaves only C alive.
 */
function VideoPlayer({
  id,
  videoRef,
  onDims,
  onFail,
}: {
  id: string;
  videoRef: React.RefObject<HTMLVideoElement | null>;
  onDims: (d: string) => void;
  onFail: (f: VideoFailure) => void;
}) {
  useEffect(() => {
    const v = videoRef.current;
    if (!v) return;
    let disposed = false;
    let endSession: () => void = () => {};
    let loadTimer = 0;
    let stallTimer = 0;

    const teardown = () => {
      clearTimeout(loadTimer);
      clearTimeout(stallTimer);
      for (const [type, fn] of handlers) v.removeEventListener(type, fn);
      v.pause();
      v.removeAttribute("src");
      v.load();
      endSession();
    };
    const fail = (f: VideoFailure) => {
      if (disposed) return;
      disposed = true;
      teardown();
      onFail(f);
    };
    const clearStall = () => clearTimeout(stallTimer);
    const handlers: [string, () => void][] = [
      [
        "loadeddata",
        () => {
          clearTimeout(loadTimer);
          // A "playable" video with no picture is a damaged container.
          if (!v.videoWidth || !v.videoHeight) return fail("damaged");
          onDims(`${v.videoWidth} × ${v.videoHeight}`);
        },
      ],
      [
        "error",
        () => {
          const code = v.error?.code;
          fail(code === MediaError.MEDIA_ERR_SRC_NOT_SUPPORTED ? "codec" : "damaged");
        },
      ],
      ["waiting", () => ((clearStall(), (stallTimer = window.setTimeout(() => fail("stalled"), STALL_TIMEOUT_MS))))],
      ["stalled", () => ((clearStall(), (stallTimer = window.setTimeout(() => fail("stalled"), STALL_TIMEOUT_MS))))],
      ["playing", clearStall],
      ["timeupdate", clearStall],
      ["pause", clearStall],
    ];
    for (const [type, fn] of handlers) v.addEventListener(type, fn);

    beginMediaSession(id).then(
      (end) => {
        if (disposed) return end(); // the user already moved on
        endSession = end;
        loadTimer = window.setTimeout(() => fail("timeout"), LOAD_TIMEOUT_MS);
        v.src = mediaUrl(id);
        v.play().catch(() => {}); // autoplay may be refused; controls remain
      },
      () => fail("damaged"),
    );

    return () => {
      if (disposed) return;
      disposed = true;
      teardown();
    };
  }, [id]);

  return (
    <video
      ref={videoRef}
      className="video"
      controls
      playsInline
      preload="auto"
      disablePictureInPicture
      disableRemotePlayback
      controlsList="nodownload noremoteplayback"
    />
  );
}

function formatDuration(ms: number) {
  const s = Math.round(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

function videoFacts(v: VideoInfo | null) {
  if (!v) return null;
  const parts = [v.container];
  if (v.videoCodec) parts.push(`video ${v.videoCodec}`);
  if (v.audioCodec) parts.push(`audio ${v.audioCodec}`);
  if (v.width && v.height) parts.push(`${v.width} × ${v.height}`);
  if (v.durationMs) parts.push(formatDuration(v.durationMs));
  return parts.join(" · ");
}

const VIDEO_MESSAGES: Record<VideoFailure, [string, string | null]> = {
  codec: ["Video format detected, but this codec is not supported for secure preview.", null],
  noVideo: ["This video could not be safely previewed.", "The file has no video track."],
  damaged: ["This video could not be safely previewed.", "The file appears to be damaged or incomplete."],
  blocked: [
    "This video could not be safely previewed.",
    "It stopped the system video engine before, so Mori won't load it again.",
  ],
  timeout: ["This video could not be safely previewed.", "Loading took too long and was stopped."],
  stalled: ["This video could not be safely previewed.", "Playback stopped making progress and was stopped."],
};

function VideoUnavailable({ entry, info, reason, onOpen }: { entry: Entry; info: Inspection; reason: VideoFailure; onOpen?: () => void }) {
  const [title, note] = VIDEO_MESSAGES[reason];
  const facts = videoFacts(info.video);
  return (
    <div className="unsupported">
      <Icon name="video" size={72} />
      <div className="name">{entry.name}</div>
      <div className="note strong">{title}</div>
      {note && <div className="note">{note}</div>}
      {info.mismatch && <MismatchNote entry={entry} info={info} />}
      <div className="details">
        {facts && (
          <>
            {facts}
            <br />
          </>
        )}
        {formatSize(entry.size)} · Modified {formatDate(entry.modified)}
      </div>
      <div className="actions">
        <button className="btn" onClick={() => api.revealFile(entry.id)}>
          <Icon name="reveal" size={14} /> {isMac ? "Show in Finder" : "Show in folder"}
        </button>
        {info.canOpen && onOpen && (
          <button className="btn primary" onClick={onOpen}>
            <Icon name="external" size={14} /> Open in system
          </button>
        )}
      </div>
    </div>
  );
}

function MismatchNote({ entry, info }: { entry: Entry; info: Inspection }) {
  return (
    <div className="mismatch">
      ⚠︎ Named “.{entry.ext}” but{" "}
      {info.detected === "executable"
        ? "the content is an executable program"
        : info.detected === "unknown"
          ? `the content isn't a valid ${entry.ext.toUpperCase()} file`
          : `the content is ${info.detected.toUpperCase()}`}
      .
    </div>
  );
}

function Unavailable({ entry, info, title, onOpen }: { entry: Entry; info: Inspection; title: string; onOpen?: () => void }) {
  return (
    <div className="unsupported">
      <Icon name={entry.kind === "folder" ? "folder" : entry.kind} size={72} />
      <div className="name">{entry.name}</div>
      <div className="note">{title}</div>
      {info.mismatch && <MismatchNote entry={entry} info={info} />}
      <FactsPanel entry={entry} />
      {info.canOpen && onOpen && (
        <button className="btn primary" onClick={onOpen}>
          <Icon name="external" size={14} /> Open in system
        </button>
      )}
    </div>
  );
}
