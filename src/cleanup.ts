/**
 * Quick Cleanup: decide Keep / Mark for Trash one item after another, very
 * fast, without touching any file.
 *
 * This module is the whole review stage and it has no access to the
 * filesystem or to Mori's commands at all: a decision only changes this
 * in-memory state. Files move to the OS Trash only through `trashIds()` →
 * the existing `trash_items` command, after the review screen and one
 * explicit confirmation (see components/Cleanup.tsx).
 */

export type Decision = "keep" | "trash";

export type CleanupKind = "all" | "photo" | "video" | "gif" | "document" | "audio" | "other";
export type CleanupOrder = "browser" | "oldest" | "newest" | "largest" | "smallest";

export interface CleanupOptions {
  recursive: boolean;
  kind: CleanupKind;
  order: CleanupOrder;
}

/** What the queue needs to know about an item. */
export interface QueueItem {
  id: string;
  size: number;
  /** Inside a protected folder (Never Modify): it can't be marked. */
  guarded?: boolean;
}

interface Step {
  id: string;
  /** The decision before this step (undefined: undecided). */
  prev: Decision | undefined;
  /** Where the cursor was. */
  cursor: number;
}

export interface CleanupState<T extends QueueItem = QueueItem> {
  readonly items: readonly T[];
  readonly decisions: ReadonlyMap<string, Decision>;
  /** Item being reviewed; `items.length` = past the last one (the summary). */
  readonly cursor: number;
  /** Undo stack of decisions, newest last. */
  readonly steps: readonly Step[];
  /** When the last decision was accepted (ms), for the key-bounce guard. */
  readonly lastAt: number;
}

/** Two decisions closer than this are one bounced key press, not two. */
export const MIN_DECISION_INTERVAL_MS = 45;
const MAX_STEPS = 20_000;

export function createSession<T extends QueueItem>(items: readonly T[]): CleanupState<T> {
  return { items, decisions: new Map(), cursor: 0, steps: [], lastAt: -Infinity };
}

export const current = <T extends QueueItem>(s: CleanupState<T>): T | undefined => s.items[s.cursor];
export const finished = (s: CleanupState) => s.cursor >= s.items.length;

export type Refusal = "repeat" | "bounce" | "protected" | "finished";

/**
 * Record a decision for the current item and move on. Refused (state
 * unchanged) for a held-down key's auto-repeat, a bounce right after the
 * previous decision, a protected item marked for Trash, or past the end.
 * Reaching the end only shows the summary: nothing is ever trashed here.
 */
export function decide<T extends QueueItem>(
  s: CleanupState<T>,
  decision: Decision,
  now: number,
  opts: { repeat?: boolean } = {},
): { state: CleanupState<T>; refused?: Refusal } {
  const item = current(s);
  if (!item) return { state: s, refused: "finished" };
  if (opts.repeat) return { state: s, refused: "repeat" };
  if (now - s.lastAt < MIN_DECISION_INTERVAL_MS) return { state: s, refused: "bounce" };
  if (decision === "trash" && item.guarded) return { state: s, refused: "protected" };
  const decisions = new Map(s.decisions);
  decisions.set(item.id, decision);
  const steps = [...s.steps, { id: item.id, prev: s.decisions.get(item.id), cursor: s.cursor }];
  if (steps.length > MAX_STEPS) steps.splice(0, steps.length - MAX_STEPS);
  return { state: { ...s, decisions, cursor: s.cursor + 1, steps, lastAt: now } };
}

/** Undo the last decision: it's reverted and its item becomes current again. */
export function undo<T extends QueueItem>(s: CleanupState<T>): CleanupState<T> {
  const step = s.steps[s.steps.length - 1];
  if (!step) return s;
  const decisions = new Map(s.decisions);
  if (step.prev === undefined) decisions.delete(step.id);
  else decisions.set(step.id, step.prev);
  const at = s.items.findIndex((e) => e.id === step.id);
  return { ...s, decisions, steps: s.steps.slice(0, -1), cursor: at >= 0 ? at : Math.min(step.cursor, s.items.length) };
}

/** Previous / next item without deciding anything. */
export function go<T extends QueueItem>(s: CleanupState<T>, delta: number): CleanupState<T> {
  const cursor = Math.min(s.items.length, Math.max(0, s.cursor + delta));
  return cursor === s.cursor ? s : { ...s, cursor };
}

export function jump<T extends QueueItem>(s: CleanupState<T>, index: number): CleanupState<T> {
  return { ...s, cursor: Math.min(s.items.length, Math.max(0, index)) };
}

/** Review: put marked items back to Keep (all of them when `ids` is omitted). */
export function keep<T extends QueueItem>(s: CleanupState<T>, ids?: Iterable<string>): CleanupState<T> {
  const decisions = new Map(s.decisions);
  const which = ids ? [...ids] : [...decisions.keys()];
  let changed = false;
  for (const id of which) {
    if (decisions.get(id) === "trash") {
      decisions.set(id, "keep");
      changed = true;
    }
  }
  return changed ? { ...s, decisions } : s;
}

export interface Counts {
  total: number;
  kept: number;
  marked: number;
  undecided: number;
  markedBytes: number;
}

export function counts(s: CleanupState): Counts {
  let kept = 0;
  let marked = 0;
  let markedBytes = 0;
  for (const e of s.items) {
    const d = s.decisions.get(e.id);
    if (d === "keep") kept++;
    else if (d === "trash") {
      marked++;
      markedBytes += e.size;
    }
  }
  return { total: s.items.length, kept, marked, undecided: s.items.length - kept - marked, markedBytes };
}

/** Items marked for Trash, in queue order. */
export const markedItems = <T extends QueueItem>(s: CleanupState<T>): T[] => s.items.filter((e) => s.decisions.get(e.id) === "trash");

/** What the final, confirmed step sends to the existing Trash command. */
export const trashIds = (s: CleanupState): string[] => markedItems(s).map((e) => e.id);

/**
 * The folder changed underneath (items moved, trashed or renamed elsewhere,
 * a drive disconnected and rescanned): keep decisions for items still
 * there, drop the rest, and stay on the same item (or the next one).
 */
export function reconcile<T extends QueueItem>(s: CleanupState<T>, items: readonly T[]): CleanupState<T> {
  const present = new Set(items.map((e) => e.id));
  const cur = current(s);
  let cursor: number;
  if (cur && present.has(cur.id)) cursor = items.findIndex((e) => e.id === cur.id);
  else {
    // The next surviving item after the old position.
    const next = s.items.slice(s.cursor).find((e) => present.has(e.id));
    cursor = next ? items.findIndex((e) => e.id === next.id) : items.length;
  }
  const decisions = new Map([...s.decisions].filter(([id]) => present.has(id)));
  const steps = s.steps.filter((st) => present.has(st.id));
  return { ...s, items, decisions, cursor, steps };
}

// ------------------------------------------------------------ persistence

/** What Mori saves to resume a session (normal mode only): ids, never names. */
export interface SavedSession {
  folder: string;
  options: CleanupOptions;
  kept: string[];
  marked: string[];
  cursor: number;
  savedAt?: number;
}

export function toSaved(s: CleanupState, folder: string, options: CleanupOptions): SavedSession {
  const kept: string[] = [];
  const marked: string[] = [];
  for (const [id, d] of s.decisions) (d === "keep" ? kept : marked).push(id);
  return { folder, options, kept, marked, cursor: s.cursor };
}

/** Rebuild a saved session over the folder's current contents. */
export function fromSaved<T extends QueueItem>(items: readonly T[], saved: SavedSession): CleanupState<T> {
  const present = new Set(items.map((e) => e.id));
  const decisions = new Map<string, Decision>();
  for (const id of saved.kept) if (present.has(id)) decisions.set(id, "keep");
  for (const id of saved.marked) if (present.has(id)) decisions.set(id, "trash");
  const firstOpen = items.findIndex((e) => !decisions.has(e.id));
  const cursor = Math.min(items.length, Math.max(0, saved.cursor), firstOpen < 0 ? items.length : Math.max(firstOpen, 0));
  return { items, decisions, cursor, steps: [], lastAt: -Infinity };
}

/**
 * A temporary session or Private Inspection never saves its cleanup queue:
 * decisions stay in memory and end with the session.
 */
export const mayPersist = (status: { temporary: boolean }) => !status.temporary;

/** Final Move to Trash is available (the backend checks again regardless). */
export const mayTrash = (status: { readOnly: boolean }, readOnlySetting: boolean) => !status.readOnly && !readOnlySetting;

/** Matches the order options to the browser's sort keys. */
export function orderToSort(order: CleanupOrder, browser: { sort: string; desc: boolean }): { sort: string; desc: boolean } {
  switch (order) {
    case "oldest":
      return { sort: "modified", desc: false };
    case "newest":
      return { sort: "modified", desc: true };
    case "largest":
      return { sort: "size", desc: true };
    case "smallest":
      return { sort: "size", desc: false };
    default:
      return browser;
  }
}
