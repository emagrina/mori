import { useMemo, useState } from "react";
import { api, plural, type Entry, type TagInfo } from "../api";
import { Icon } from "./Icon";
import { ModalFrame } from "./Modal";

/**
 * Tags for one or more items. Tags live in Mori's app data only: the files
 * and their metadata are never changed.
 */
export function TagDialog({ entries, tags, onClose, onDone }: { entries: Entry[]; tags: TagInfo[]; onClose: () => void; onDone: (msg: string) => void }) {
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const ids = entries.map((e) => e.id);
  const state = (t: TagInfo) => {
    const n = entries.filter((e) => e.tags?.includes(t.id)).length;
    return n === 0 ? "none" : n === entries.length ? "all" : "some";
  };
  const run = async (tagName: string, on: boolean) => {
    setBusy(true);
    setError(null);
    try {
      await api.tagItems(ids, tagName, on);
      onDone(on ? `Tagged ${entries.length === 1 ? `“${entries[0].name}”` : plural(entries.length, "item")} “${tagName}”` : `Removed “${tagName}”`);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  const filtered = tags.filter((t) => !name.trim() || t.name.toLowerCase().includes(name.trim().toLowerCase()));
  const exact = tags.some((t) => t.name.toLowerCase() === name.trim().toLowerCase());
  return (
    <ModalFrame onCancel={onClose}>
      <div className="dialog-icon">
        <Icon name="tag" size={20} />
      </div>
      <h2>Tags for {entries.length === 1 ? `“${entries[0].name}”` : plural(entries.length, "item")}</h2>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (name.trim()) run(name.trim(), true);
        }}
      >
        <input className="text-input" value={name} onChange={(e) => setName(e.target.value)} placeholder="Find or create a tag" autoFocus maxLength={60} spellCheck={false} />
      </form>
      <div className="tag-pick">
        {filtered.map((t) => {
          const st = state(t);
          return (
            <button key={t.id} className={`tag-option ${st}`} disabled={busy} onClick={() => run(t.name, st !== "all")}>
              <span className="check-box" aria-hidden>
                {st === "all" && <Icon name="check" size={12} stroke={2.2} />}
                {st === "some" && <span className="dash" />}
              </span>
              <span className="truncate">{t.name}</span>
              <span className="muted">{t.count.toLocaleString()}</span>
            </button>
          );
        })}
        {name.trim() && !exact && (
          <button className="tag-option create" disabled={busy} onClick={() => run(name.trim(), true)}>
            <Icon name="tag" size={13} /> Create “{name.trim()}”
          </button>
        )}
        {!tags.length && !name.trim() && <p className="muted small">No tags yet. Type a name to create one.</p>}
      </div>
      {error && <p className="field-error">{error}</p>}
      <p className="dialog-note">Tags are kept by Mori on this computer. The files aren't changed.</p>
      <div className="dialog-actions">
        <button className="btn" onClick={onClose}>
          Done
        </button>
      </div>
    </ModalFrame>
  );
}

/** Rename and delete tags. Deleting a tag never touches the tagged files. */
export function TagManager({ tags, onClose, onChanged }: { tags: TagInfo[]; onClose: () => void; onChanged: (msg: string) => void }) {
  const [query, setQuery] = useState("");
  const [editing, setEditing] = useState<{ id: number; name: string } | null>(null);
  const [confirm, setConfirm] = useState<TagInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const shown = useMemo(() => tags.filter((t) => t.name.toLowerCase().includes(query.trim().toLowerCase())), [tags, query]);
  const act = async (f: () => Promise<void>, msg: string) => {
    setError(null);
    try {
      await f();
      onChanged(msg);
    } catch (e) {
      setError(String(e));
    }
  };
  return (
    <ModalFrame onCancel={onClose}>
      <h2>Manage Tags</h2>
      <input className="text-input" value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search tags" spellCheck={false} />
      <div className="tag-pick">
        {shown.map((t) =>
          editing?.id === t.id ? (
            <form
              key={t.id}
              className="tag-option editing"
              onSubmit={(e) => {
                e.preventDefault();
                act(() => api.tagRename(t.id, editing.name), `Renamed to “${editing.name.trim()}”`).then(() => setEditing(null));
              }}
            >
              <input className="text-input slim" value={editing.name} onChange={(e) => setEditing({ id: t.id, name: e.target.value })} autoFocus maxLength={60} />
              <button className="btn small" type="submit">
                Save
              </button>
              <button className="btn small" type="button" onClick={() => setEditing(null)}>
                Cancel
              </button>
            </form>
          ) : confirm?.id === t.id ? (
            <div key={t.id} className="tag-option editing">
              <span className="truncate">
                Delete “{t.name}” from {plural(t.count, "item")}? The files stay as they are.
              </span>
              <button className="btn small danger-fill primary" onClick={() => act(() => api.tagDelete(t.id), `Deleted tag “${t.name}”`).then(() => setConfirm(null))}>
                Delete
              </button>
              <button className="btn small" onClick={() => setConfirm(null)}>
                Cancel
              </button>
            </div>
          ) : (
            <div key={t.id} className="tag-option">
              <Icon name="tag" size={13} />
              <span className="truncate">{t.name}</span>
              <span className="muted">{t.count.toLocaleString()}</span>
              <button className="icon-btn" onClick={() => setEditing({ id: t.id, name: t.name })} title="Rename">
                <Icon name="rename" size={13} />
              </button>
              <button className="icon-btn" onClick={() => setConfirm(t)} title="Delete tag">
                <Icon name="trash" size={13} />
              </button>
            </div>
          ),
        )}
        {!shown.length && <p className="muted small">{tags.length ? "No tag matches." : "No tags yet. Use “Tags…” on a file or folder to add one."}</p>}
      </div>
      {error && <p className="field-error">{error}</p>}
      <div className="dialog-actions">
        <button className="btn" onClick={onClose}>
          Done
        </button>
      </div>
    </ModalFrame>
  );
}
