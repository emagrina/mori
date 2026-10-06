import { useEffect, useState } from "react";
import { cachedThumb, requestThumb, type Entry } from "../api";
import { FolderGlyph, Icon } from "./Icon";

/** Lazily loaded thumbnail; only mounted while its tile is on screen. */
export function Thumb({ entry, fit = "contain", iconSize = 40 }: { entry: Entry; fit?: "cover" | "contain"; iconSize?: number }) {
  const thumbable = entry.kind === "photo" || entry.kind === "gif" || entry.kind === "video";
  const [url, setUrl] = useState<string | null | undefined>(() => (thumbable ? cachedThumb(entry) : null));
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    if (!thumbable) return;
    const hit = cachedThumb(entry);
    if (hit !== undefined) {
      setUrl(hit);
      return;
    }
    setUrl(undefined);
    let alive = true;
    const cancel = requestThumb(entry, (u) => alive && setUrl(u));
    return () => {
      alive = false;
      cancel();
    };
  }, [entry.id, entry.modified, thumbable]);

  if (url) {
    return (
      <div className={`thumb ${fit}`}>
        <img
          src={url}
          alt=""
          draggable={false}
          decoding="async"
          className={loaded ? "loaded" : ""}
          onLoad={() => setLoaded(true)}
          onError={() => setUrl(null)}
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
