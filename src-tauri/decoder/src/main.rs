//! Dedicated service executable: no Tauri, UI, bookmarks, file broker or shell.
#![allow(dead_code)]
#[cfg(all(feature = "xpc-testing", not(debug_assertions)))]
compile_error!("XPC test operations must never be included in release builds");
#[macro_use]
#[path = "../../src/i18n.rs"]
mod i18n;
#[path = "../../src/heif.rs"]
mod heif;
#[path = "../../src/metadata.rs"]
mod metadata;
#[path = "../../src/pdf.rs"]
mod pdf;
#[path = "../../src/probe.rs"]
mod probe;
#[path = "../../src/sanitize.rs"]
mod sanitize;
#[path = "../../src/worker.rs"]
mod worker;
use std::ffi::{c_char, CStr};

type Decode = unsafe extern "C" fn(*const c_char, u32, *const u8, usize, *mut *mut u8, *mut usize) -> i32;
extern "C" {
    fn mori_service_main(decode: Decode);
}
unsafe extern "C" fn decode(
    op: *const c_char,
    edge: u32,
    input: *const u8,
    len: usize,
    out: *mut *mut u8,
    out_len: *mut usize,
) -> i32 {
    *out = std::ptr::null_mut();
    *out_len = 0;
    if op.is_null() || (input.is_null() && len != 0) || len as u64 > worker::MAX_INPUT {
        return 3;
    }
    let result = std::panic::catch_unwind(|| {
        let op = CStr::from_ptr(op).to_str().map_err(|_| 6)?;
        let bytes = if len == 0 { &[] } else { std::slice::from_raw_parts(input, len) };
        worker::decode_wire(op, edge, bytes)
    });
    match result {
        Ok(Ok(bytes)) => {
            let p = libc::malloc(bytes.len()).cast::<u8>();
            if p.is_null() {
                return 3;
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
            *out = p;
            *out_len = bytes.len();
            0
        }
        Ok(Err(code)) => code,
        Err(_) => 4,
    }
}
fn main() {
    unsafe { mori_service_main(decode) };
}
