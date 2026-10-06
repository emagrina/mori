//! HEIC / HEIF stills on macOS, decoded by the system's ImageIO.
//!
//! Only ever called inside the sandboxed worker, from an in-memory buffer
//! whose magic bytes say HEIF. HEVC decoding is done by Apple's own sandboxed
//! decoder service, so HEIC jobs run under `worker::HEIF_PROFILE`: like the
//! default lockdown (no user files, no writes, no network, no processes) but
//! allowed to reach that one service and IOSurface. The image is decoded
//! straight to a bounded size with the EXIF orientation applied, then copied
//! into a plain RGBA buffer.

use std::ffi::c_void;

type CFTypeRef = *const c_void;
type CFIndex = isize;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    w: f64,
    h: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFAllocatorDefault: CFTypeRef;
    static kCFBooleanTrue: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    fn CFDataCreate(alloc: CFTypeRef, bytes: *const u8, len: CFIndex) -> CFTypeRef;
    fn CFNumberCreate(alloc: CFTypeRef, kind: CFIndex, value: *const c_void) -> CFTypeRef;
    fn CFNumberGetValue(n: CFTypeRef, kind: CFIndex, value: *mut c_void) -> bool;
    fn CFDictionaryCreate(
        alloc: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        n: CFIndex,
        kcb: *const c_void,
        vcb: *const c_void,
    ) -> CFTypeRef;
    fn CFDictionaryGetValue(d: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn CFRelease(cf: CFTypeRef);
}

#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    static kCGImageSourceCreateThumbnailFromImageAlways: CFTypeRef;
    static kCGImageSourceCreateThumbnailWithTransform: CFTypeRef;
    static kCGImageSourceThumbnailMaxPixelSize: CFTypeRef;
    static kCGImagePropertyPixelWidth: CFTypeRef;
    static kCGImagePropertyPixelHeight: CFTypeRef;
    static kCGImagePropertyOrientation: CFTypeRef;
    fn CGImageSourceCreateWithData(data: CFTypeRef, options: CFTypeRef) -> CFTypeRef;
    fn CGImageSourceGetCount(src: CFTypeRef) -> usize;
    fn CGImageSourceCopyPropertiesAtIndex(src: CFTypeRef, index: usize, options: CFTypeRef) -> CFTypeRef;
    fn CGImageSourceCreateThumbnailAtIndex(src: CFTypeRef, index: usize, options: CFTypeRef) -> CFTypeRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGImageGetWidth(img: CFTypeRef) -> usize;
    fn CGImageGetHeight(img: CFTypeRef) -> usize;
    fn CGColorSpaceCreateDeviceRGB() -> CFTypeRef;
    fn CGBitmapContextCreate(
        data: *mut c_void,
        w: usize,
        h: usize,
        bits: usize,
        row: usize,
        space: CFTypeRef,
        info: u32,
    ) -> CFTypeRef;
    fn CGContextDrawImage(ctx: CFTypeRef, rect: CGRect, img: CFTypeRef);
}

/// Releases a CoreFoundation object when dropped.
struct Owned(CFTypeRef);
impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

const K_CF_NUMBER_SINT64: CFIndex = 4;
/// kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big
const RGBA_INFO: u32 = 1 | (4 << 12);

/// True for ISO-BMFF files whose major brand is a HEIF still-image brand.
pub fn is_heif(input: &[u8]) -> bool {
    input.len() >= 12
        && &input[4..8] == b"ftyp"
        && matches!(&input[8..12], b"heic" | b"heix" | b"heim" | b"heis" | b"mif1" | b"msf1")
}

pub struct Decoded {
    /// Source size after orientation.
    pub src_w: u32,
    pub src_h: u32,
    pub rgba: image::RgbaImage,
}

/// Decode the primary image, oriented, with its longest edge at most `max`.
pub fn decode(input: &[u8], max: u32, max_dimension: u32, max_pixels: u64) -> Option<Decoded> {
    unsafe {
        let data = Owned(CFDataCreate(kCFAllocatorDefault, input.as_ptr(), input.len() as CFIndex));
        if data.0.is_null() {
            return None;
        }
        let src = Owned(CGImageSourceCreateWithData(data.0, std::ptr::null()));
        if src.0.is_null() || CGImageSourceGetCount(src.0) == 0 {
            return None;
        }
        // Check the declared size before decoding anything (decompression bombs).
        let props = Owned(CGImageSourceCopyPropertiesAtIndex(src.0, 0, std::ptr::null()));
        if props.0.is_null() {
            return None;
        }
        let num = |key: CFTypeRef| -> Option<i64> {
            let v = CFDictionaryGetValue(props.0, key);
            let mut out: i64 = 0;
            (!v.is_null() && CFNumberGetValue(v, K_CF_NUMBER_SINT64, &mut out as *mut i64 as *mut c_void))
                .then_some(out)
        };
        let (w, h) = (num(kCGImagePropertyPixelWidth)?, num(kCGImagePropertyPixelHeight)?);
        if w <= 0
            || h <= 0
            || w > max_dimension as i64
            || h > max_dimension as i64
            || (w as u64) * (h as u64) > max_pixels
        {
            return None;
        }
        // EXIF orientations 5–8 swap width and height.
        let swapped = matches!(num(kCGImagePropertyOrientation), Some(5..=8));
        let (src_w, src_h) = if swapped { (h as u32, w as u32) } else { (w as u32, h as u32) };

        let edge = max.min(src_w.max(src_h)) as i64;
        let edge_num =
            Owned(CFNumberCreate(kCFAllocatorDefault, K_CF_NUMBER_SINT64, &edge as *const i64 as *const c_void));
        let keys = [
            kCGImageSourceCreateThumbnailFromImageAlways,
            kCGImageSourceCreateThumbnailWithTransform,
            kCGImageSourceThumbnailMaxPixelSize,
        ];
        let values = [kCFBooleanTrue, kCFBooleanTrue, edge_num.0];
        let opts = Owned(CFDictionaryCreate(
            kCFAllocatorDefault,
            keys.as_ptr(),
            values.as_ptr(),
            keys.len() as CFIndex,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        ));
        let img = Owned(CGImageSourceCreateThumbnailAtIndex(src.0, 0, opts.0));
        if img.0.is_null() {
            return None;
        }
        let (ow, oh) = (CGImageGetWidth(img.0), CGImageGetHeight(img.0));
        if ow == 0 || oh == 0 || ow > 8192 || oh > 8192 {
            return None;
        }
        let mut buf = vec![0u8; ow * oh * 4];
        let space = Owned(CGColorSpaceCreateDeviceRGB());
        let ctx = Owned(CGBitmapContextCreate(buf.as_mut_ptr() as *mut c_void, ow, oh, 8, ow * 4, space.0, RGBA_INFO));
        if ctx.0.is_null() {
            return None;
        }
        let rect = CGRect { origin: CGPoint { x: 0.0, y: 0.0 }, size: CGSize { w: ow as f64, h: oh as f64 } };
        CGContextDrawImage(ctx.0, rect, img.0);
        drop(ctx);
        // ImageIO can fail lazily at draw time and leave the canvas empty:
        // that is a failed decode, not a white image.
        if !buf.as_chunks::<4>().0.iter().any(|p| p[3] != 0) {
            return None;
        }
        let rgba = image::RgbaImage::from_raw(ow as u32, oh as u32, buf)?;
        Some(Decoded { src_w, src_h, rgba })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn decodes_system_heic_outside_sandbox() {
        let data = std::fs::read("/System/Library/Desktop Pictures/Sonoma.heic").unwrap();
        assert!(super::is_heif(&data));
        let d = super::decode(&data, 256, 20_000, 100_000_000).expect("decoded");
        println!("{}x{} -> {}x{}", d.src_w, d.src_h, d.rgba.width(), d.rgba.height());
        let mut junk = b"\0\0\0\x18ftypheic".to_vec();
        junk.extend(std::iter::repeat_n(7u8, 4000));
        assert!(super::decode(&junk, 256, 20_000, 100_000_000).is_none());
    }
}
