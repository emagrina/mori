// @vitest-environment jsdom
import { cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import type { Entry } from "../api";
import { frameFraction, frameWidth, MAX_ASPECT, MIN_ASPECT } from "../filmstrip";
import { Filmstrip } from "../components/Viewers";

describe("filmstrip geometry", () => {
  const H = 42;
  const cases: [string, number, number, number][] = [
    ["1920×1080", 1920, 1080, 75],
    ["1080×1920", 1080, 1920, 24],
    ["1080×1080", 1080, 1080, 42],
    ["352×640 (phone portrait)", 352, 640, 23],
    ["640×352", 640, 352, 76],
  ];
  for (const [name, w, h, px] of cases) {
    it(`${name}: fixed height, width from the aspect ratio`, () => expect(frameWidth(w / h, H)).toBe(px));
  }
  it("portrait frames are narrower than tall; landscape wider", () => {
    expect(frameWidth(352 / 640, H)).toBeLessThan(H);
    expect(frameWidth(16 / 9, H)).toBeGreaterThan(H);
  });
  it("clamps pathological ratios (the frame is letterboxed, never cropped)", () => {
    expect(frameWidth(10, H)).toBe(Math.round(H * MAX_ASPECT));
    expect(frameWidth(0.05, H)).toBe(Math.round(H * MIN_ASPECT));
    expect(frameWidth(null, H)).toBe(75); // unknown: 16:9 until a frame is measured
    expect(frameWidth(NaN, H)).toBe(75);
  });
  it("maps each frame to the moment it shows (start, middle, end)", () => {
    expect(frameFraction(0, 8)).toBe(1 / 16);
    expect(frameFraction(4, 8)).toBe(9 / 16);
    expect(frameFraction(7, 8)).toBe(15 / 16);
  });
});

// jsdom loads no images: frames "exist" and report a size when asked.
let natural = { w: 352, h: 640 };
beforeAll(() => {
  class FakeImage {
    onload: (() => void) | null = null;
    onerror: (() => void) | null = null;
    naturalWidth = 0;
    naturalHeight = 0;
    set src(_: string) {
      this.naturalWidth = natural.w;
      this.naturalHeight = natural.h;
      queueMicrotask(() => this.onload?.());
    }
  }
  vi.stubGlobal("Image", FakeImage);
  (window as unknown as { __TAURI_INTERNALS__: object }).__TAURI_INTERNALS__ = { convertFileSrc: (p: string) => `mori://${p}`, invoke: async () => null };
});
afterEach(cleanup);

const video: Entry = { id: "v1", name: "phone.mp4", path: "phone.mp4", ext: "mp4", kind: "video", size: 1, modified: 0, created: null };
const widths = (c: HTMLElement) => [...c.querySelectorAll<HTMLButtonElement>(".filmstrip button")].map((b) => b.style.width);

describe("filmstrip (portrait video regression)", () => {
  it("a 352×640 video gets narrow portrait frames showing the whole frame", async () => {
    natural = { w: 352, h: 640 };
    const { container } = render(<Filmstrip entry={video} aspect={352 / 640} onSeek={() => {}} />);
    await waitFor(() => expect(container.querySelectorAll(".filmstrip img")).toHaveLength(8));
    expect(new Set(widths(container))).toEqual(new Set(["23px"]));
    expect([...container.querySelectorAll<HTMLButtonElement>(".filmstrip button")].every((b) => b.style.height === "42px")).toBe(true);
  });

  it("a rotated phone video is measured from its frames, whatever the hint says", async () => {
    // Stored landscape with 90° rotation: the frames are what the player shows (portrait).
    natural = { w: 270, h: 480 };
    const { container } = render(<Filmstrip entry={{ ...video, id: "v2" }} aspect={1920 / 1080} onSeek={() => {}} />);
    await waitFor(() => expect(container.querySelector(".filmstrip")?.getAttribute("data-aspect")).toBe((270 / 480).toFixed(3)));
    expect(widths(container)[0]).toBe("24px");
  });

  it("landscape stays landscape", async () => {
    natural = { w: 480, h: 270 };
    const { container } = render(<Filmstrip entry={{ ...video, id: "v3" }} aspect={null} onSeek={() => {}} />);
    await waitFor(() => expect(widths(container)[0]).toBe("75px"));
  });

  it("clicking a frame seeks to the moment it shows: beginning, middle, end", async () => {
    natural = { w: 352, h: 640 };
    const onSeek = vi.fn();
    const { container } = render(<Filmstrip entry={{ ...video, id: "v4" }} aspect={352 / 640} onSeek={onSeek} />);
    await waitFor(() => expect(container.querySelectorAll(".filmstrip img")).toHaveLength(8));
    const b = container.querySelectorAll<HTMLButtonElement>(".filmstrip button");
    fireEvent.click(b[0]);
    fireEvent.click(b[4]);
    fireEvent.click(b[7]);
    expect(onSeek.mock.calls.map((c) => c[0])).toEqual([1 / 16, 9 / 16, 15 / 16]);
  });
});
