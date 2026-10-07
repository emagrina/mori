import { describe, expect, it } from "vitest";
import { beginDrag, canDropInto, dragged, dragSource, endDrag } from "../dnd";

const f = (path: string, kind = "photo", extra: object = {}) => ({ id: path, path, kind, ...extra });
const archive = { id: "Archive", path: "Photos/Archive", kind: "folder" };
const rw = { readOnly: false };

describe("drag sources", () => {
  const a = f("Photos/1.jpg");
  const b = f("Photos/2.jpg");
  const c = f("Photos/3.jpg");
  it("grabbing a selected item drags the whole selection", () => {
    expect(dragSource(b, [a, b, c])).toEqual({ items: [a, b, c], reselect: false });
  });
  it("grabbing an unselected item drags only it, and it becomes the selection", () => {
    expect(dragSource(c, [a, b])).toEqual({ items: [c], reselect: true });
    expect(dragSource(a, [])).toEqual({ items: [a], reselect: true });
  });
  it("keeps what is dragged in memory, not in the drag data", () => {
    beginDrag([a, b]);
    expect(dragged()?.map((e) => e.id)).toEqual([a.id, b.id]);
    endDrag();
    expect(dragged()).toBeNull();
  });
});

describe("drop targets", () => {
  it("accepts files into another folder", () => {
    expect(canDropInto(archive, [f("Photos/1.jpg"), f("Photos/2024/2.jpg")], rw).ok).toBe(true);
  });
  it("refuses the folder the items are already in", () => {
    const r = canDropInto({ id: "P", path: "Photos", kind: "folder" }, [f("Photos/1.jpg"), f("Photos/2.jpg")], rw);
    expect(r).toEqual({ ok: false, reason: "Already in this folder." });
    // …but a mix from several folders may go there.
    expect(canDropInto({ id: "P", path: "Photos", kind: "folder" }, [f("Photos/1.jpg"), f("Other/2.jpg")], rw).ok).toBe(true);
  });
  it("refuses a folder into itself or one of its subfolders", () => {
    const folder = f("Photos", "folder");
    expect(canDropInto({ id: "Photos", path: "Photos", kind: "folder" }, [folder], rw).ok).toBe(false);
    expect(canDropInto(archive, [folder], rw)).toMatchObject({ ok: false, reason: expect.stringContaining("own subfolders") });
    // A sibling whose name only starts the same is fine.
    expect(canDropInto({ id: "x", path: "Photos 2", kind: "folder" }, [folder], rw).ok).toBe(true);
  });
  it("refuses protected folders, protected items, Read-only Mode and Private Inspection, and non-folders", () => {
    const items = [f("Elsewhere/1.jpg")];
    expect(canDropInto({ ...archive, guarded: true }, items, rw).ok).toBe(false);
    expect(canDropInto(archive, [f("Vault/1.jpg", "photo", { guarded: true })], rw).ok).toBe(false);
    expect(canDropInto(archive, items, { readOnly: true })).toMatchObject({ ok: false, reason: expect.stringContaining("Read-only") });
    expect(canDropInto({ id: "f", path: "a.jpg", kind: "photo" }, items, rw).ok).toBe(false);
    expect(canDropInto(archive, [], rw).ok).toBe(false);
  });
  it("accepts the drive root as a target", () => {
    expect(canDropInto({ id: "", path: "", kind: "folder" }, [f("Photos/1.jpg")], rw).ok).toBe(true);
    expect(canDropInto({ id: "", path: "", kind: "folder" }, [f("1.jpg")], rw).ok).toBe(false);
  });
});
