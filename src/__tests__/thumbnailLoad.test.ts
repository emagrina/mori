import { describe, expect, it, vi } from "vitest";
import { loadThumbnail, ThumbnailTransient } from "../thumbnailLoad";

describe("thumbnail transient retry", () => {
  it("retries busy then succeeds without treating it as a missing image", async () => {
    const fetcher = vi.fn().mockResolvedValueOnce(new Response(null, { status: 503 })).mockResolvedValueOnce(new Response(null, { status: 200 }));
    const wait = vi.fn().mockResolvedValue(undefined);
    expect(await loadThumbnail("mori://localhost/thumb/1?g=0", new AbortController().signal, fetcher, wait)).toBe(true);
    expect(fetcher).toHaveBeenCalledTimes(2);
    expect(wait).toHaveBeenCalledWith(250);
  });
  it("caps retries and returns a transient error rather than a cacheable miss", async () => {
    const fetcher = vi.fn().mockImplementation(async () => new Response(null, { status: 503 }));
    await expect(loadThumbnail("mori://localhost/thumb/1?g=0", new AbortController().signal, fetcher, async () => {})).rejects.toBeInstanceOf(ThumbnailTransient);
    expect(fetcher).toHaveBeenCalledTimes(3);
  });
  it("does not retry permanent decode failure or cancelled work", async () => {
    const fetcher = vi.fn().mockResolvedValue(new Response(null, { status: 422 }));
    expect(await loadThumbnail("mori://localhost/thumb/1?g=0", new AbortController().signal, fetcher)).toBe(false);
    const abort = new AbortController(); abort.abort();
    await expect(loadThumbnail("mori://localhost/thumb/1?g=0", abort.signal, fetcher)).rejects.toBeInstanceOf(ThumbnailTransient);
    expect(fetcher).toHaveBeenCalledTimes(1);
  });
});
