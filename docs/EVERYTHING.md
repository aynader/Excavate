# Bedrock and Everything

Excavate finds files by name on every drive in one of two ways. You choose in the first-run steps, which show how the
two compare, and can change it in Settings → General.

## Bedrock (the default)

Excavate's own index of file names, for every fixed and removable drive (NTFS, ReFS, exFAT, FAT32). It lists each
drive once, watches it for changes as they happen (no polling), and at each start catches up with what changed while
Excavate was closed from Windows' record of changes (the NTFS change journal).

- **Listing**: with your permission once (Windows' administrator prompt), Excavate sets up Excavate Bedrock, a small
  Windows service that reads each drive's own table of files: about 10 seconds for millions of files, every folder
  included. It starts when a drive needs listing or catching up, and stops after a minute with nothing to do. It
  passes on only names, sizes, dates and attributes, never what's inside a file, and other accounts' files on the PC
  stay out. Settings → General removes it again. Without it, Bedrock lists folders one by one, which takes a few
  minutes for millions of files, and the few hundred folders Windows protects are left out.
- **Searching**: Everything's syntax (`ext:`, `dm:`, `size:`, `path:`, `regex:`…), plus `dc:` (date created), `da:`
  (date accessed) and `attrib:` from the index itself.
- **Footprint**: the index is mapped from its file, so it costs Excavate almost no memory of its own.
- **On its own**: Bedrock is also a Rust library, [bedrock-index](https://crates.io/crates/bedrock-index)
  ([repository](https://github.com/aynader/Bedrock)).

## Everything

[Everything](https://www.voidtools.com) is a free file-name search engine by voidtools. If you choose it, Excavate asks
the Everything running on your PC for names, on your PC, and Bedrock stays off.

**Install Everything** (offered when it isn't installed):

1. Downloads the official installer (64-bit, about 2 MB) from voidtools.com over HTTPS.
2. Checks it against the SHA-256 checksum voidtools publishes. If it doesn't match, it isn't run.
3. Runs it silently. Windows asks for your approval once, because Everything installs for the whole PC.
4. Starts Everything and waits until its list of files is ready.

**Custom installation** opens the same installer with its usual screens. You can uninstall Everything at any time from
Windows' Settings.
