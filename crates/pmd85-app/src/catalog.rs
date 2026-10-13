//! Catalog of available monitor ROMs and ROM modules.
//!
//! Scans the `Rom/` directory and joins the file list with the
//! human-written descriptions from `rom-list.txt` (the archive's own
//! inventory, kept in Slovak). Used by the settings window to offer
//! model/module pickers with real descriptions.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// One ROM file on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RomEntry {
    /// File name inside the ROM directory (`basic3.rmm`).
    pub file_name: String,
    /// Full path.
    pub path: PathBuf,
    /// File size in bytes.
    pub size: u64,
    /// Description from `rom-list.txt`, if the file is listed there
    /// (joined bullet lines, original wording).
    pub description: String,
    /// Embedded contents (portable builds): `Some` when the entry
    /// comes from the binary instead of `path`.
    pub embedded: Option<&'static [u8]>,
}

impl RomEntry {
    /// Human-readable size ("10 kB").
    pub fn size_label(&self) -> String {
        size_label(self.size)
    }

    /// The entry's contents: the embedded bytes, or the file on disk.
    pub fn read(&self) -> std::io::Result<Vec<u8>> {
        match self.embedded {
            Some(bytes) => Ok(bytes.to_vec()),
            None => fs::read(&self.path),
        }
    }
}

/// Human-readable file size ("10 kB").
fn size_label(size: u64) -> String {
    let kb = size as f64 / 1024.0;
    if kb >= 1.0 {
        format!("{kb:.0} kB")
    } else {
        format!("{size} B")
    }
}

/// The parsed contents of `rom-list.txt`: file name -> description.
///
/// Entries start with a `+ file.rom` line and continue with indented
/// `  - text` bullet lines until the next entry or separator.
pub fn parse_rom_list(text: &str) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = HashMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(name) = line.strip_prefix("+ ") {
            let name = name.trim();
            current = Some(name.to_string());
            map.entry(name.to_string()).or_default();
        } else if trimmed.starts_with("- ") && current.is_some() {
            let bullet = trimmed[2..].trim();
            let entry = map.entry(current.clone().unwrap()).or_default();
            if !entry.is_empty() {
                entry.push(' ');
            }
            entry.push_str(bullet);
        } else if line.starts_with('*') || line.starts_with("---") {
            // Section headers and separators end an entry.
            current = None;
        }
    }
    map
}

/// A scanned ROM directory.
#[derive(Clone, Debug)]
pub struct RomCatalog {
    monitors: Vec<RomEntry>,
    modules: Vec<RomEntry>,
}

impl RomCatalog {
    /// Scan `rom_dir` for `.rom` and `.rmm` files. Missing directory or
    /// unreadable files are not fatal: the catalog just comes back
    /// (possibly empty), the caller decides how loud to complain.
    pub fn scan(rom_dir: impl Into<PathBuf>) -> Self {
        let rom_dir = rom_dir.into();
        let mut descriptions = HashMap::new();
        if let Ok(text) = fs::read_to_string(rom_dir.join("rom-list.txt")) {
            descriptions = parse_rom_list(&text);
        }
        let mut monitors = Vec::new();
        let mut modules = Vec::new();
        let entries = fs::read_dir(&rom_dir).into_iter().flatten().flatten();
        for entry in entries {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let path = entry.path();
            let Some(ext) = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
            else {
                continue;
            };
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let file_name = file_name.to_string();
            let description = descriptions.get(&file_name).cloned().unwrap_or_default();
            let rom = RomEntry {
                file_name,
                path,
                size: meta.len(),
                description,
                embedded: None,
            };
            match ext.as_str() {
                "rom" => monitors.push(rom),
                "rmm" => modules.push(rom),
                _ => {}
            }
        }
        monitors.sort_by(|a, b| a.file_name.cmp(&b.file_name));
        modules.sort_by(|a, b| a.file_name.cmp(&b.file_name));
        RomCatalog { monitors, modules }
    }

    pub fn monitors(&self) -> &[RomEntry] {
        &self.monitors
    }

    pub fn modules(&self) -> &[RomEntry] {
        &self.modules
    }

    /// Find a monitor/module entry by file name.
    pub fn find(&self, file_name: &str) -> Option<&RomEntry> {
        self.monitors
            .iter()
            .chain(self.modules.iter())
            .find(|e| e.file_name == file_name)
    }

    /// Build the catalog from the embedded file table (`embed-roms`
    /// builds): the same classification and `rom-list.txt`
    /// descriptions as a directory scan, with the bytes served from
    /// the binary instead of disk.
    #[cfg(any(feature = "embed-roms", test))]
    pub fn from_embedded(files: &'static [(&'static str, &'static [u8])]) -> Self {
        let mut descriptions = HashMap::new();
        if let Some((_, bytes)) = files.iter().find(|(name, _)| *name == "rom-list.txt") {
            if let Ok(text) = std::str::from_utf8(bytes) {
                descriptions = parse_rom_list(text);
            }
        }
        let mut monitors = Vec::new();
        let mut modules = Vec::new();
        for (file_name, bytes) in files {
            let ext = file_name
                .rsplit_once('.')
                .map(|(_, e)| e.to_ascii_lowercase())
                .unwrap_or_default();
            let entry = RomEntry {
                file_name: file_name.to_string(),
                path: PathBuf::new(),
                size: bytes.len() as u64,
                description: descriptions.get(*file_name).cloned().unwrap_or_default(),
                embedded: Some(bytes),
            };
            match ext.as_str() {
                "rom" => monitors.push(entry),
                "rmm" => modules.push(entry),
                _ => {}
            }
        }
        monitors.sort_by(|a, b| a.file_name.cmp(&b.file_name));
        modules.sort_by(|a, b| a.file_name.cmp(&b.file_name));
        RomCatalog { monitors, modules }
    }
}

/// One bundled tape image: a `.ptp`/`.pmd` file on disk (a
/// `--tapes-dir` directory) or embedded in the binary (the
/// `embed-tapes` build flavor).
#[derive(Clone, Debug)]
pub struct TapeEntry {
    /// File name (`games-2.ptp`).
    pub file_name: String,
    /// File size in bytes.
    pub size: u64,
    source: TapeSource,
}

#[derive(Clone, Debug)]
enum TapeSource {
    File(PathBuf),
    /// Only constructed by [`TapeLibrary::from_embedded`] (compiled
    /// under `embed-tapes` and for tests); unmatched-but-unbuilt in
    /// plain builds.
    #[allow(dead_code)]
    Embedded(&'static [u8]),
}

impl TapeEntry {
    /// Human-readable size ("60 kB").
    pub fn size_label(&self) -> String {
        size_label(self.size)
    }

    /// The tape image contents: the embedded bytes, or the file.
    pub fn read(&self) -> std::io::Result<Vec<u8>> {
        match &self.source {
            TapeSource::Embedded(bytes) => Ok(bytes.to_vec()),
            TapeSource::File(path) => fs::read(path),
        }
    }
}

/// The bundled example tapes shown in the Tape window: scanned from
/// a directory (`--tapes-dir`) or embedded in the binary
/// (`embed-tapes`). `None` in the App means no library at all —
/// plain builds without `--tapes-dir` show no section.
#[derive(Clone, Debug, Default)]
pub struct TapeLibrary {
    entries: Vec<TapeEntry>,
}

impl TapeLibrary {
    /// Scan `dir` for `.ptp`/`.pmd` tape images (missing directory:
    /// an empty library, like a missing ROM directory).
    pub fn scan(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let mut entries = Vec::new();
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(ext) = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
            else {
                continue;
            };
            if !matches!(ext.as_str(), "ptp" | "pmd") {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            entries.push(TapeEntry {
                file_name: file_name.to_string(),
                size: meta.len(),
                source: TapeSource::File(path),
            });
        }
        entries.sort_by(|a, b| a.file_name.cmp(&b.file_name));
        TapeLibrary { entries }
    }

    /// Build the library from the embedded file table (`embed-tapes`
    /// builds): every table entry is a tape image.
    #[cfg(any(feature = "embed-tapes", test))]
    pub fn from_embedded(files: &'static [(&'static str, &'static [u8])]) -> Self {
        let mut entries = Vec::new();
        for (file_name, bytes) in files {
            entries.push(TapeEntry {
                file_name: file_name.to_string(),
                size: bytes.len() as u64,
                source: TapeSource::Embedded(bytes),
            });
        }
        // The build script emits the table sorted already; sort here
        // anyway so hand-made tables behave the same.
        entries.sort_by(|a, b| a.file_name.cmp(&b.file_name));
        TapeLibrary { entries }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&TapeEntry> {
        self.entries.get(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pmd85_core::Model;

    const SAMPLE_LIST: &str = "\
***************************************
* header
***************************************
+ monit3.rom
  - zakladny monitor PMD 85-3
  - velkost 8kB
  - umiestnenie v pamati od adresy 0E000h
-------------------------------------------------------------------------------
+ basic3.rmm
  - BASIC G V3.0 pre PMD 85-3
+ notarom.txt
  - this is not a rom
";

    #[test]
    fn parses_entries_and_joins_bullets() {
        let map = parse_rom_list(SAMPLE_LIST);
        assert_eq!(
            map.get("monit3.rom").unwrap(),
            "zakladny monitor PMD 85-3 velkost 8kB umiestnenie v pamati od adresy 0E000h"
        );
        assert_eq!(map.get("basic3.rmm").unwrap(), "BASIC G V3.0 pre PMD 85-3");
        // `notarom.txt` is cut off by the following entry header: the
        // separator is not required to end an entry.
        assert_eq!(map.get("notarom.txt").unwrap(), "this is not a rom");
        assert_eq!(map.len(), 3);
    }

    fn write_test_roms(dir: &std::path::Path) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("monit3.rom"), [0u8; 8192]).unwrap();
        fs::write(dir.join("basic3.rmm"), [0u8; 10240]).unwrap();
        fs::write(dir.join("README.md"), b"ignore me").unwrap();
        fs::write(dir.join("rom-list.txt"), SAMPLE_LIST).unwrap();
    }

    #[test]
    fn scans_and_classifies_rom_files() {
        let dir = std::env::temp_dir().join(format!("pmd85-catalog-test-{}", std::process::id()));
        write_test_roms(&dir);
        let catalog = RomCatalog::scan(&dir);
        let _ = fs::remove_dir_all(&dir);

        assert_eq!(catalog.monitors().len(), 1);
        assert_eq!(catalog.modules().len(), 1);
        let module = &catalog.modules()[0];
        assert_eq!(module.file_name, "basic3.rmm");
        assert_eq!(module.size, 10240);
        assert_eq!(module.size_label(), "10 kB");
        assert_eq!(module.description, "BASIC G V3.0 pre PMD 85-3");
        // Descriptions join with the monitor's too.
        assert!(catalog.monitors()[0].description.contains("0E000h"));
        // Lookup by name, including the model default monitor.
        assert_eq!(
            catalog
                .find(Model::Pmd853.default_monitor())
                .map(|e| &e.file_name),
            Some(&"monit3.rom".to_string())
        );
        assert!(catalog.find("nope.rom").is_none());
    }

    #[test]
    fn missing_directory_is_not_fatal() {
        let catalog = RomCatalog::scan("/nonexistent-pmd85-rom-dir");
        assert!(catalog.monitors().is_empty());
        assert!(catalog.modules().is_empty());
    }

    #[test]
    fn embedded_table_builds_the_same_catalog() {
        // A synthetic table, shaped like the one the build script
        // generates for the `embed-roms` feature.
        static FILES: &[(&str, &[u8])] = &[
            ("basic3.rmm", &[1, 2, 3]),
            ("monit3.rom", &[0x55; 8]),
            ("notarom.txt", b"ignored"),
            ("rom-list.txt", SAMPLE_LIST.as_bytes()),
        ];
        let catalog = RomCatalog::from_embedded(FILES);
        assert_eq!(catalog.monitors().len(), 1);
        assert_eq!(catalog.modules().len(), 1);
        // Classification and descriptions work like a scan.
        let monitor = catalog.find("monit3.rom").unwrap();
        assert!(monitor.description.contains("0E000h"));
        assert_eq!(monitor.size, 8);
        // The bytes come from the table, not a file.
        assert_eq!(monitor.read().unwrap(), vec![0x55u8; 8]);
        assert!(catalog.find("notarom.txt").is_none());
    }

    #[test]
    fn tape_library_scans_loadable_images() {
        let dir = std::env::temp_dir().join(format!("pmd85-tapes-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("b.pmd"), b"old").unwrap();
        fs::write(dir.join("a.ptp"), b"ptp").unwrap();
        fs::write(dir.join("notes.txt"), b"no").unwrap();
        fs::write(dir.join("games.rmm"), b"no").unwrap();
        let library = TapeLibrary::scan(&dir);
        assert_eq!(library.len(), 2);
        // Sorted, and only the loadable formats.
        assert_eq!(library.get(0).unwrap().file_name, "a.ptp");
        assert_eq!(library.get(1).unwrap().file_name, "b.pmd");
        assert_eq!(library.get(0).unwrap().read().unwrap(), b"ptp".to_vec());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tape_library_from_embedded_sorts_and_reads() {
        static FILES: &[(&str, &[u8])] = &[("b.ptp", &[9]), ("a.ptp", &[7])];
        let library = TapeLibrary::from_embedded(FILES);
        assert_eq!(library.len(), 2);
        assert_eq!(library.get(0).unwrap().file_name, "a.ptp");
        assert_eq!(library.get(0).unwrap().size, 1);
        assert_eq!(library.get(0).unwrap().read().unwrap(), vec![7]);
        assert!(library.get(2).is_none());
    }

    #[test]
    fn missing_tapes_directory_is_an_empty_library() {
        let library = TapeLibrary::scan("/nonexistent-pmd85-tapes-dir");
        assert!(library.is_empty());
    }
}
