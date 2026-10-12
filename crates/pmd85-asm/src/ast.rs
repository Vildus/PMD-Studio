//! The statement/expression AST, with spans for diagnostics.

use crate::diags::Span;

/// An expression. `i32` arithmetic; the use site range-checks.
#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Num(i32),
    /// A symbol or label reference.
    Sym(String),
    /// `$`: the address of the current instruction.
    Pc,
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    And,
    Or,
    Xor,
    Shl,
    Shr,
}

/// One item of a `DB` list: a value or a string.
#[derive(Clone, Debug)]
pub enum DbItem {
    Expr(Expr),
    Str(Vec<u8>),
}

/// The instruction set, conditions baked into the kind. Mirrors the
/// 8080 encoding exactly (see `encode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstrKind {
    Nop,
    Hlt,
    Rlc,
    Rrc,
    Ral,
    Rar,
    Daa,
    Cma,
    Stc,
    Cmc,
    Xchg,
    Xthl,
    Pchl,
    Sphl,
    Ret,
    Ei,
    Di,
    /// `Rcc`: conditional return.
    Retc(Cond),
    /// `RST n`.
    Rst,
    /// `IN n` / `OUT n`.
    In,
    Out,
    /// `MOV r,r`.
    Mov,
    /// `MVI r,n`.
    Mvi,
    /// `LXI rp,nn`.
    Lxi,
    /// `STAX` / `LDAX` (B or D).
    Stax,
    Ldax,
    /// `DAD rp` (B, D, H, SP).
    Dad,
    /// `INX` / `DCX rp` (B, D, H, SP).
    Inx,
    Dcx,
    /// `INR r` / `DCR r`.
    Inr,
    Dcr,
    /// `PUSH` / `POP rp` (B, D, H, PSW).
    Push,
    Pop,
    /// `ADD r` ... `CMP r`.
    Add,
    Adc,
    Sub,
    Sbb,
    Ana,
    Xra,
    Ora,
    Cmp,
    /// `ADI n` ... `CPI n`.
    Adi,
    Aci,
    Sui,
    Sbi,
    Ani,
    Xri,
    Ori,
    Cpi,
    /// `JMP nn` / `CALL nn`.
    Jmp,
    Call,
    /// `Jcc nn` / `Ccc nn`.
    Jcc(Cond),
    Ccc(Cond),
    /// `SHLD` / `LHLD` / `STA` / `LDA nn`.
    Shld,
    Lhld,
    Sta,
    Lda,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cond {
    Nz,
    Z,
    Nc,
    C,
    Po,
    Pe,
    P,
    M,
}

/// The eight addressable registers, `M` being memory via HL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reg {
    B,
    C,
    D,
    E,
    H,
    L,
    M,
    A,
}

/// A register pair, as used by `LXI`/`DAD`/`INX`/`DCX`/`PUSH`/`POP`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pair {
    B,
    D,
    H,
    Sp,
    Psw,
}

/// One instruction operand, typed by what the mnemonic expects.
#[derive(Clone, Debug)]
pub enum Op {
    Reg(Reg),
    Pair(Pair),
    Expr(Expr),
}

/// One parsed statement.
#[derive(Clone, Debug)]
pub struct Line {
    /// Label defined at this line (`Some` for a definition, even
    /// without a trailing colon — the lenient classic form).
    pub label: Option<(String, Span)>,
    pub stmt: Stmt,
    pub span: Span,
    /// Trimmed source text, for the listing.
    pub text: String,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    /// Nothing but (at most) a label.
    Empty,
    Equ(Expr),
    Org(Expr),
    Db(Vec<DbItem>),
    Dw(Vec<Expr>),
    /// `DS size[, fill]`.
    Ds {
        size: Expr,
        fill: Option<Expr>,
    },
    /// `INCBIN "path"[, offset[, length]]`.
    IncBin {
        path: String,
        offset: Option<Expr>,
        length: Option<Expr>,
    },
    Align(Expr),
    /// `END [entry]`.
    End(Option<Expr>),
    Instr {
        kind: InstrKind,
        ops: Vec<Op>,
    },
}
