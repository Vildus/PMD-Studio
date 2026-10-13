//! Command-line arguments. Every CLI switch overrides the persisted
//! settings for that run.

use pmd85_core::Model;
use std::path::PathBuf;

#[derive(Clone, Debug, Default)]
pub struct Args {
    /// `--model 85-3` (overrides settings).
    pub model: Option<Model>,
    /// `--monitor path.rom` (overrides the model default).
    pub monitor: Option<String>,
    /// `--rom-module path.rmm`.
    pub rom_module: Option<String>,
    /// `--rom-dir DIR` (default: the repository `Rom/` directory,
    /// or the embedded images in `embed-roms` builds).
    pub rom_dir: Option<PathBuf>,
    /// `--tapes-dir DIR`: bundled example tapes for the Tape window
    /// (default: embedded images in `embed-tapes` builds, none
    /// otherwise).
    pub tapes_dir: Option<PathBuf>,
    /// `--mute`.
    pub mute: bool,
}

pub fn usage() -> &'static str {
    "PMD 85 emulator\n\
     \n\
     usage: pmd85 [options]\n\
     \n\
     options:\n\
     \x20 --model <m>         85-1 | 85-2 | 85-2A | 85-3 (default 85-3)\n\
     \x20 --monitor <file>    monitor ROM file\n\
     \x20 --rom-module <file> ROM module (.rmm) file\n\
     \x20 --rom-dir <dir>     ROM directory (default: Rom/)\n\
     \x20 --tapes-dir <dir>   bundled example tapes for the Tape window\n\
     \x20 --mute              start with the speaker muted"
}

pub fn parse() -> Args {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--model" => {
                let v = it.next().unwrap_or_else(|| missing_value("--model"));
                args.model =
                    Some(Model::from_str_loose(&v).unwrap_or_else(|| {
                        panic!("unknown model {v:?} (85-1, 85-2, 85-2A, 85-3)")
                    }));
            }
            "--monitor" => {
                args.monitor = Some(it.next().unwrap_or_else(|| missing_value("--monitor")))
            }
            "--rom-module" => {
                args.rom_module = Some(it.next().unwrap_or_else(|| missing_value("--rom-module")))
            }
            "--rom-dir" => {
                args.rom_dir = Some(
                    it.next()
                        .unwrap_or_else(|| missing_value("--rom-dir"))
                        .into(),
                )
            }
            "--tapes-dir" => {
                args.tapes_dir = Some(
                    it.next()
                        .unwrap_or_else(|| missing_value("--tapes-dir"))
                        .into(),
                )
            }
            "--mute" => args.mute = true,
            "--help" | "-h" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument {other:?}\n\n{}", usage());
                std::process::exit(2);
            }
        }
    }
    args
}

fn missing_value(flag: &str) -> String {
    panic!("{flag} needs a value\n\n{}", usage());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_mentions_all_flags() {
        let usage = usage();
        for flag in [
            "--model",
            "--monitor",
            "--rom-module",
            "--rom-dir",
            "--tapes-dir",
            "--mute",
        ] {
            assert!(usage.contains(flag), "{flag} missing from usage");
        }
    }
}
