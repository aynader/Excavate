# Releasing

Releases are published on this repository's Releases page. A copy of Excavate with **Update automatically** on checks
for the latest release once a day, downloads it, verifies its signature, and uses it at its next start
(`app/src/update.rs`).

## Once

1. **The signing key.** `scripts\release-key.ps1` makes an ECDSA P-256 key pair. The private key goes in
   `%USERPROFILE%\.excavate\release-signing.key`, readable by your account only; back it up offline. Whoever has it can
   publish an update every copy will install, and without it no update can reach the copies already out. It never goes
   in the repository. The public half is `PUBLIC_KEY` in `app/src/update.rs`.
2. **The repository.** `REPO` in `app/src/update.rs` names it (`"owner/name"`); with none set, nothing is checked.

## Each release

CI (`.github/workflows/ci.yml`) builds and tests every push, but it never packs or signs: a release is built and
signed on the maintainer's PC, where the key is. The key is never uploaded, never stored as a GitHub secret, and never
printed.

1. **Ready.** The commit to release is on `main` and its CI run is green.
2. **Version.** Raise `version` in the workspace's `Cargo.toml` (`0.2.0`; a pre-release such as `0.2.0-beta.1` sorts
   before `0.2.0`). The executables' version details follow it. Build once so `Cargo.lock` follows too, and commit both.
3. **Libraries.** `scripts/fetch-pdfium.sh` and `scripts/fetch-onnxruntime.sh`, if `vendor/` doesn't have them yet.
4. **Build and test** on the same PC: `cargo build --release -p excavate -p indexer`, then
   `cargo test --workspace --release`.
5. **Try it.** Run `target\release\excavate.exe` with `EXCAVATE_HOME` set to an empty folder: the first run, a search,
   reading a few files. Then close it (`excavate-indexer.exe --quit` stops the reader).
6. **Pack and sign:** `scripts\package-release.ps1`. In `dist\`: `Excavate-<version>-windows-x64.zip` (excavate.exe,
   excavate-indexer.exe, excavate-helper.exe, excavate-bedrock.exe, pdfium.dll, onnxruntime.dll, DirectML.dll, and Microsoft's C++ runtime
   with its OpenMP library from Visual Studio's redistributable folder, so a PC without it needs nothing installed),
   its signature `….zip.sig` (ECDSA P-256 over the zip's SHA-256, as hex, made with the key in
   `%USERPROFILE%\.excavate`), `Excavate-<version>-setup.exe` (the installer, with the zip packed inside) and its
   `.sig`, and `SHA256SUMS.txt` with both.
7. **Publish.** On GitHub, a release tagged `v<version>` on that commit, with what changed, and the setup, the zip, their `.sig`
   files and `SHA256SUMS.txt` attached. Mark it latest. Pre-releases aren't offered as updates.
8. **Check the update.** A copy of the previous version with **Update automatically** on (under its own
   `EXCAVATE_HOME`): Settings → About → Check now finds the new version, and the next start runs it.

## What a copy does

- Asks `https://api.github.com/repos/<REPO>/releases/latest` (once a day, or Settings → About → Check now).
- For a newer version: downloads the zip and its signature, verifies the signature against `PUBLIC_KEY` with Windows'
  own cryptography, unpacks only the files it knows into `%LOCALAPPDATA%\Excavate\updates\<version>`, and says
  so in About.
- At its next start: renames each of its files `.old`, copies the new ones in, starts the new version and ends; the new
  one removes the `.old` files. A copy that fails is undone, and a folder Excavate can't write to leaves the old version
  running. A zip that doesn't match its signature is never used.
