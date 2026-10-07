/**
 * Internal drag and drop: Mori items onto Mori folders.
 *
 * A drag only expresses an intent. What is dragged is kept here, in memory,
 * as entries (opaque ids); the drag itself carries only an opaque Mori marker,
 * never a path, so nothing leaks to other apps if the pointer leaves the
 * window. A drop runs the same backend Move as "Move to…", which re-checks
 * everything (Read-only Mode, protected folders, conflicts, folder into
 * itself…). The checks here only drive the visual feedback.
 */

export const DRAG_TYPE = "application/x-mori-items";

/** What the checks need to know about an item. */
export interface DragItem {
  id: string;
  /** Display path relative to the root ("Photos/2024/a.jpg"). */
  path: string;
  kind: string;
  guarded?: boolean;
}

let current: DragItem[] | null = null;

export function beginDrag(items: DragItem[]) {
  current = items.length ? items : null;
}
export const dragged = (): DragItem[] | null => current;
export function endDrag() {
  current = null;
  // Drop-target marks set outside React (sidebar rows) never outlive a drag.
  if (typeof document !== "undefined") document.querySelectorAll<HTMLElement>("[data-drop]").forEach((el) => el.removeAttribute("data-drop"));
}

const parentOf = (p: string) => (p.includes("/") ? p.slice(0, p.lastIndexOf("/")) : "");

/**
 * The items a drag starts with: the whole selection when the grabbed item
 * is part of it, otherwise only the grabbed item (which becomes the
 * selection, like Finder).
 */
export function dragSource<T extends { id: string }>(grabbed: T, selected: readonly T[]): { items: T[]; reselect: boolean } {
  if (selected.some((e) => e.id === grabbed.id)) return { items: [...selected], reselect: false };
  return { items: [grabbed], reselect: true };
}

/**
 * Whether `items` may be dropped into `target` (a folder; path "" = the
 * root), and why not. The backend checks again on drop.
 */
export function canDropInto(
  target: { id: string; path: string; kind?: string; guarded?: boolean },
  items: readonly DragItem[],
  opts: { readOnly: boolean },
): { ok: true } | { ok: false; reason: string } {
  if (!items.length) return { ok: false, reason: "Nothing to move." };
  if (target.kind && target.kind !== "folder") return { ok: false, reason: "Not a folder." };
  if (opts.readOnly) return { ok: false, reason: "Read-only: Mori won't change files right now." };
  if (target.guarded) return { ok: false, reason: "Protected (Never Modify): nothing can be moved into it." };
  for (const e of items) {
    if (e.id === target.id) return { ok: false, reason: "A folder can't be moved into itself." };
    if (e.kind === "folder" && (target.path === e.path || target.path.startsWith(`${e.path}/`)))
      return { ok: false, reason: "A folder can't be moved into one of its own subfolders." };
    if (e.guarded) return { ok: false, reason: "A protected (Never Modify) item can't be moved." };
  }
  if (items.every((e) => parentOf(e.path) === target.path)) return { ok: false, reason: "Already in this folder." };
  return { ok: true };
}

/** Hover this long over a collapsed sidebar folder (or a folder card) to open it. */
export const SPRING_MS = 750;

/**
 * Scroll `el` when the pointer is near its top or bottom edge during a drag,
 * so long sidebars and grids can be reached.
 */
export function edgeScroll(el: HTMLElement, clientY: number) {
  const r = el.getBoundingClientRect();
  const zone = 36;
  if (clientY < r.top + zone) el.scrollTop -= Math.ceil(((r.top + zone - clientY) / zone) * 14);
  else if (clientY > r.bottom - zone) el.scrollTop += Math.ceil(((clientY - (r.bottom - zone)) / zone) * 14);
}

/**
 * The drag image: one thumbnail (or icon) and "5 items", never a stack of
 * full thumbnails. Built off-screen and removed right after the browser
 * snapshots it.
 */
export function dragGhost(dt: DataTransfer, count: number, name: string, thumb: HTMLElement | null) {
  const ghost = document.createElement("div");
  ghost.className = "drag-ghost";
  const pic = document.createElement("div");
  pic.className = "drag-ghost-pic";
  const img = thumb?.querySelector("img.loaded, img") as HTMLImageElement | null;
  if (img?.src) {
    const copy = document.createElement("img");
    copy.src = img.src;
    pic.appendChild(copy);
  } else {
    const svg = thumb?.querySelector("svg");
    if (svg) pic.appendChild(svg.cloneNode(true));
  }
  const label = document.createElement("span");
  label.textContent = count > 1 ? `${count.toLocaleString()} items` : name;
  ghost.append(pic, label);
  if (count > 1) {
    const badge = document.createElement("span");
    badge.className = "drag-ghost-badge";
    badge.textContent = String(count);
    ghost.appendChild(badge);
  }
  document.body.appendChild(ghost);
  try {
    dt.setDragImage(ghost, 24, 24);
  } catch {
    // Some engines refuse custom drag images: the default one is fine.
  }
  window.setTimeout(() => ghost.remove(), 0);
}

/**
 * Window-level drag handling, installed once:
 * - anywhere that isn't a drop target refuses the drop ("not allowed"
 *   cursor), and nothing dropped from outside Mori (Finder files, text) is
 *   ever opened or navigated to;
 * - a drag whose source disappeared (e.g. after spring-loading into a
 *   folder) may never get its `dragend`: the next pointer press clears it.
 */
export function installDragGuards() {
  window.addEventListener("dragover", (e) => {
    if (e.defaultPrevented) return; // a Mori drop target accepted or refused it
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "none";
  });
  window.addEventListener("drop", (e) => {
    e.preventDefault();
    endDrag();
  });
  window.addEventListener("dragend", () => endDrag(), true);
  window.addEventListener("pointerdown", () => endDrag(), true);
}
