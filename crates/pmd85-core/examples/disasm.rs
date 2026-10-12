//! Disassemble a range of a ROM image. Usage:
//! `disasm <rom> <start-hex-offset> <end-hex-offset> [base-hex]`
//! (offsets are file offsets; `base` is the address of file offset 0,
//! 0x8000 for the 85-1/2/2A monitors, 0xE000 for Monitor 3).
use pmd85_core::disasm::disassemble;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: disasm <rom> <start-hex> <end-hex> [base-hex]");
        std::process::exit(2);
    }
    let rom = std::fs::read(&args[1]).expect("cannot read ROM");
    let start = usize::from_str_radix(args[2].trim_start_matches("0x"), 16).expect("start");
    let end = usize::from_str_radix(args[3].trim_start_matches("0x"), 16).expect("end");
    let base = if args.len() > 4 {
        u16::from_str_radix(args[4].trim_start_matches("0x"), 16).expect("base")
    } else {
        0x8000
    };
    let mut pc = start;
    while pc < end && pc < rom.len() {
        let (text, len) = disassemble(base + pc as u16, &rom[pc..end.min(rom.len())]);
        println!("{:04X}: {text}", base + pc as u16);
        pc += len;
    }
}
