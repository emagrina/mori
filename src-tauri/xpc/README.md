# Mori Store decoder

Store builds use the embedded `app.mori.viewer.decoder.xpc` service (macOS 12+).
Standalone builds still use the original per-job worker and Seatbelt profiles.
The Store CLI worker entry remains disabled: missing or rejected XPC never falls
back to decoding inside Mori or to the standalone process path.

## Build and archive

From the repository root, before compiling the Store release:

```sh
src-tauri/xpc/prepare-decoder.sh 8DDF5A00D0405A3BB6EB3F7DC38E5F0862AFB2FA
```

This builds the service in release for arm64 and x86_64, combines the slices,
signs it with only `com.apple.security.app-sandbox`, verifies it and records
source/binary fingerprints under `src-tauri/target/store-decoder`. Test fault
injection cannot compile in release. `build.rs` refuses a Store release without
this verified, current artifact. Any change to shared decoder sources requires
running the preparation script again.

Use `mac-app-store` and `tauri.appstore.conf.json` for the main universal build.
The Store config copies the prepared XPC to `Contents/XPCServices`. For a manually
assembled app instead, run `xpc/embed-decoder.sh /path/Mori.app [identity]` before
signing the containing app. Keep the service's own entitlements; **do not sign
nested code with the host's entitlements, or use `codesign --deep` for signing**.
The main app's provisioning profile, archive, installer and upload are separate
steps. After the final main-app signature, always run:

```sh
python3 src-tauri/xpc/verify-decoder.py --app /path/Mori.app
```

This verifies the enclosing signature, both architectures, the service signature,
hardened runtime, exact service entitlements and the macOS 12 minimum. It fails
if packaging re-signs the decoder with the host's bookmarks or file grants.

## Security boundaries

- The host verifies the embedded code signature before sending bytes. Both XPC
  endpoints install public `xpc_connection_set_peer_code_signing_requirement`
  requirements before resuming their connections: Apple anchor, team AYA5S225VU,
  exact respective identifiers and App Sandbox entitlement. Following Apple's
  TN3127, an alternative Apple Mac App Store certificate-OID branch accepts the
  Store's re-signing while retaining the exact identifier. If identity entitlements
  are added to the service, they must still match our team and service identifier.
  The service never
  authorizes a client by a reusable PID. The minimum is macOS 12; no unsupported
  fallback to `SecCodeCreateWithXPCMessage` is used on older systems.
- Requests contain only protocol version, fixed operation name, edge limit,
  timeout and data. No file path, descriptor, bookmark, sandbox extension or
  arbitrary command is accepted. Request dictionaries have exactly five keys.
- The service has its **own** container. It has no network, user-folder,
  user-selected, bookmarks, application-group or inheritance entitlement. It
  refuses startup with extra entitlements. System/framework reads and the normal
  App Sandbox container capabilities remain; this is not the standalone
  zero-filesystem Seatbelt profile.
- Input is capped at 160 MiB and output at 64 MiB plus the 16-byte header, checked
  at both ends. XPC itself necessarily receives a message before application
  validation; the signed client is trusted to construct the bounded envelope.
  One request executes at a time. Thumbnail, preview and PDF protocol requests
  use a FIFO admission queue with at most 16 waiters, before reading input bytes;
  waiters retain file handles and metadata, not large input vectors. Waiting,
  loading and XPC execution share the request deadline (capped at 30 seconds).
  Already-materialized callers never wait: contention returns a distinct Busy
  error. Queue overflow or admission expiry also returns Busy. Protocol busy,
  timeout and transport failures return HTTP 503 with Retry-After and no-store,
  without persisting a thumbnail miss. The frontend keeps four thumbnail jobs
  in flight, retries transient failures at most twice, and cancels scrolled-away
  jobs. Cache version v3 invalidates misses produced by the old fail-fast path.
  Existing eager callers still allocate input before admission; this change does
  not impose a global memory bound on every caller outside the transport.
- Service decoding is serialized. Host waits at most the requested deadline
  (maximum 30 s, less preflight time); an independent service timer exits a stuck
  decoder. Preflight signature verification is synchronous local filesystem
  work; its elapsed time counts against the request and prevents a late send,
  but an OS stall inside that API is not forcibly interruptible.
- Hard rlimits: no core dumps, file growth or new user processes; 128 descriptors;
  60 seconds of cumulative service CPU. Successful smoke tests include actual
  user-file read/write denial, inbound/outbound network denial and spawn denial.
  App Sandbox itself is not presented as a universal process-creation prohibition.
- Rust codecs retain dimension/pixel/allocation limits. A separate 25 ms watchdog
  reads `TASK_VM_INFO.phys_footprint` and exits the service above 1 GiB, covering
  ImageIO/CoreGraphics as well as Rust allocations. It is **not an instantaneous
  kernel hard memory cap**: transient overshoot and system decoder allocations in
  Apple's own process are outside that limit. Native code compromise can interfere
  with in-process watchdogs; the OS sandbox and hard rlimits are the independent
  containment boundary.
- Display responses are restricted to a single, static 8-bit RGBA PNG, including
  GIF previews (first frame). The host independently validates PNG chunk CRCs,
  IHDR dimensions against the requested edge, allowed chunks, no APNG, and no
  trailing bytes. It does not invoke an image codec to validate the response.
  Other operations have fixed format/length/JSON-shape checks. No response is
  interpreted as a path or shell command.
- Before a sanitized copy can be written, the host independently walks the
  original and returned JPEG/PNG/WebP containers. A strict segment/chunk allowlist
  and digest require preserved compressed image/color/animation content to match
  the original; only a canonical, unchanged scalar orientation may be retained
  from EXIF. This check does not ask the producer to attest its own result. The
  prior dimension/metadata checks also remain as additional rejection checks.
  Color-profile/image payloads are retained, not rewritten or semantically scrubbed.

## Reproducible tests

```sh
cargo test --manifest-path src-tauri/Cargo.toml --features mac-app-store --bin mori worker::
cargo test --manifest-path src-tauri/decoder/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --test worker
python3 src-tauri/xpc/smoke-test.py --identity 2F5DF05794973AEA9285B66E37AA6E2431602E54 --testing --rosetta --output /tmp/mori-xpc-security-NEW
python3 src-tauri/xpc/smoke-test.py --identity 8DDF5A00D0405A3BB6EB3F7DC38E5F0862AFB2FA --rosetta --output /tmp/mori-xpc-production-NEW
```

Use a fresh output directory each time. The script builds a universal test app
with the **real production bridge and Rust decoder**, signs both nested service
and host, checks both signatures, and executes all operations. It generates its
PNG/JPEG/GIF/PDF fixtures and uses the repository's HEIC/MP4 fixtures. It tests
malformed requests, oversized dimensions, wrong app identity signed by the same
team and no fallback when the service is missing. Debug-only fault injection
also checks timeout, memory abort and successful restart in the same client
process. Production rejects all fault-injection operation names. `--rosetta`
additionally executes the universal host as x86_64; it is not a physical Intel
machine test. Results are written to `report.json`.

The sandbox denial test creates and removes a synthetic sentinel in the host's
Caches directory. It never reads personal fixture data. Test containers may be
created by macOS. `launchd` can delay a service restart by roughly ten seconds;
previews can fail during that interval, without weakening isolation.

These tests exercise signed XPC bundles and the real transport/decoder, not the
final Tauri UI or App Store processing. The final signed Mori app should also be
opened and exercised by the archive/release owner before submission.

Signing references: [Apple TN3127](https://developer.apple.com/documentation/technotes/tn3127-inside-code-signing-requirements)
and [Apple TN3161](https://developer.apple.com/documentation/technotes/tn3161-inside-code-signing-certificates).
The Apple re-signed branch requires a TestFlight/App Store install to exercise;
local Distribution signing cannot simulate Apple's private signing identity.
