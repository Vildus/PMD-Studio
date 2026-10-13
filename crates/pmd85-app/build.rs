//! Build script.
//!
//! Windows .exe resources: embeds the app icon (and version info)
//! into the binary so Explorer, shortcuts and file dialogs show it.
//! This is separate from the runtime window icon winit sets.
//!
//! Everything else is a no-op: the script runs on every host but
//! only compiles resources when the *target* is Windows. Building on
//! Windows with the MSVC toolchain works out of the box (`rc.exe`);
//! the GNU toolchain needs `windres` on PATH.
//!
//! Resource embedding (portable build flavors): when the
//! `embed-roms` feature is on, the repository `Rom/` directory is
//! embedded into the binary (monitors, ROM modules, `rom-list.txt`);
//! `embed-tapes` embeds the `Tapes/` example tape images. The files
//! are copied into `OUT_DIR` and pulled in with `include_bytes!`
//! through a generated table, so the binary owns a private copy and
//! nothing on disk is needed at runtime.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    // Feature-gated embedding runs on every host; the variables are
    // set (to "1") whenever the feature — or a feature implying it —
    // is enabled.
    if env::var_os("CARGO_FEATURE_EMBED_ROMS").is_some() {
        embed_dir(
            "Rom",
            "embedded_roms.rs",
            "ROM_FILES",
            // Monitors, modules and the description list the catalog
            // joins them with. Other files in the directory (README
            // texts, the RAM-tester notes) stay out.
            |ext, name| {
                matches!(ext, "rom" | "rmm") || name == "rom-list.txt"
            },
        );
    }
    if env::var_os("CARGO_FEATURE_EMBED_TAPES").is_some() {
        embed_dir("Tapes", "embedded_tapes.rs", "TAPE_FILES", |ext, _| {
            // Only what the tape editor can open: PTP images and the
            // old format. The archive's .mrm/.p32 variants use other
            // tape formats and are not loadable.
            matches!(ext, "ptp" | "pmd")
        });
    }

    if env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "windows" {
        return;
    }
    // The icon lives at the repository root, next to its PNG source.
    println!("cargo:rerun-if-changed=../../assets/icon.ico");

    let version = env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/icon.ico");
    res.set("FileDescription", "PMD 85 emulator");
    res.set("ProductName", "slop-PMD");
    res.set("FileVersion", &version);
    res.set("ProductVersion", &version);
    // The numeric FILEVERSION/PRODUCTVERSION words (four 16-bit
    // parts packed into a u64, `major.minor.patch` + a trailing 0).
    let mut parts = [0u16; 4];
    for (slot, field) in version.split('.').take(4).enumerate() {
        parts[slot] = field.parse().unwrap_or(0);
    }
    let packed = (u64::from(parts[0]) << 48)
        | (u64::from(parts[1]) << 32)
        | (u64::from(parts[2]) << 16)
        | u64::from(parts[3]);
    res.set_version_info(winresource::VersionInfo::FILEVERSION, packed);
    res.set_version_info(winresource::VersionInfo::PRODUCTVERSION, packed);

    if let Err(e) = res.compile() {
        panic!("cannot embed the Windows resources: {e}");
    }
}

/// Copy every matching file of the repository directory `dir`
/// (next to `crates/`, e.g. `Rom`) into `OUT_DIR/embed-<dir>/` and
/// generate `OUT_DIR/<out_file>`: a `pub static <const_name>:
/// &[(&str, &[u8])]` table of `(file name, include_bytes!)` pairs,
/// sorted by name so the table is deterministic.
fn embed_dir(dir: &str, out_file: &str, const_name: &str, include: impl Fn(&str, &str) -> bool) {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = manifest.join("../..").join(dir);
    println!("cargo:rerun-if-changed={}", src.display());

    let mut names: Vec<String> = Vec::new();
    for entry in fs::read_dir(&src).unwrap_or_else(|e| {
        panic!("cannot embed {dir}/ ({}): {e} — the {dir} directory is required for this build flavor", src.display())
    }) {
        let path = entry.expect("readable directory entry").path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let ext = name
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        if include(&ext, name) {
            names.push(name.to_string());
        }
    }
    assert!(
        !names.is_empty(),
        "nothing to embed from {dir}/ — the build flavor needs its files"
    );
    names.sort();

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let embed_root = out_dir.join(format!("embed-{}", dir.to_lowercase()));
    let _ = fs::remove_dir_all(&embed_root);
    fs::create_dir_all(&embed_root).expect("cannot create the embedded copy directory");

    let mut table = String::new();
    for name in &names {
        fs::copy(src.join(name), embed_root.join(name))
            .unwrap_or_else(|e| panic!("cannot copy {name} for embedding: {e}"));
        // A path relative to the generated table (both live in
        // OUT_DIR), so odd absolute paths stay out of the source.
        table.push_str(&format!(
            "    ({name:?}, include_bytes!({:?})),\n",
            format!("{}/{name}", embed_root.file_name().unwrap().to_str().unwrap())
        ));
    }
    let code = format!(
        "/// Generated by build.rs — do not edit.\n\
         pub static {const_name}: &[(&str, &[u8])] = &[\n{table}];\n"
    );
    fs::write(out_dir.join(out_file), code).expect("cannot write the embedded file table");
}
