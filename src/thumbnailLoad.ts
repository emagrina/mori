/** A temporary decoder/admission failure must not become a permanent thumbnail miss. */
export class ThumbnailTransient extends Error {}

export async function loadThumbnail(
  url: string,
  signal: AbortSignal,
  fetcher: typeof fetch = fetch,
  wait: (ms: number) => Promise<void> = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
): Promise<boolean> {
  // Three attempts total. Each has a timeout; retries share the caller's cancellation.
  for (let attempt = 0; attempt < 3; attempt++) {
    if (signal.aborted) throw new ThumbnailTransient();
    const request = new AbortController();
    const cancel = () => request.abort();
    signal.addEventListener("abort", cancel, { once: true });
    const timer = setTimeout(cancel, 15_000);
    try {
      const response = await fetcher(`${url}&attempt=${attempt}`, { signal: request.signal });
      // The image element subsequently uses the backend's cached, validated output.
      await response.body?.cancel();
      if (response.ok) return true;
      if (![500, 503, 504].includes(response.status)) return false;
    } catch {
      if (signal.aborted) throw new ThumbnailTransient();
    } finally {
      clearTimeout(timer);
      signal.removeEventListener("abort", cancel);
    }
    if (attempt < 2) await wait(250 * 2 ** attempt);
  }
  throw new ThumbnailTransient();
}
