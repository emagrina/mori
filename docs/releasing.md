# Releasing Mori

Releases are built by GitHub Actions (`.github/workflows/release.yml`) on native runners when a version tag is pushed. Ordinary pushes and pull requests never run it.

## Steps

1. **Set the version** in `package.json`, `package-lock.json` (`npm version X.Y.Z --no-git-tag-version`), `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json`.
   - Running `cargo build` updates `Cargo.lock`.
   - `cargo test` fails if any of these disagree (`tests/version.rs`).
2. **Update `CHANGELOG.md`** and add release notes as `docs/releases/vX.Y.Z.md`. The workflow refuses to release without that file.
3. **Merge to `main`** through a pull request.
4. **Tag the merge commit on `main` and push the tag:**
   ```bash
   git checkout main && git pull --ff-only
   git log -1 --oneline            # the commit you intend to release
   git tag -a vX.Y.Z -m "Mori X.Y.Z"
   git push origin vX.Y.Z
   ```
5. **The workflow runs.**
   1. **Four build jobs run in parallel**, one per native runner:

      | Job | Runner | Produces |
      |---|---|---|
      | macOS_arm64 | `macos-15` | `Mori_X.Y.Z_macOS_arm64.dmg` |
      | macOS_x64 | `macos-15-intel` | `Mori_X.Y.Z_macOS_x64.dmg` |
      | Windows_x64 | `windows-2025` | `Mori_X.Y.Z_Windows_x64-setup.exe`, `Mori_X.Y.Z_Windows_x64.msi` |
      | Linux_x64 | `ubuntu-22.04` | `Mori_X.Y.Z_Linux_x64.AppImage`, `Mori_X.Y.Z_Linux_x64.deb` |

      Each job:
      - refuses a tag that doesn't match the app version or doesn't point at a commit on `main`;
      - installs the pinned Rust toolchain and the locked dependencies (`npm ci`, `cargo --locked`);
      - runs the type check, `cargo fmt`, clippy and the tests;
      - builds;
      - checks the output: macOS signature, architecture and version; DMG integrity; `.deb` metadata.

      The files are Tauri's own bundles with clearer names; their contents aren't changed.
   2. **Only if all four succeed,** a final job:
      1. writes `SHA256SUMS.txt` over the final files;
      2. creates the release **"Mori vX.Y.Z"** as a draft, with `docs/releases/vX.Y.Z.md` as the notes and every file attached;
      3. downloads the attached files again and checks them against `SHA256SUMS.txt`;
      4. **publishes** the release and marks it as latest.

      If any step fails, nothing is published (at most an unpublished draft is left to delete).

To build without releasing, use **Actions → Release → Run workflow**. It runs the same checks and builds, keeps the files as workflow artifacts, and never creates a release.

**If a release run fails**, delete the tag before retrying, both on GitHub and locally (`git push --delete origin vX.Y.Z && git tag -d vX.Y.Z`), along with any draft it left. Then fix `main` and tag again.

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

The secrets are given only to the macOS build step. They are never available to pull requests, because this workflow doesn't run for them.

**Windows** builds are unsigned. Signing them needs a code-signing certificate (OV/EV, or a cloud signing service) and Tauri's `bundle.windows` signing settings; until then SmartScreen shows *Unknown publisher*. **Linux** AppImage and `.deb` files are unsigned; users verify them with `SHA256SUMS.txt`.

Mori needs **no entitlements**: it is not App Sandboxed, uses no JIT in its own process, and loads no third-party libraries. The hardened runtime is on. Don't add entitlements to make signing or notarization pass.

## Local release build

```bash
npm ci
npx tauri build --bundles app,dmg -- --locked
shasum -a 256 src-tauri/target/release/bundle/dmg/*.dmg
```

The bundles are in `src-tauri/target/release/bundle/` (`macos/Mori.app`, `dmg/Mori_X.Y.Z_aarch64.dmg`); the workflow gives them the release names above.
