//! The 8080 instruction encoding: name lookup, operand patterns,
//! instruction sizes and byte emission. The table mirrors
//! `pmd85-core`'s disassembler opcode for opcode.
//!
//! Errors carry the span of the offending operand (or of the whole
//! statement for structural complaints like `MOV M,M`).

use crate::ast::{BinOp, Cond, Expr, InstrKind, Op, Pair, Reg};
use crate::diags::Span;

/// Look up a mnemonic (case-insensitive). None for directives and
/// unknown words.
pub fn lookup_mnemonic(name: &str) -> Option<InstrKind> {
    use Cond::*;
    use InstrKind::*;
    let kind = match name.to_ascii_uppercase().as_str() {
        "NOP" => Nop,
        "HLT" => Hlt,
        "RLC" => Rlc,
        "RRC" => Rrc,
        "RAL" => Ral,
        "RAR" => Rar,
        "DAA" => Daa,
        "CMA" => Cma,
        "STC" => Stc,
        "CMC" => Cmc,
        "XCHG" => Xchg,
        "XTHL" => Xthl,
        "PCHL" => Pchl,
        "SPHL" => Sphl,
        "RET" => Ret,
        "EI" => Ei,
        "DI" => Di,
        "RNZ" => Retc(Nz),
        "RZ" => Retc(Z),
        "RNC" => Retc(Nc),
        "RC" => Retc(C),
        "RPO" => Retc(Po),
        "RPE" => Retc(Pe),
        "RP" => Retc(P),
        "RM" => Retc(M),
        "RST" => Rst,
        "IN" => In,
        "OUT" => Out,
        "MOV" => Mov,
        "MVI" => Mvi,
        "LXI" => Lxi,
        "STAX" => Stax,
        "LDAX" => Ldax,
        "DAD" => Dad,
        "INX" => Inx,
        "DCX" => Dcx,
        "INR" => Inr,
        "DCR" => Dcr,
        "PUSH" => Push,
        "POP" => Pop,
        "ADD" => Add,
        "ADC" => Adc,
        "SUB" => Sub,
        "SBB" => Sbb,
        "ANA" => Ana,
        "XRA" => Xra,
        "ORA" => Ora,
        "CMP" => Cmp,
        "ADI" => Adi,
        "ACI" => Aci,
        "SUI" => Sui,
        "SBI" => Sbi,
        "ANI" => Ani,
        "XRI" => Xri,
        "ORI" => Ori,
        "CPI" => Cpi,
        "JMP" => Jmp,
        "CALL" => Call,
        "JNZ" => Jcc(Nz),
        "JZ" => Jcc(Z),
        "JNC" => Jcc(Nc),
        "JC" => Jcc(C),
        "JPO" => Jcc(Po),
        "JPE" => Jcc(Pe),
        "JP" => Jcc(P),
        "JM" => Jcc(M),
        "CNZ" => Ccc(Nz),
        "CZ" => Ccc(Z),
        "CNC" => Ccc(Nc),
        "CC" => Ccc(C),
        "CPO" => Ccc(Po),
        "CPE" => Ccc(Pe),
        "CP" => Ccc(P),
        "CM" => Ccc(M),
        "SHLD" => Shld,
        "LHLD" => Lhld,
        "STA" => Sta,
        "LDA" => Lda,
        _ => return None,
    };
    Some(kind)
}

/// What an operand slot accepts.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// A single register A B C D E H L M.
    Reg,
    /// B D H SP (LXI, DAD, INX, DCX).
    Pair,
    /// B D H SP PSW (PUSH, POP).
    PairPsw,
    /// A value.
    Expr,
}

/// The operand pattern of a kind.
pub fn pattern(kind: InstrKind) -> &'static [Slot] {
    use InstrKind::*;
    match kind {
        Nop | Hlt | Rlc | Rrc | Ral | Rar | Daa | Cma | Stc | Cmc | Xchg | Xthl | Pchl | Sphl
        | Ret | Ei | Di | Retc(_) => &[],
        Rst | In | Out => &[Slot::Expr],
        Mov => &[Slot::Reg, Slot::Reg],
        Mvi => &[Slot::Reg, Slot::Expr],
        Lxi => &[Slot::Pair, Slot::Expr],
        Stax | Ldax | Dad | Inx | Dcx => &[Slot::Pair],
        Push | Pop => &[Slot::PairPsw],
        Add | Adc | Sub | Sbb | Ana | Xra | Ora | Cmp => &[Slot::Reg],
        Inr | Dcr => &[Slot::Reg],
        Adi | Aci | Sui | Sbi | Ani | Xri | Ori | Cpi => &[Slot::Expr],
        Jmp | Call | Jcc(_) | Ccc(_) | Shld | Lhld | Sta | Lda => &[Slot::Expr],
    }
}

/// The encoded size in bytes, before the expressions are evaluated
/// (the 8080 has no PC-relative operands).
pub fn instr_size(kind: InstrKind) -> usize {
    use InstrKind::*;
    match kind {
        Mvi | In | Out | Adi | Aci | Sui | Sbi | Ani | Xri | Ori | Cpi => 2,
        Lxi | Jmp | Call | Jcc(_) | Ccc(_) | Shld | Lhld | Sta | Lda => 3,
        _ => 1,
    }
}

fn reg_bits(r: Reg) -> u8 {
    match r {
        Reg::B => 0,
        Reg::C => 1,
        Reg::D => 2,
        Reg::E => 3,
        Reg::H => 4,
        Reg::L => 5,
        Reg::M => 6,
        Reg::A => 7,
    }
}

fn cond_bits(c: Cond) -> u8 {
    match c {
        Cond::Nz => 0,
        Cond::Z => 1,
        Cond::Nc => 2,
        Cond::C => 3,
        Cond::Po => 4,
        Cond::Pe => 5,
        Cond::P => 6,
        Cond::M => 7,
    }
}

fn alu_bits(kind: InstrKind) -> u8 {
    use InstrKind::*;
    match kind {
        Add | Adi => 0,
        Adc | Aci => 1,
        Sub | Sui => 2,
        Sbb | Sbi => 3,
        Ana | Ani => 4,
        Xra | Xri => 5,
        Ora | Ori => 6,
        Cmp | Cpi => 7,
        _ => unreachable!("not an ALU op"),
    }
}

/// Encode one instruction. `span` is the statement (used for
/// structural errors); `eval` evaluates operand expressions and
/// reports problems with the offending operand's span.
pub fn encode(
    kind: InstrKind,
    span: Span,
    ops: &[Op],
    mut eval: impl FnMut(&Expr) -> Result<i32, (Span, String)>,
) -> Result<Vec<u8>, (Span, String)> {
    use InstrKind::*;
    type Err = (Span, String);
    let byte = |e: &Expr, eval: &mut dyn FnMut(&Expr) -> Result<i32, Err>| -> Result<u8, Err> {
        let v = eval(e)?;
        if !(-128..=255).contains(&v) {
            return Err((e.span, format!("byte operand {v} out of range")));
        }
        Ok(v as u8)
    };
    let word = |e: &Expr, eval: &mut dyn FnMut(&Expr) -> Result<i32, Err>| -> Result<u16, Err> {
        let v = eval(e)?;
        if !(-32768..=65535).contains(&v) {
            return Err((e.span, format!("word operand {v} out of range")));
        }
        Ok(v as u16)
    };
    // The parser already type-checked operands against the pattern;
    // these errors are unreachable in practice.
    fn expr(op: &Op, span: Span) -> Result<&Expr, Err> {
        match op {
            Op::Expr(e) => Ok(e),
            _ => Err((span, "expected a value".to_string())),
        }
    }
    let reg = |op: &Op| -> Result<Reg, Err> {
        match op {
            Op::Reg(r) => Ok(*r),
            _ => Err((span, "expected a register".to_string())),
        }
    };
    let pair = |op: &Op, psw: bool| -> Result<Pair, Err> {
        match op {
            Op::Pair(p) if psw || *p != Pair::Psw => Ok(*p),
            _ => Err((
                span,
                if psw {
                    "expected B, D, H, SP or PSW".to_string()
                } else {
                    "expected B, D, H or SP".to_string()
                },
            )),
        }
    };

    let mut bytes = Vec::with_capacity(3);
    match kind {
        Nop => bytes.push(0x00),
        Hlt => bytes.push(0x76),
        Rlc => bytes.push(0x07),
        Rrc => bytes.push(0x0F),
        Ral => bytes.push(0x17),
        Rar => bytes.push(0x1F),
        Daa => bytes.push(0x27),
        Cma => bytes.push(0x2F),
        Stc => bytes.push(0x37),
        Cmc => bytes.push(0x3F),
        Xchg => bytes.push(0xEB),
        Xthl => bytes.push(0xE3),
        Pchl => bytes.push(0xE9),
        Sphl => bytes.push(0xF9),
        Ret => bytes.push(0xC9),
        Ei => bytes.push(0xFB),
        Di => bytes.push(0xF3),
        Retc(c) => bytes.push(0xC0 | (cond_bits(c) << 3)),
        Rst => {
            let n = byte(expr(&ops[0], span)?, &mut eval)?;
            if n > 7 {
                return Err((span, format!("RST vector {n} out of range 0..=7")));
            }
            bytes.push(0xC7 | (n << 3));
        }
        In => {
            bytes.push(0xDB);
            bytes.push(byte(expr(&ops[0], span)?, &mut eval)?);
        }
        Out => {
            bytes.push(0xD3);
            bytes.push(byte(expr(&ops[0], span)?, &mut eval)?);
        }
        Mov => {
            let (d, s) = (reg(&ops[0])?, reg(&ops[1])?);
            if d == Reg::M && s == Reg::M {
                return Err((span, "MOV M,M is the HLT opcode; write HLT".to_string()));
            }
            bytes.push(0x40 | (reg_bits(d) << 3) | reg_bits(s));
        }
        Mvi => {
            bytes.push(0x06 | (reg_bits(reg(&ops[0])?) << 3));
            bytes.push(byte(expr(&ops[1], span)?, &mut eval)?);
        }
        Lxi => {
            let pair_bits = match pair(&ops[0], false)? {
                Pair::B => 0u8,
                Pair::D => 1,
                Pair::H => 2,
                Pair::Sp => 3,
                Pair::Psw => unreachable!("pattern excludes PSW"),
            };
            let w = word(expr(&ops[1], span)?, &mut eval)?;
            bytes.push(0x01 | (pair_bits << 4));
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        Stax | Ldax => {
            let pair_bits = match pair(&ops[0], false)? {
                Pair::B => 0u8,
                Pair::D => 1,
                _ => return Err((span, "expected B or D".to_string())),
            };
            let base = if kind == Stax { 0x02 } else { 0x0A };
            bytes.push(base | (pair_bits << 4));
        }
        Dad => {
            let pair_bits = match pair(&ops[0], false)? {
                Pair::B => 0u8,
                Pair::D => 1,
                Pair::H => 2,
                Pair::Sp => 3,
                Pair::Psw => unreachable!("pattern excludes PSW"),
            };
            bytes.push(0x09 | (pair_bits << 4));
        }
        Inx | Dcx => {
            let pair_bits = match pair(&ops[0], false)? {
                Pair::B => 0u8,
                Pair::D => 1,
                Pair::H => 2,
                Pair::Sp => 3,
                Pair::Psw => unreachable!("pattern excludes PSW"),
            };
            let base = if kind == Inx { 0x03 } else { 0x0B };
            bytes.push(base | (pair_bits << 4));
        }
        Push | Pop => {
            let pair_bits = match pair(&ops[0], true)? {
                Pair::B => 0u8,
                Pair::D => 1,
                Pair::H => 2,
                // SP and PSW share the encoding slot.
                Pair::Sp | Pair::Psw => 3,
            };
            let base = if kind == Push { 0xC5 } else { 0xC1 };
            bytes.push(base | (pair_bits << 4));
        }
        Inr | Dcr => {
            bytes.push(if kind == Inr { 0x04 } else { 0x05 } | (reg_bits(reg(&ops[0])?) << 3));
        }
        Add | Adc | Sub | Sbb | Ana | Xra | Ora | Cmp => {
            bytes.push(0x80 | (alu_bits(kind) << 3) | reg_bits(reg(&ops[0])?));
        }
        Adi | Aci | Sui | Sbi | Ani | Xri | Ori | Cpi => {
            bytes.push(0xC6 | (alu_bits(kind) << 3));
            bytes.push(byte(expr(&ops[0], span)?, &mut eval)?);
        }
        Jmp | Call => {
            let w = word(expr(&ops[0], span)?, &mut eval)?;
            bytes.push(if kind == Jmp { 0xC3 } else { 0xCD });
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        Jcc(c) => {
            let w = word(expr(&ops[0], span)?, &mut eval)?;
            bytes.push(0xC2 | (cond_bits(c) << 3));
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        Ccc(c) => {
            let w = word(expr(&ops[0], span)?, &mut eval)?;
            bytes.push(0xC4 | (cond_bits(c) << 3));
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        Shld => {
            let w = word(expr(&ops[0], span)?, &mut eval)?;
            bytes.push(0x22);
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        Lhld => {
            let w = word(expr(&ops[0], span)?, &mut eval)?;
            bytes.push(0x2A);
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        Sta => {
            let w = word(expr(&ops[0], span)?, &mut eval)?;
            bytes.push(0x32);
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        Lda => {
            let w = word(expr(&ops[0], span)?, &mut eval)?;
            bytes.push(0x3A);
            bytes.extend_from_slice(&w.to_le_bytes());
        }
    }
    Ok(bytes)
}

/// Binary operator application, for the evaluator in `asm.rs`.
pub fn apply_bin(op: BinOp, a: i32, b: i32) -> Result<i32, String> {
    Ok(match op {
        BinOp::Add => a.wrapping_add(b),
        BinOp::Sub => a.wrapping_sub(b),
        BinOp::Mul => a.wrapping_mul(b),
        BinOp::Div => {
            if b == 0 {
                return Err("division by zero".to_string());
            }
            a.wrapping_div(b)
        }
        BinOp::Mod => {
            if b == 0 {
                return Err("division by zero".to_string());
            }
            a.wrapping_rem(b)
        }
        BinOp::And => a & b,
        BinOp::Or => a | b,
        BinOp::Xor => a ^ b,
        BinOp::Shl => a.wrapping_shl(b as u32),
        BinOp::Shr => a.wrapping_shr(b as u32),
    })
}
