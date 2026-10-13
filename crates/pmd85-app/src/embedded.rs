//! Build-time embedded resources for the portable build flavors.
//!
//! The build script generates a file table per enabled feature (see
//! `build.rs`); each module below is the generated table, compiled
//! only when its feature is on. The rest of the app reads the tables
//! through the catalog and the tape library, exactly like it reads
//! the files from disk — entries carry the embedded bytes instead
//! of a path.

/// Monitor and ROM-module images plus `rom-list.txt` from the
/// repository `Rom/` directory (`embed-roms`).
#[cfg(feature = "embed-roms")]
pub(crate) mod roms {
    include!(concat!(env!("OUT_DIR"), "/embedded_roms.rs"));
}

/// Bundled example tape images from the repository `Tapes/`
/// directory (`embed-tapes`).
#[cfg(feature = "embed-tapes")]
pub(crate) mod tapes {
    include!(concat!(env!("OUT_DIR"), "/embedded_tapes.rs"));
}
