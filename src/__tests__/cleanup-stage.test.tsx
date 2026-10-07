// @vitest-environment jsdom
import { cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import type { Entry, Status } from "../api";
import { QuickCleanup } from "../components/Cleanup";

// Regression guard for portrait media in Quick Cleanup (720×1280 clips were
// laid out 2276 px tall and cropped). jsdom has no layout engine, so this
// pins the structure that makes the layout correct; the real-WebKit
// measurements are in the PR.

const video: Entry = { id: "0000000000000001", name: "ACTH1426.MP4", path: "ACTH1426.MP4", ext: "mp4", kind: "video", size: 1, modified: 0, created: null };
const photo: Entry = { ...video, id: "0000000000000002", name: "p.jpg", path: "p.jpg", ext: "jpg", kind: "photo" };

beforeAll(() => {
  const playable = { status: "playable", container: "MP4", videoCodec: "avc1", audioCodec: null, width: 720, height: 1280, durationMs: 3000 };
  const handlers: Record<string, (a: Record<string, unknown>) => unknown> = {
    query: () => ({ items: [video, photo], total: 2, truncated: false, crumbs: [] }),
    cleanup_saved: () => null,
    inspect: (a) =>
      a.id === video.id
        ? { detected: "mp4", preview: "video", canOpen: false, mismatch: false, previewsOff: false, video: playable }
        : { detected: "jpeg", preview: "image", canOpen: false, mismatch: false, previewsOff: false, video: null },
    video_session_start: () => 1,
  };
  (window as unknown as { __TAURI_INTERNALS__: object }).__TAURI_INTERNALS__ = {
    convertFileSrc: (p: string) => `mori://${p}`,
    invoke: async (cmd: string, args: Record<string, unknown>) => handlers[cmd]?.(args ?? {}) ?? null,
    transformCallback: () => 1,
  };
  HTMLMediaElement.prototype.play = async () => {};
  HTMLMediaElement.prototype.pause = () => {};
  HTMLMediaElement.prototype.load = () => {};
});
afterEach(cleanup);

const status: Status = {
  hasRoot: true,
  rootName: "Pictures",
  scanning: false,
  scanCount: 0,
  fileCount: 2,
  scannedAt: 0,
  safeMode: false,
  decoded: 0,
  temporary: false,
  privateInspection: false,
  readOnly: false,
};

describe("Quick Cleanup media stage", () => {
  it("shows media inside the normal viewer's stage (one sizing system, fit the whole media)", async () => {
    const { container, getByText } = render(
      <QuickCleanup
        folder={{ id: "", name: "Pictures" }}
        status={status}
        readOnly={false}
        browserSort={{ sort: "name", desc: false }}
        recursiveDefault={false}
        indexVersion={0}
        onClose={() => {}}
        onToast={() => {}}
        onTrashed={() => {}}
        onUndo={() => {}}
      />,
    );
    await waitFor(() => expect((getByText(/Start/).closest("button") as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(getByText(/Start/));
    const v = await waitFor(() => {
      const el = container.querySelector("video");
      expect(el).not.toBeNull();
      return el!;
    });
    const stage = v.closest(".cleanup-stage")!;
    expect(stage.classList.contains("preview-stage")).toBe(true);
    // Same chain as the viewer: video.video → .video-stack → .preview-stage.
    expect(v.classList.contains("video")).toBe(true);
    expect(v.parentElement?.classList.contains("video-stack")).toBe(true);
    expect(v.parentElement?.parentElement).toBe(stage);
  });

  it("the stylesheet never turns the stage back into an auto-row grid", () => {
    const css = readFileSync(resolve("src/styles.css"), "utf8");
    const rules = [...css.matchAll(/\.cleanup-stage\s*\{([^}]*)\}/g)].map((m) => m[1]);
    expect(rules.length).toBeGreaterThan(0);
    for (const r of rules) {
      expect(r).not.toMatch(/display:\s*grid/);
      expect(r).not.toMatch(/height:\s*\d/); // no fixed height: the stage is sized by the viewer's flex rules
    }
    // The viewer stage keeps the property that makes height: 100% definite.
    const viewer = css.match(/\.preview-stage\s*\{([^}]*)\}/)![1];
    expect(viewer).toMatch(/min-height:\s*0/);
    expect(viewer).toMatch(/display:\s*flex/);
  });
});
