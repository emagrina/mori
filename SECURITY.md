# Security policy

## What Mori is

Mori is a local, offline media and file browser for inspecting folders and
external drives, including ones you don't trust. It is **not antivirus
software**: it doesn't detect or remove malware, and a clean result in Mori
means only that Mori observed no anomaly. No sandbox should be considered an
absolute security guarantee.

## Threat model

**Mori treats every file it inspects as untrusted input.** It aims to reduce the impact of hostile content: malformed or oversized images, crafted video containers, hostile PDFs, archives with path traversal or bombs, disguised extensions, deceptive names, and symlink tricks.

The isolation is meant to protect:
- **the rest of your files:** decoder workers can't read or write files, and Mori never follows links out of a folder;
- **Mori itself:** a crashing or hung decoder fails one preview, not the app;
- **your privacy:** everything runs locally, nothing is uploaded, and Mori keeps as little persistent state as it can.

How:
- **Type detection** by content, never by extension.
- **Decoding in a separate worker process.** On macOS each worker is locked down by the OS sandbox (no file access, no writes, no network, no new processes) and receives bytes, never paths.
- **Validated output.** Results are re-encoded and validated before the UI sees them.
- **Resource limits** on dimensions, pixels, memory, input size and time.
- **Archives are listed, not extracted**, with traversal, bomb and nesting checks.
- **Links are never followed.**
- **Original files are never opened in other apps automatically.** Mori refuses that for executable or mismatched files, and entirely during temporary sessions.
- **A backend mutation policy** (Read-only Mode, protected folders) gates every change to files.

**Diagnostics → Run Self-Test** verifies these protections on the running installation with synthetic fixtures.

**Outside the threat model:**
- vulnerabilities in the operating system, its media engine, WebKit or file-system drivers that a crafted file could still reach (video and audio are played by the system's engine; HEIC and PDF are decoded by system frameworks inside Mori's sandbox);
- a compromised or malicious operating system, or kernel-level attacks;
- hardware attacks, including malicious USB devices (BadUSB);
- files you choose to open in other applications;
- traces outside Mori's control: swap, snapshots, backups, crash reports, journals, SSD behaviour. Temporary sessions minimise what *Mori* keeps; Mori does not claim complete forensic trace elimination.

**Platform notes:**
- Mori 1.0.0 is released for **macOS on Apple Silicon** only.
- The worker sandbox is enforced on **macOS**.
- **Windows** uses Job object limits and no file paths, but no filesystem-denying sandbox.
- **Linux** uses resource limits and `no_new_privs`, without seccomp yet.

## Supported versions

| Version | Supported |
|---|---|
| 1.0.x | Yes |
| Earlier development builds | No |

Security fixes are released for the latest 1.x version.

## Reporting a vulnerability

Please report vulnerabilities privately through **GitHub Security Advisories** for this repository ("Report a vulnerability" on the Security tab). Do not open a public issue.

Include the Mori version, the platform, and a description or synthetic sample that reproduces the problem. **Never send real malware.**

You'll get an acknowledgement as soon as possible, and credit if you wish once a fix is released.
