//! Excavate's indexer: reads what's inside the person's files, in the background, and keeps the content index the
//! popup searches. Started and stopped by Excavate (and ended with it, by a job object). Its own process, so OCR's
//! models never weigh on the popup, and a file that trips a reader can't take search down.
//!
//! A session: list the chosen folders (Everything, or a walk), compare with the catalog, group the images (the first sweep),
//! queue what's new or changed (newest first), read it in two lanes (documents; pictures with OCR), commit as it goes, and
//! then follow changes as Windows reports them. Settings changing starts a new session.
//!
//! Two executables of this one program (`run.rs`), so Task Manager names each by what it does: `excavate-indexer.exe`
//! ("Excavate Reader", reads what's inside files) and `excavate-helper.exe` ("Excavate Helper", the search helper,
//! `--seek`, started by the popup).

#![windows_subsystem = "windows"]

include!("run.rs");
