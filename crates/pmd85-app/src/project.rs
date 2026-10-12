//! The IDE project model: a directory with a `project.json`
//! manifest, the source files being edited, and the build.
//!
//! The manifest names the entry source and the target the program
//! is built for. The target is an open set: `tape` (a cassette
//! file the monitor loads), `bin` (raw bytes injected into RAM),
//! `rom-module` (a module image attached to the machine), with
//! more kinds to come (e.g. `vbg` pictures) — old manifests keep
//! working because every field has a default.

use std::path::{Path, PathBuf};

use pmd85_asm::{AsmOutput, SourceResolver};
use serde::{Deserialize, Serialize};

/// What the project builds into.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Target {
    /// A cassette-tape file, loaded by the monitor over the tape
    /// (flash) path. `name` is the file name the monitor shows
    /// (A-Z, digits; kept to 8 characters); with `autostart` the
    /// tape file is preceded by the wiki's autorun loader block,
    /// which the monitor runs after loading.
    Tape {
        name: String,
        #[serde(default = "default_true")]
        autostart: bool,
    },
    /// A raw binary: the image lands in RAM at its ORG, the CPU
    /// starts at the `END` entry (or the image base).
    Bin,
    /// A ROM-module image: the program becomes a `.rmm` cartridge the
    /// monitor BIOS transfers to RAM and runs at boot, through the
    /// connector's 8255.
    RomModule,
}

fn default_true() -> bool {
    true
}

impl Default for Target {
    fn default() -> Self {
        Target::Tape {
            name: "PROGRAM".to_string(),
            autostart: true,
        }
    }
}

impl Target {
    /// The short name for UI labels.
    pub fn kind(&self) -> &'static str {
        match self {
            Target::Tape { .. } => "tape",
            Target::Bin => "binary",
            Target::RomModule => "ROM module",
        }
    }
}

/// The `project.json` manifest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Manifest {
    /// Human-readable project name.
    pub name: String,
    /// The entry source file (relative to the project directory);
    /// further files come in through `INCLUDE`.
    pub main: String,
    /// What to build for.
    pub target: Target,
}

impl Default for Manifest {
    fn default() -> Self {
        Manifest {
            name: "project".to_string(),
            main: "main.asm".to_string(),
            target: Target::default(),
        }
    }
}

/// One source file being edited. `path` is relative to the project
/// directory; `text` is the buffer (possibly unsaved, `dirty`).
#[derive(Clone, Debug)]
pub struct SourceFile {
    pub path: String,
    pub text: String,
    pub dirty: bool,
}

/// The find/replace bar state.
#[derive(Clone, Debug, Default)]
pub struct FindState {
    pub open: bool,
    pub query: String,
    pub replace: String,
    /// 0-based index of the active match.
    pub active: usize,
    /// Number of matches for the current query (from the last pass).
    pub matches: usize,
    /// Byte offset of the active match in the active buffer (for
    /// the highlighter's backlit runs).
    pub active_match: Option<usize>,
}

/// One assembly of the project.
#[derive(Clone, Debug)]
pub struct Build {
    pub asm: AsmOutput,
}

impl Build {
    /// Diagnostics count for the status line.
    pub fn has_errors(&self) -> bool {
        self.asm.has_errors()
    }
}

/// An open project.
#[derive(Debug)]
pub struct Project {
    /// The project directory (holds `project.json`, the sources and
    /// the assets).
    pub dir: PathBuf,
    pub manifest: Manifest,
    /// The open source buffers, sorted by path.
    pub files: Vec<SourceFile>,
    /// Which buffer the editor shows.
    pub active: Option<usize>,
    /// The last build (None until the first Build).
    pub build: Option<Build>,
    /// The find/replace bar.
    pub find: FindState,
    /// The go-to-line popup.
    pub goto_open: bool,
    /// The go-to-line input.
    pub goto_line: String,
    /// The target-settings popup.
    pub target_open: bool,
    /// The new-file input in the tree.
    pub new_name: String,
}

/// Source file name suffixes that show up in the file tree.
const SOURCE_SUFFIXES: [&str; 3] = ["asm", "inc", "a80"];

impl Project {
    /// Open the project in `dir`: read `project.json` (with
    /// defaults for missing fields) and list the source files.
    pub fn open(dir: &Path) -> Result<Project, String> {
        let manifest_path = dir.join("project.json");
        let manifest = match std::fs::read_to_string(&manifest_path) {
            Ok(text) => serde_json::from_str::<Manifest>(&text)
                .map_err(|e| format!("{}: {e}", manifest_path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!("no project.json in {}", dir.display()))
            }
            Err(e) => return Err(format!("{}: {e}", manifest_path.display())),
        };
        let mut files = Vec::new();
        for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            let is_source = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| SOURCE_SUFFIXES.contains(&ext.to_ascii_lowercase().as_str()))
                && path.is_file();
            if is_source {
                let rel = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("?")
                    .to_string();
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                files.push(SourceFile {
                    path: rel,
                    text,
                    dirty: false,
                });
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let active = files
            .iter()
            .position(|f| f.path == manifest.main)
            .or(if files.is_empty() { None } else { Some(0) });
        Ok(Project {
            dir: dir.to_path_buf(),
            manifest,
            files,
            active,
            build: None,
            find: FindState::default(),
            goto_open: false,
            goto_line: String::new(),
            target_open: false,
            new_name: String::new(),
        })
    }

    /// Create a fresh project in `dir` (which must not yet hold a
    /// `project.json`): the manifest and a hello-world `main.asm`,
    /// then open it.
    pub fn create(dir: &Path, name: &str) -> Result<Project, String> {
        let manifest_path = dir.join("project.json");
        if manifest_path.exists() {
            return Err(format!("{} already exists", manifest_path.display()));
        }
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let manifest = Manifest {
            name: name.to_string(),
            ..Manifest::default()
        };
        let json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| format!("cannot serialize the manifest: {e}"))?;
        std::fs::write(&manifest_path, json)
            .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        let main = dir.join(&manifest.main);
        if !main.exists() {
            std::fs::write(&main, TEMPLATE).map_err(|e| format!("{}: {e}", main.display()))?;
        }
        Project::open(dir)
    }

    /// The buffer for `path`, if open. (Labels used by the UI.)
    fn file_index(&self, path: &str) -> Option<usize> {
        self.files.iter().position(|f| f.path == path)
    }

    /// Open a file into a buffer (from disk) and make it active.
    pub fn open_file(&mut self, path: &str) -> Result<(), String> {
        if self.file_index(path).is_some() {
            self.active = self.file_index(path);
            return Ok(());
        }
        let full = self.dir.join(path);
        let text =
            std::fs::read_to_string(&full).map_err(|e| format!("{}: {e}", full.display()))?;
        let at = self
            .files
            .binary_search_by(|f| f.path.as_str().cmp(path))
            .unwrap_or_else(|at| at);
        self.files.insert(
            at,
            SourceFile {
                path: path.to_string(),
                text,
                dirty: false,
            },
        );
        self.active = Some(at);
        Ok(())
    }

    /// Create a new empty source file (on disk and in the buffers).
    pub fn new_file(&mut self, path: &str) -> Result<(), String> {
        let clean = path.trim().trim_start_matches('/');
        if clean.is_empty() {
            return Err("empty file name".to_string());
        }
        let full = self.dir.join(clean);
        if full.exists() {
            return Err(format!("{} already exists", full.display()));
        }
        std::fs::write(&full, "").map_err(|e| format!("{}: {e}", full.display()))?;
        self.open_file(clean)
    }

    /// Save the active buffer to disk. Returns the file name saved.
    pub fn save_active(&mut self) -> Result<String, String> {
        let active = self.active.ok_or("no file open")?;
        self.save_file(active)
    }

    /// Save buffer `index` to disk. Returns the file name saved.
    pub fn save_file(&mut self, index: usize) -> Result<String, String> {
        let file = self.files.get_mut(index).ok_or("no such file")?;
        let full = self.dir.join(&file.path);
        std::fs::write(&full, &file.text).map_err(|e| format!("{}: {e}", full.display()))?;
        file.dirty = false;
        Ok(file.path.clone())
    }

    /// Save the manifest to `project.json`.
    pub fn save_manifest(&self) -> Result<(), String> {
        let path = self.dir.join("project.json");
        let json = serde_json::to_string_pretty(&self.manifest)
            .map_err(|e| format!("cannot serialize the manifest: {e}"))?;
        std::fs::write(&path, json).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Assemble the project: the entry source with the buffers as
    /// they are (unsaved edits included), files not open read from
    /// disk.
    pub fn build(&mut self) -> Build {
        let mut resolver = ProjectResolver {
            dir: &self.dir,
            files: &self.files,
        };
        let asm = pmd85_asm::assemble(&self.manifest.main, &mut resolver);
        Build { asm }
    }
}

/// Reads sources from the open buffers first, then from disk.
struct ProjectResolver<'a> {
    dir: &'a Path,
    files: &'a [SourceFile],
}

impl SourceResolver for ProjectResolver<'_> {
    fn read_text(&mut self, path: &str) -> Result<String, String> {
        if let Some(file) = self.files.iter().find(|f| f.path == path) {
            return Ok(file.text.clone());
        }
        std::fs::read_to_string(self.dir.join(path)).map_err(|e| e.to_string())
    }

    fn read_bytes(&mut self, path: &str) -> Result<Vec<u8>, String> {
        std::fs::read(self.dir.join(path)).map_err(|e| e.to_string())
    }
}

/// The hello-world written into a fresh `main.asm`: fills the screen
/// with colored stripes, straight into video RAM — no monitor
/// routines needed, and the result is plainly visible when run.
const TEMPLATE: &str = "\
; Fills the PMD 85 screen with colored stripes.
; Build with Ctrl+B, run with F5 (tape load) or Ctrl+F5 (direct).

        ORG 1000H
START:  LXI H,0C000H    ; video RAM: 256 rows of 64 bytes
        MVI B,0         ; stripe phase
FILL:   MOV A,B
        ANI 03H         ; color bits: 0 green, 1 red, 2 blue, 3 violet
        ORI 2AH         ; pixels 001010 within the color
        MOV M,A
        INX H
        MOV A,H
        ORA A           ; H wrapped to 0 = all 64 KiB done
        JNZ FILL
        INR B
        HLT

        END START
";

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_project(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pmd85-project-{name}-{}-{}",
            std::process::id(),
            name.len()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn manifest_defaults_and_targets() {
        let m: Manifest = serde_json::from_str("{}").unwrap();
        assert_eq!(m.main, "main.asm");
        assert_eq!(m.target, Target::default());
        let m: Manifest =
            serde_json::from_str(r#"{ "target": { "type": "tape", "name": "GAME" } }"#).unwrap();
        assert_eq!(
            m.target,
            Target::Tape {
                name: "GAME".to_string(),
                autostart: true
            }
        );
        // An unknown target kind is an error, not a silent default:
        // better the user fixes the manifest than builds the wrong thing.
        assert!(
            serde_json::from_str::<Manifest>(r#"{ "target": { "type": "cartridge" } }"#).is_err()
        );
    }

    #[test]
    fn create_build_and_save_round_trip() {
        let dir = temp_project("create");
        let mut project = Project::create(&dir, "hello").unwrap();
        assert!(dir.join("project.json").exists());
        assert!(dir.join("main.asm").exists());
        assert_eq!(project.active, Some(0));

        // Build the template: it must assemble clean.
        let build = project.build();
        assert!(
            build.asm.diagnostics.is_empty(),
            "{:#?}",
            build.asm.diagnostics
        );
        assert!(build.asm.entry.is_some_and(|e| e == 0x1000));

        // Edit the buffer (unsaved), rebuild: the build sees the edit.
        project.files[0].text = "MVI A,1\nHLT\n".to_string();
        let build = project.build();
        assert!(build.asm.diagnostics.is_empty());
        let (base, image) = build.asm.flat_image(0xFF).unwrap();
        assert_eq!(base, 0);
        assert_eq!(image, vec![0x3E, 0x01, 0x76]);

        // Save, then a fresh open sees the saved text.
        let saved = project.save_active().unwrap();
        assert_eq!(saved, "main.asm");
        let reopened = Project::open(&dir).unwrap();
        assert_eq!(reopened.files[0].text, "MVI A,1\nHLT\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn include_sees_buffers_and_disk() {
        let dir = temp_project("include");
        std::fs::write(dir.join("lib.asm"), "MVI B,7\n").unwrap();
        let mut project = Project::create(&dir, "inc").unwrap();
        let main = project
            .files
            .iter()
            .position(|f| f.path == "main.asm")
            .expect("main.asm is open");
        project.files[main].text = "INCLUDE \"lib.asm\"\nMVI A,1\n".to_string();
        // lib.asm is on disk but not open: still included.
        let build = project.build();
        assert!(
            build.asm.diagnostics.is_empty(),
            "{:#?}",
            build.asm.diagnostics
        );
        assert_eq!(
            build.asm.flat_image(0).unwrap().1,
            vec![0x06, 0x07, 0x3E, 0x01]
        );

        // An unsaved buffer with the same name shadows the disk file.
        project.open_file("lib.asm").unwrap();
        let lib = project
            .files
            .iter()
            .position(|f| f.path == "lib.asm")
            .expect("lib.asm is open");
        project.files[lib].text = "MVI B,9\n".to_string();
        let build = project.build();
        assert!(build.asm.diagnostics.is_empty());
        assert_eq!(
            build.asm.flat_image(0).unwrap().1,
            vec![0x06, 0x09, 0x3E, 0x01]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_requires_manifest() {
        let dir = temp_project("empty");
        assert!(Project::open(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn problems_format_with_file() {
        let dir = temp_project("problems");
        let mut project = Project::create(&dir, "p").unwrap();
        project.files[0].text = "MVI A,MISSING\n".to_string();
        let build = project.build();
        assert!(!build.asm.diagnostics.is_empty());
        let d = &build.asm.diagnostics[0];
        let file = build
            .asm
            .files
            .get(d.file)
            .map(String::as_str)
            .unwrap_or("?");
        assert!(
            format!("{file}:{}:{}:", d.line, d.col).starts_with("main.asm:1:"),
            "{file}:{}:{}",
            d.line,
            d.col
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
