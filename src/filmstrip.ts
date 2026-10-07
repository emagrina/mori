/**
 * Filmstrip geometry. Every strip has one fixed height; each frame is as
 * wide as the video's *displayed* aspect ratio needs (rotation metadata
 * already applied: the frames are taken from what the player shows), so a
 * 352×640 phone video gets narrow portrait frames, never landscape crops.
 */

/** Displayed width ÷ height outside this range is clamped (the frame is then letterboxed, never cropped). */
export const MIN_ASPECT = 0.4;
export const MAX_ASPECT = 3;
const FALLBACK = 16 / 9;

/** Width of one frame for a strip `height` px tall. */
export function frameWidth(aspect: number | null | undefined, height: number): number {
  const a = aspect && Number.isFinite(aspect) && aspect > 0 ? aspect : FALLBACK;
  return Math.round(height * Math.min(MAX_ASPECT, Math.max(MIN_ASPECT, a)));
}

/** Aspect ratio from dimensions (0 or missing: unknown). */
export const aspectOf = (w: number | null | undefined, h: number | null | undefined): number | null => (w && h ? w / h : null);

/**
 * The moment frame `k` of `count` shows, as a fraction of the duration. The
 * same function places the samples and seeks when a frame is clicked, so a
 * frame always jumps to exactly what it shows, whatever its width.
 */
export const frameFraction = (k: number, count: number): number => (k + 0.5) / count;
