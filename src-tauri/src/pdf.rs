//! PDF pages rendered to plain bitmaps by macOS CoreGraphics, inside the
//! sandboxed worker. CoreGraphics' PDF renderer draws page content only: it
//! has no JavaScript engine, does not run actions, follow links, open
//! attachments or load anything from the network. The UI only ever receives
//! the re-encoded bitmap.

use std::ffi::{c_char, c_void, CStr};

type Ref = *const c_void;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CGRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGAffineTransform {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    tx: f64,
    ty: f64,
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFAllocatorDefault: Ref;
    fn CFDataCreate(alloc: Ref, bytes: *const u8, len: isize) -> Ref;
    fn CFRelease(cf: Ref);
    fn CFStringGetCString(s: Ref, buf: *mut c_char, size: isize, encoding: u32) -> bool;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGDataProviderCreateWithCFData(data: Ref) -> Ref;
    fn CGDataProviderRelease(p: Ref);
    fn CGPDFDocumentCreateWithProvider(p: Ref) -> Ref;
    fn CGPDFDocumentRelease(d: Ref);
    fn CGPDFDocumentGetNumberOfPages(d: Ref) -> usize;
    fn CGPDFDocumentIsEncrypted(d: Ref) -> bool;
    fn CGPDFDocumentIsUnlocked(d: Ref) -> bool;
    fn CGPDFDocumentGetPage(d: Ref, n: usize) -> Ref;
    fn CGPDFDocumentGetInfo(d: Ref) -> Ref;
    fn CGPDFDocumentGetCatalog(d: Ref) -> Ref;
    fn CGPDFPageGetBoxRect(p: Ref, box_: i32) -> CGRect;
    fn CGPDFPageGetRotationAngle(p: Ref) -> i32;
    fn CGPDFPageGetDrawingTransform(p: Ref, box_: i32, rect: CGRect, rotate: i32, preserve: bool) -> CGAffineTransform;
    fn CGPDFDictionaryGetString(d: Ref, key: *const c_char, out: *mut Ref) -> bool;
    fn CGPDFDictionaryGetDictionary(d: Ref, key: *const c_char, out: *mut Ref) -> bool;
    fn CGPDFDictionaryGetObject(d: Ref, key: *const c_char, out: *mut Ref) -> bool;
    fn CGPDFStringCopyTextString(s: Ref) -> Ref;
    fn CGColorSpaceCreateDeviceRGB() -> Ref;
    fn CGBitmapContextCreate(
        data: *mut c_void,
        w: usize,
        h: usize,
        bits: usize,
        row: usize,
        space: Ref,
        info: u32,
    ) -> Ref;
    fn CGContextSetRGBFillColor(c: Ref, r: f64, g: f64, b: f64, a: f64);
    fn CGContextFillRect(c: Ref, r: CGRect);
    fn CGContextConcatCTM(c: Ref, t: CGAffineTransform);
    fn CGContextScaleCTM(c: Ref, sx: f64, sy: f64);
    fn CGContextDrawPDFPage(c: Ref, p: Ref);
}

const MEDIA_BOX: i32 = 0;
const CROP_BOX: i32 = 1;
/// kCGImageAlphaNoneSkipLast | kCGBitmapByteOrder32Big
const RGBX: u32 = 5 | (4 << 12);
const MAX_PAGES: usize = 5000;

struct Doc {
    provider: Ref,
    doc: Ref,
}

impl Drop for Doc {
    fn drop(&mut self) {
        unsafe {
            if !self.doc.is_null() {
                CGPDFDocumentRelease(self.doc);
            }
            if !self.provider.is_null() {
                CGDataProviderRelease(self.provider);
            }
        }
    }
}

fn open(bytes: &[u8]) -> Option<Doc> {
    unsafe {
        let data = CFDataCreate(kCFAllocatorDefault, bytes.as_ptr(), bytes.len() as isize);
        if data.is_null() {
            return None;
        }
        let provider = CGDataProviderCreateWithCFData(data);
        CFRelease(data);
        if provider.is_null() {
            return None;
        }
        let doc = CGPDFDocumentCreateWithProvider(provider);
        let d = Doc { provider, doc };
        (!doc.is_null()).then_some(d)
    }
}

fn info_string(dict: Ref, key: &CStr) -> Option<String> {
    unsafe {
        let mut s: Ref = std::ptr::null();
        if dict.is_null() || !CGPDFDictionaryGetString(dict, key.as_ptr(), &mut s) || s.is_null() {
            return None;
        }
        let cf = CGPDFStringCopyTextString(s);
        if cf.is_null() {
            return None;
        }
        let mut buf = vec![0 as c_char; 1024];
        let ok = CFStringGetCString(cf, buf.as_mut_ptr(), buf.len() as isize, 0x0800_0100);
        CFRelease(cf);
        ok.then(|| CStr::from_ptr(buf.as_ptr()).to_string_lossy().chars().take(300).collect())
    }
}

fn has(dict: Ref, key: &CStr) -> bool {
    let mut o: Ref = std::ptr::null();
    !dict.is_null() && unsafe { CGPDFDictionaryGetObject(dict, key.as_ptr(), &mut o) }
}

fn sub(dict: Ref, key: &CStr) -> Ref {
    let mut o: Ref = std::ptr::null();
    if dict.is_null() || !unsafe { CGPDFDictionaryGetDictionary(dict, key.as_ptr(), &mut o) } {
        return std::ptr::null();
    }
    o
}

/// `key=value` lines: pages, encryption, info fields and the presence of
/// active content (reported, never run).
pub fn info(bytes: &[u8]) -> Option<String> {
    let d = open(bytes)?;
    unsafe {
        let pages = CGPDFDocumentGetNumberOfPages(d.doc);
        let mut out = format!(
            "pages={pages}\nencrypted={}\nlocked={}\n",
            CGPDFDocumentIsEncrypted(d.doc),
            CGPDFDocumentIsEncrypted(d.doc) && !CGPDFDocumentIsUnlocked(d.doc)
        );
        let info = CGPDFDocumentGetInfo(d.doc);
        for (k, name) in [
            (c"Title", "title"),
            (c"Author", "author"),
            (c"Creator", "creator"),
            (c"Producer", "producer"),
            (c"Subject", "subject"),
        ] {
            if let Some(v) = info_string(info, k) {
                let v: String = v.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
                out.push_str(&format!("{name}={v}\n"));
            }
        }
        let catalog = CGPDFDocumentGetCatalog(d.doc);
        let names = sub(catalog, c"Names");
        out.push_str(&format!(
            "javascript={}\nopenaction={}\nembedded={}\nforms={}\n",
            has(names, c"JavaScript"),
            has(catalog, c"OpenAction") || has(catalog, c"AA"),
            has(names, c"EmbeddedFiles"),
            has(catalog, c"AcroForm")
        ));
        Some(out)
    }
}

/// Render page `n` (1-based) to RGBA with its longest edge at most `max`.
pub fn render(bytes: &[u8], n: usize, max: u32) -> Option<image::RgbaImage> {
    let d = open(bytes)?;
    unsafe {
        let pages = CGPDFDocumentGetNumberOfPages(d.doc);
        if n == 0 || n > pages.min(MAX_PAGES) || (CGPDFDocumentIsEncrypted(d.doc) && !CGPDFDocumentIsUnlocked(d.doc)) {
            return None;
        }
        let page = CGPDFDocumentGetPage(d.doc, n);
        if page.is_null() {
            return None;
        }
        let mut box_ = CGPDFPageGetBoxRect(page, CROP_BOX);
        if box_.w <= 0.0 || box_.h <= 0.0 {
            box_ = CGPDFPageGetBoxRect(page, MEDIA_BOX);
        }
        if !(box_.w > 0.0 && box_.h > 0.0 && box_.w < 1e6 && box_.h < 1e6) {
            return None;
        }
        let rot = CGPDFPageGetRotationAngle(page).rem_euclid(360);
        let (pw, ph) = if rot == 90 || rot == 270 { (box_.h, box_.w) } else { (box_.w, box_.h) };
        let scale = max as f64 / pw.max(ph);
        let (w, h) = (((pw * scale).round() as usize).clamp(1, 8192), ((ph * scale).round() as usize).clamp(1, 8192));
        let mut buf = vec![0u8; w * h * 4];
        let space = CGColorSpaceCreateDeviceRGB();
        let ctx = CGBitmapContextCreate(buf.as_mut_ptr() as *mut c_void, w, h, 8, w * 4, space, RGBX);
        CFRelease(space);
        if ctx.is_null() {
            return None;
        }
        let full = CGRect { x: 0.0, y: 0.0, w: w as f64, h: h as f64 };
        CGContextSetRGBFillColor(ctx, 1.0, 1.0, 1.0, 1.0);
        CGContextFillRect(ctx, full);
        // The drawing transform never scales up, so fit the page at its own
        // size (handling rotation and the box origin) and scale from there.
        let native = CGRect { x: 0.0, y: 0.0, w: pw, h: ph };
        let t = CGPDFPageGetDrawingTransform(page, CROP_BOX, native, 0, true);
        CGContextScaleCTM(ctx, w as f64 / pw, h as f64 / ph);
        CGContextConcatCTM(ctx, t);
        CGContextDrawPDFPage(ctx, page);
        CFRelease(ctx);
        for px in buf.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
        image::RgbaImage::from_raw(w as u32, h as u32, buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal one-page PDF with a correct xref table; `extra` goes into the catalog.
    pub fn make_pdf(extra: &str) -> Vec<u8> {
        let objs = [
            format!("<< /Type /Catalog /Pages 2 0 R {extra} >>"),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R >>".to_string(),
            "<< /Length 35 >>\nstream\n0 0 1 rg 10 10 80 40 re f\nendstream".to_string(),
            "<< /Title (Synthetic) /Author (Mori tests) >>".to_string(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
        for off in offsets {
            out.extend(format!("{off:010} 00000 n \n").bytes());
        }
        out.extend(
            format!("trailer\n<< /Size {} /Root 1 0 R /Info 5 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1)
                .bytes(),
        );
        out
    }

    #[test]
    fn reads_and_renders_a_plain_pdf() {
        let pdf = make_pdf("");
        let info = info(&pdf).expect("info");
        assert!(info.contains("pages=1"));
        assert!(info.contains("title=Synthetic"));
        assert!(info.contains("javascript=false") && info.contains("openaction=false"));
        let img = render(&pdf, 1, 400).expect("render");
        assert_eq!((img.width(), img.height()), (400, 200));
        // The blue rectangle (10..90 × 10..50 of 200 × 100, PDF origin bottom
        // left) fills the page at the requested size, not a corner of it.
        let blue = |x: u32, y: u32| {
            let p = img.get_pixel(x, y);
            p[2] > 200 && p[0] < 50
        };
        assert!(blue(25, 175) && blue(175, 105), "rectangle scaled to the bitmap");
        assert!(!blue(195, 140) && !blue(100, 90));
        assert!(render(&pdf, 2, 400).is_none(), "out-of-range page");
        assert!(render(&pdf, 0, 400).is_none());
    }

    #[test]
    fn active_content_is_reported_not_run() {
        let pdf = make_pdf("/OpenAction << /S /JavaScript /JS (app.alert(1)) >> /Names << /JavaScript << /Names [] >> /EmbeddedFiles << /Names [] >> >> /AcroForm << /Fields [] >>");
        let info = info(&pdf).expect("info");
        assert!(info.contains("javascript=true"), "{info}");
        assert!(info.contains("openaction=true"));
        assert!(info.contains("embedded=true"));
        assert!(info.contains("forms=true"));
        // Rendering draws the page content only.
        assert!(render(&pdf, 1, 200).is_some());
    }

    #[test]
    fn malformed_pdfs_fail_cleanly() {
        assert!(info(b"").is_none());
        assert!(info(b"%PDF-1.7\n garbage garbage").is_none_or(|i| i.starts_with("pages=0")));
        let pdf = make_pdf("");
        let truncated = &pdf[..pdf.len() / 2];
        // CoreGraphics may repair or refuse; either way no crash and no bogus page.
        if let Some(i) = info(truncated) {
            assert!(i.starts_with("pages="));
        }
        let _ = render(truncated, 1, 200);
        let mut huge = make_pdf("");
        let s = String::from_utf8_lossy(&huge).replace("[0 0 200 100]", "[0 0 9e9 9e9]");
        huge = s.into_bytes();
        assert!(render(&huge, 1, 200).is_none(), "absurd page boxes are refused");
    }
}
