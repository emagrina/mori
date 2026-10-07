// @vitest-environment jsdom
import { cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import type { TagInfo } from "../api";
import { TagDeleteDialog, TagList, tagNameProblem, TagRenameDialog } from "../components/Organize";

const calls: [string, unknown][] = [];
beforeAll(() => {
  (window as unknown as { __TAURI_INTERNALS__: object }).__TAURI_INTERNALS__ = {
    convertFileSrc: (p: string) => p,
    invoke: async (cmd: string, args: unknown) => {
      calls.push([cmd, args]);
      return null;
    },
  };
});
beforeEach(() => (calls.length = 0));
afterEach(cleanup);

const tags: TagInfo[] = [
  { id: 1, name: "Test", count: 24 },
  { id: 2, name: "Travel", count: 127 },
  { id: 3, name: "Empty", count: 0 },
];

describe("tag names", () => {
  it("rejects empty, whitespace-only, too long and duplicate names (any case); never merges", () => {
    expect(tagNameProblem("", 1, tags)).toMatch(/needs a name/);
    expect(tagNameProblem("   ", 1, tags)).toMatch(/needs a name/);
    expect(tagNameProblem("x".repeat(61), 1, tags)).toMatch(/too long/);
    expect(tagNameProblem("travel", 1, tags)).toMatch(/already exists.*aren't merged/);
    expect(tagNameProblem(" TRAVEL ", 1, tags)).not.toBeNull();
  });
  it("accepts a case-only rename of the same tag, Unicode, and trims", () => {
    expect(tagNameProblem("TEST", 1, tags)).toBeNull();
    expect(tagNameProblem("  Personal ", 1, tags)).toBeNull();
    expect(tagNameProblem("Viaje ✈️ 日本", 1, tags)).toBeNull();
  });
});

describe("sidebar tags", () => {
  const setup = (canEdit = true) => {
    const fns = { onOpen: vi.fn(), onRename: vi.fn(), onDelete: vi.fn() };
    const r = render(<TagList tags={tags} active={2} canEdit={canEdit} {...fns} />);
    return { ...r, fns };
  };

  it("shows names and counts, and marks the open tag", () => {
    const { getByText, container } = setup();
    expect(getByText("Travel").closest("button")?.className).toContain("on");
    expect([...container.querySelectorAll(".count")].map((c) => c.textContent)).toEqual(["24", "127", "0"]);
  });

  it("right-click opens Rename / Delete without opening the tag; Escape closes", () => {
    const { getByText, getByRole, queryByRole, fns } = setup();
    fireEvent.contextMenu(getByText("Test"));
    expect(fns.onOpen).not.toHaveBeenCalled();
    const menu = getByRole("menu", { name: "Tag “Test”" });
    expect(menu.className).toBe("menu");
    expect([...menu.querySelectorAll("[role=menuitem]")].map((b) => b.textContent?.trim())).toEqual(["Rename Tag…", "Delete Tag…"]);
    fireEvent.keyDown(window, { key: "Escape" });
    expect(queryByRole("menu")).toBeNull();
  });

  it("clicking outside closes the menu; items reach the right tag", () => {
    const { getByText, getByRole, queryByRole, fns } = setup();
    fireEvent.contextMenu(getByText("Travel"));
    fireEvent.click(document.body);
    expect(queryByRole("menu")).toBeNull();
    fireEvent.contextMenu(getByText("Travel"));
    fireEvent.click(getByRole("menuitem", { name: /Delete Tag/ }));
    expect(fns.onDelete).toHaveBeenCalledWith(tags[1]);
    fireEvent.contextMenu(getByText("Test"));
    fireEvent.click(getByRole("menuitem", { name: /Rename Tag/ }));
    expect(fns.onRename).toHaveBeenCalledWith(tags[0]);
  });

  it("the keyboard context-menu key opens it too", () => {
    const { getByText, getByRole } = setup();
    fireEvent.keyDown(getByText("Empty"), { key: "ContextMenu" });
    expect(getByRole("menu", { name: "Tag “Empty”" })).toBeTruthy();
  });

  it("in a temporary session the actions are shown disabled", () => {
    const { getByText, getByRole } = setup(false);
    fireEvent.contextMenu(getByText("Test"));
    expect((getByRole("menuitem", { name: /Rename Tag/ }) as HTMLButtonElement).disabled).toBe(true);
    expect((getByRole("menuitem", { name: /Delete Tag/ }) as HTMLButtonElement).disabled).toBe(true);
  });
});

describe("rename dialog", () => {
  it("renames the same tag (by id), trimmed", async () => {
    const onDone = vi.fn();
    const { getByLabelText, getByText } = render(<TagRenameDialog tag={tags[0]} tags={tags} onCancel={() => {}} onDone={onDone} />);
    fireEvent.change(getByLabelText("New tag name"), { target: { value: "  Personal  " } });
    fireEvent.click(getByText("Rename"));
    await waitFor(() => expect(onDone).toHaveBeenCalled());
    expect(calls).toEqual([["tag_rename", { id: 1, name: "Personal" }]]);
  });
  it("refuses a duplicate name without calling the backend", () => {
    const onDone = vi.fn();
    const { getByLabelText, getByText } = render(<TagRenameDialog tag={tags[0]} tags={tags} onCancel={() => {}} onDone={onDone} />);
    fireEvent.change(getByLabelText("New tag name"), { target: { value: "TRAVEL" } });
    fireEvent.click(getByText("Rename"));
    expect(getByText(/already exists/)).toBeTruthy();
    expect(calls).toEqual([]);
    expect(onDone).not.toHaveBeenCalled();
  });
});

describe("delete dialog", () => {
  it("says the tag is removed from items and that no files are deleted", async () => {
    const onDone = vi.fn();
    const { container, getByText } = render(<TagDeleteDialog tag={tags[1]} onCancel={() => {}} onDone={onDone} />);
    const text = container.textContent ?? "";
    expect(text).toContain("Delete tag “Travel”?");
    expect(text).toContain("This removes the tag from 127 items.");
    expect(text).toContain("No files are deleted or changed.");
    expect(text).not.toMatch(/Delete 127 items/i);
    fireEvent.click(getByText("Delete Tag"));
    await waitFor(() => expect(onDone).toHaveBeenCalled());
    expect(calls).toEqual([["tag_delete", { id: 2 }]]);
  });
  it("an empty tag gets the simpler wording", () => {
    const { container } = render(<TagDeleteDialog tag={tags[2]} onCancel={() => {}} onDone={() => {}} />);
    expect(container.textContent).toContain("No items have this tag.");
  });
});
