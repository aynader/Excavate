# Bedrock and Everything

Excavate finds files by name on every drive in one of two ways. You choose in the first-run steps, and can change it
in Settings → General.

## Bedrock (the default)

Excavate's own index of file names, for every fixed and removable drive (NTFS, ReFS, exFAT, FAT32). It lists each
drive once, watches it for changes as they happen (no polling), and at each start catches up with what changed while
Excavate was closed from Windows' record of changes (the NTFS change journal).

- **Listing**: with your permission once (Windows' administrator prompt), Bedrock reads each drive's own table of files
  directly: about 15 seconds for millions of files, every folder included. Without it, it lists folders one by one:
  about a minute and a half, and about 500 protected system folders are left out. Only names, sizes, dates and
  attributes are read; nothing keeps running with those rights.
- **Searching**: Everything's syntax (`ext:`, `dm:`, `size:`, `path:`, `regex:`…), plus `dc:` (date created), `da:`
  (date accessed) and `attrib:` from the index itself.
- **Footprint**: the index is mapped from its file, so it costs Excavate almost no memory of its own.

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
