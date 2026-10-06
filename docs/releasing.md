# Releasing Mori

Releases are built by GitHub Actions (`.github/workflows/release.yml`) from a version tag. Nothing is published automatically: the workflow creates a **draft** release for you to review.

## Steps

1. **Set the version** in `package.json`, `package-lock.json` (`npm version X.Y.Z --no-git-tag-version`), `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json`.
   - Running `cargo build` updates `Cargo.lock`.
   - `cargo test` fails if any of these disagree (`tests/version.rs`).
2. **Update `CHANGELOG.md`** and add release notes as `docs/releases/vX.Y.Z.md`.
3. **Merge to `main`** through a pull request.
4. **Tag the merge commit and push the tag:**
   ```bash
   git checkout main && git pull
   git tag -a vX.Y.Z -m "Mori X.Y.Z"
   git push origin vX.Y.Z
   ```
5. **Wait for the workflow.** It:
   1. checks out exactly that tag;
   2. refuses to continue if the tag doesn't match the app version;
   3. installs the pinned toolchains and the locked dependencies (`npm ci`, `cargo --locked`);
   4. runs the type check, `cargo fmt`, clippy and every test, including the sandboxed-worker and temporary-session persistence tests;
   5. builds `Mori.app` and `Mori_X.Y.Z_aarch64.dmg` on an Apple Silicon runner and verifies the bundle signature and version;
   6. writes `SHA256SUMS.txt`;
   7. creates a **draft** GitHub Release named "Mori X.Y.Z", with the DMG, the checksums and the release notes attached.
6. **Review the draft** on the Releases page, download and test the DMG, and click **Publish**.

To build without releasing, use **Actions → Release → Run workflow**. It runs the same checks and build, and keeps the DMG as a workflow artifact.

## Signing and notarization (optional)

Without the secrets below, the app is **ad-hoc signed** (`signingIdentity: "-"` in `tauri.conf.json`). That gives it a valid, sealed signature and the hardened runtime, but not an Apple Developer ID. Users then have to allow it once in **System Settings → Privacy & Security** (see the README).

A Developer ID signed and notarized release needs a paid Apple Developer Program membership and these **GitHub Secrets** (Settings → Secrets and variables → Actions). Never commit them to the repository.

| Secret | What it is |
|---|---|
| `APPLE_CERTIFICATE` | The *Developer ID Application* certificate and private key, exported as `.p12` and base64-encoded (`base64 -i cert.p12`) |
| `APPLE_CERTIFICATE_PASSWORD` | The password chosen when exporting the `.p12` |
| `APPLE_SIGNING_IDENTITY` | The identity name, e.g. `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | The Apple ID used for notarization |
| `APPLE_PASSWORD` | An **app-specific password** for that Apple ID (not the account password) |
| `APPLE_TEAM_ID` | The 10-character Team ID |

When `APPLE_SIGNING_IDENTITY` is set, the workflow does two things:
- **Signing:** it replaces the ad-hoc identity with that identity for the build only, and Tauri imports the certificate into a temporary keychain.
- **Notarization:** when the Apple ID secrets are also present, Tauri notarizes the app and staples the ticket.

The secrets are given only to the build step.

Mori needs **no entitlements**: it is not App Sandboxed, uses no JIT in its own process, and loads no third-party libraries. The hardened runtime is on. Don't add entitlements to make signing or notarization pass.

## Local release build

```bash
npm ci
npx tauri build --bundles app,dmg -- --locked
shasum -a 256 src-tauri/target/release/bundle/dmg/*.dmg
```

The bundles are in `src-tauri/target/release/bundle/` (`macos/Mori.app`, `dmg/Mori_X.Y.Z_aarch64.dmg`).
