import { describe, expect, it } from "vitest";
import {
  counts,
  createSession,
  current,
  decide,
  finished,
  fromSaved,
  go,
  keep,
  markedItems,
  mayPersist,
  mayTrash,
  MIN_DECISION_INTERVAL_MS,
  reconcile,
  toSaved,
  trashIds,
  undo,
  type CleanupState,
  type QueueItem,
} from "../cleanup";
import * as cleanup from "../cleanup";

const items: QueueItem[] = Array.from({ length: 6 }, (_, i) => ({ id: `f${i}`, size: (i + 1) * 100 }));
const opts = { recursive: false, kind: "all" as const, order: "browser" as const };

/** Apply decisions 100 ms apart (a fast but normal pace). */
function run(s: CleanupState, keys: ("keep" | "trash")[], start = 1000, gap = 100) {
  keys.forEach((k, i) => (s = decide(s, k, start + i * gap).state));
  return s;
}

describe("Quick Cleanup staging", () => {
  it("Keep and Mark only record decisions and advance", () => {
    const s = run(createSession(items), ["keep", "trash", "keep"]);
    expect(s.cursor).toBe(3);
    expect(current(s)?.id).toBe("f3");
    expect(counts(s)).toEqual({ total: 6, kept: 2, marked: 1, undecided: 3, markedBytes: 200 });
  });

  it("the review stage has no way to touch files", () => {
    // The module exposes no command, no I/O: only pure state functions and constants.
    for (const [name, value] of Object.entries(cleanup)) {
      if (typeof value === "function") expect((value as (...a: unknown[]) => unknown).toString()).not.toMatch(/invoke|trash_items|api\./);
      else expect(["MIN_DECISION_INTERVAL_MS"]).toContain(name);
    }
  });

  it("reaching the end shows the summary and never trashes anything by itself", () => {
    let s = run(createSession(items), ["trash", "trash", "trash", "trash", "trash", "trash"]);
    expect(finished(s)).toBe(true);
    const more = decide(s, "trash", 99_999);
    expect(more.refused).toBe("finished");
    s = more.state;
    expect(trashIds(s)).toEqual(items.map((e) => e.id));
  });

  it("Undo reverts the last decision and goes back to its item", () => {
    let s = run(createSession(items), ["keep", "trash"]);
    s = undo(s);
    expect(s.cursor).toBe(1);
    expect(s.decisions.has("f1")).toBe(false);
    s = undo(s);
    expect(s.cursor).toBe(0);
    expect(s.decisions.size).toBe(0);
    expect(undo(s)).toBe(s);
  });

  it("Undo of a changed decision restores the earlier one", () => {
    let s = run(createSession(items), ["trash"]);
    s = go(s, -1);
    s = decide(s, "keep", 5000).state;
    expect(s.decisions.get("f0")).toBe("keep");
    s = undo(s);
    expect(s.decisions.get("f0")).toBe("trash");
  });

  it("Back / forward move without deciding", () => {
    let s = run(createSession(items), ["keep", "keep"]);
    s = go(s, -1);
    expect(current(s)?.id).toBe("f1");
    expect(counts(s).kept).toBe(2);
    expect(go(go(s, -10), -1).cursor).toBe(0);
  });

  it("a held-down key marks one item, not hundreds", () => {
    let s = createSession(items);
    s = decide(s, "trash", 1000).state;
    // Auto-repeat events from the same held key, at the OS repeat rate.
    for (let t = 1030; t < 3000; t += 33) s = decide(s, "trash", t, { repeat: true }).state;
    expect(counts(s).marked).toBe(1);
  });

  it("a bounced key press counts once, rapid alternating input all counts", () => {
    let s = createSession(items);
    s = decide(s, "trash", 1000).state;
    const bounce = decide(s, "trash", 1000 + MIN_DECISION_INTERVAL_MS - 10);
    expect(bounce.refused).toBe("bounce");
    s = run(s, ["keep", "trash", "keep", "trash", "keep"], 1100, 60);
    expect(counts(s)).toMatchObject({ kept: 3, marked: 3, undecided: 0 });
  });

  it("protected items can't be marked", () => {
    const s = createSession([{ id: "p", size: 1, guarded: true }, ...items]);
    const r = decide(s, "trash", 1000);
    expect(r.refused).toBe("protected");
    expect(r.state.decisions.size).toBe(0);
    expect(decide(s, "keep", 1000).state.decisions.get("p")).toBe("keep");
  });

  it("review: restore selected, keep all", () => {
    let s = run(createSession(items), ["trash", "trash", "keep", "trash"]);
    expect(markedItems(s).map((e) => e.id)).toEqual(["f0", "f1", "f3"]);
    s = keep(s, ["f1"]);
    expect(trashIds(s)).toEqual(["f0", "f3"]);
    s = keep(s);
    expect(trashIds(s)).toEqual([]);
    expect(counts(s).kept).toBe(4);
  });

  it("a file that disappears mid-session leaves the queue cleanly", () => {
    let s = run(createSession(items), ["trash", "keep"]);
    // f2 is current; f0 (marked) and f2 vanish (moved elsewhere, drive rescanned…).
    s = reconcile(
      s,
      items.filter((e) => e.id !== "f0" && e.id !== "f2"),
    );
    expect(current(s)?.id).toBe("f3");
    expect(trashIds(s)).toEqual([]);
    expect(counts(s)).toMatchObject({ total: 4, kept: 1 });
    // Undo never resurrects a vanished item.
    expect(undo(s).decisions.has("f0")).toBe(false);
  });

  it("everything disappearing (a drive disconnected) ends at an empty summary", () => {
    const s = reconcile(run(createSession(items), ["trash"]), []);
    expect(finished(s)).toBe(true);
    expect(trashIds(s)).toEqual([]);
  });

  it("saves only ids and decisions, and resumes over the current contents", () => {
    const s = run(createSession(items), ["keep", "trash", "trash"]);
    const saved = toSaved(s, "folder", opts);
    expect(saved).toEqual({ folder: "folder", options: opts, kept: ["f0"], marked: ["f1", "f2"], cursor: 3 });
    expect(JSON.stringify(saved)).not.toMatch(/size|name|path/);
    // f1 vanished since; a new file appeared.
    const now = [...items.filter((e) => e.id !== "f1"), { id: "new", size: 5 }];
    const r = fromSaved(now, saved);
    expect(trashIds(r)).toEqual(["f2"]);
    expect(current(r)?.id).toBe("f3");
  });

  it("temporary sessions and Private Inspection never persist; read-only never trashes", () => {
    expect(mayPersist({ temporary: true })).toBe(false);
    expect(mayPersist({ temporary: false })).toBe(true);
    expect(mayTrash({ readOnly: true }, false)).toBe(false);
    expect(mayTrash({ readOnly: false }, true)).toBe(false);
    expect(mayTrash({ readOnly: false }, false)).toBe(true);
  });

  it("stays fast with 10k+ items", () => {
    const big: QueueItem[] = Array.from({ length: 12_000 }, (_, i) => ({ id: `b${i}`, size: i }));
    let s = createSession(big);
    const t = performance.now();
    for (let i = 0; i < 300; i++) s = decide(s, i % 3 ? "keep" : "trash", 1000 + i * 100).state;
    s = reconcile(s, big.slice(10));
    counts(s);
    expect(performance.now() - t).toBeLessThan(1500);
    expect(s.cursor).toBe(290);
  });
});
