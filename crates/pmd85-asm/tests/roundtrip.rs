//! The round-trip oracle: disassemble every `Rom/` image with
//! `pmd85-core`'s disassembler, translate the Monitor-style listing
//! into our dialect, reassemble, and require the exact same bytes.
//! If the encoder disagrees with the disassembler by a single bit,
//! this catches it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pmd85_asm::{assemble, SourceResolver};
use pmd85_core::disasm::disassemble;

struct Mem {
    text: BTreeMap<String, String>,
}

impl SourceResolver for Mem {
    fn read_text(&mut self, path: &str) -> Result<String, String> {
        self.text
            .get(path)
            .cloned()
            .ok_or_else(|| format!("no such file {path:?}"))
    }

    fn read_bytes(&mut self, _path: &str) -> Result<Vec<u8>, String> {
        Err("not needed".to_string())
    }
}

/// Operand words the Monitor prints that are not numbers.
const WORDS: &[&str] = &["A", "B", "C", "D", "E", "H", "L", "M", "SP", "PSW"];

/// Translate one Monitor-style instruction line (`MVI A,3E`,
/// `LXI H,2400`, `DB 08`, `RST 3`) into our dialect: numbers get a
/// `0x` prefix, everything else stays.
fn translate(monitor: &str) -> String {
    let mut out = String::new();
    let mut first = true;
    for tok in monitor.replace(',', " , ").split_whitespace() {
        if tok == "," {
            out.push(',');
            continue;
        }
        if !first {
            out.push(' ');
        }
        if first || WORDS.contains(&tok) {
            out.push_str(tok);
        } else if tok.bytes().all(|b| b.is_ascii_hexdigit()) {
            out.push_str("0x");
            out.push_str(tok);
        } else {
            panic!("unexpected operand {tok:?} in {monitor:?}");
        }
        first = false;
    }
    out
}

fn rom_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Rom")
}

fn rom_images() -> Vec<std::path::PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(rom_dir())
        .expect("Rom directory")
        .map(|e| e.unwrap().path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("rmm") | Some("rom")
            )
        })
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no Rom images found");
    files
}

fn roundtrip(path: &Path) {
    let data = std::fs::read(path).expect("read image");
    let name = path.file_name().unwrap().to_string_lossy();

    let mut src = String::new();
    let mut pc = 0usize;
    while pc < data.len() {
        let (text, len) = disassemble(pc as u16, &data[pc..]);
        assert!(len > 0, "{name}: zero-length disassembly at {pc:#x}");
        src.push_str(&translate(&text));
        src.push('\n');
        pc += len;
    }

    let mut mem = Mem {
        text: [("main.asm".to_string(), src.clone())]
            .into_iter()
            .collect(),
    };
    let out = assemble("main.asm", &mut mem);
    if !out.diagnostics.is_empty() {
        for d in out.diagnostics.iter().take(5) {
            let ctx = src.lines().nth(d.line - 1).unwrap_or("?").to_string();
            eprintln!("{}:{}:{}: {} | line: {ctx}", name, d.line, d.col, d.message);
        }
    }
    assert!(out.diagnostics.is_empty(), "{name}: {:#?}", out.diagnostics);
    let (base, image) = out.flat_image(0).unwrap();
    assert_eq!(base, 0, "{name}: image not at 0");
    assert_eq!(image, data, "{name}: bytes differ after round-trip");
}

#[test]
fn all_rom_images_round_trip() {
    for path in rom_images() {
        roundtrip(&path);
    }
}
