import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { cachedThumb, frameUrl, onThumbsReset, requestScrubFrames, requestThumb, SCRUB_FRAMES, thumbsGeneration, type Entry } from "../api";
import { FolderGlyph, Icon } from "./Icon";

/** Lazily loaded thumbnail; only mounted while its tile is on screen. */
export function Thumb({ entry, fit = "contain", iconSize = 40 }: { entry: Entry; fit?: "cover" | "contain"; iconSize?: number }) {
  const thumbable = entry.kind === "photo" || entry.kind === "gif" || entry.kind === "video";
  const [url, setUrl] = useState<string | null | undefined>(() => (thumbable ? cachedThumb(entry) : null));
  const [loaded, setLoaded] = useState(false);
  // A tile can stay mounted while thumbnails are reset (e.g. Generate previews
  // in Safe Inspection Mode): one that has none yet must ask again.
  const generation = useSyncExternalStore(onThumbsReset, thumbsGeneration);
  const shown = useRef<{ key: string; url: string | null | undefined }>({ key: "", url: undefined });
  const key = `${entry.id}:${entry.modified}`;

  useEffect(() => {
    if (!thumbable) return;
    const hit = cachedThumb(entry);
    if (hit !== undefined) {
      shown.current = { key, url: hit };
      setUrl(hit);
      return;
    }
    // After a reset, a thumbnail already on screen stays; only missing ones are requested again.
    if (shown.current.key === key && typeof shown.current.url === "string") return;
    setUrl(undefined);
    let alive = true;
    const cancel = requestThumb(entry, (u) => {
      if (!alive) return;
      shown.current = { key, url: u };
      setUrl(u);
    });
    return () => {
      alive = false;
      cancel();
    };
  }, [entry.id, entry.modified, thumbable, generation]);

  // Hover scrub (videos): frames sampled earlier and re-encoded by the worker.
  const [frame, setFrame] = useState<number | null>(null);
  const [frames, setFrames] = useState<boolean | null>(null);
  const hoverTimer = useRef(0);
  const scrub =
    entry.kind === "video" && url
      ? {
          onMouseEnter: () => {
            if (frames === null) {
              const probe = new Image();
              probe.onload = () => setFrames(true);
              probe.onerror = () => {
                setFrames(false);
                // Linger to ask for frames in the background (once per video).
                hoverTimer.current = window.setTimeout(() => requestScrubFrames(entry, () => setFrames(true)), 700);
              };
              probe.src = frameUrl(entry.id, 0);
            }
          },
          onMouseMove: (e: React.MouseEvent) => {
            if (!frames) return;
            const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
            setFrame(Math.min(SCRUB_FRAMES - 1, Math.max(0, Math.floor(((e.clientX - r.left) / r.width) * SCRUB_FRAMES))));
          },
          onMouseLeave: () => {
            window.clearTimeout(hoverTimer.current);
            setFrame(null);
          },
        }
      : {};

  if (url) {
    return (
      <div className={`thumb ${fit}`} {...scrub}>
        {frame !== null && <img className="scrub loaded" src={frameUrl(entry.id, frame)} alt="" draggable={false} />}
        {frame !== null && (
          <span className="scrub-bar">
            <span style={{ width: `${((frame + 1) / SCRUB_FRAMES) * 100}%` }} />
          </span>
        )}
        <img
          src={url}
          alt=""
          draggable={false}
          decoding="async"
          className={loaded ? "loaded" : ""}
          onLoad={() => setLoaded(true)}
          onError={() => {
            shown.current = { key, url: null };
            setUrl(null);
          }}
        />
        {entry.kind === "video" && (
          <span className="badge">
            <Icon name="video" size={12} />
          </span>
        )}
        {entry.kind === "gif" && <span className="badge text">GIF</span>}
      </div>
    );
  }
  if (entry.kind === "folder") {
    return (
      <div className="thumb placeholder folder">
        <FolderGlyph size={iconSize * 1.6} />
      </div>
    );
  }
  return (
    <div className={`thumb placeholder ${url === undefined && thumbable ? "pending" : ""}`}>
      <Icon name={entry.kind} size={iconSize} stroke={1.3} />
      {entry.ext && url === null && <span className="ext">{entry.ext}</span>}
    </div>
  );
}
