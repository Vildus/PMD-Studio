//! The two-pass assembler driver.
//!
//! Pass 1 walks the parsed lines, assigns addresses (instruction
//! sizes never depend on operand values — the 8080 has no
//! PC-relative operands) and defines labels. `EQU`s resolve
//! afterwards, iteratively, so they may reference each other in any
//! order. Pass 2 encodes, building the segments and the listing.
//!
//! First-pass expressions (`ORG`, `DS`/`ALIGN`/`INCBIN` sizes) must
//! not use forward references — the classic two-pass restriction;
//! everything else may.

use std::collections::BTreeMap;

use crate::ast::{DbItem, Expr, ExprKind, Stmt};
use crate::diags::{Diagnostic, Span};
use crate::encode::{apply_bin, encode, instr_size};
use crate::parse::Parser;
use crate::SourceResolver;

/// A contiguous chunk of emitted bytes at a fixed address.
#[derive(Clone, Debug)]
pub struct Segment {
    pub start: u16,
    pub data: Vec<u8>,
}

impl Segment {
    pub fn end(&self) -> u32 {
        self.start as u32 + self.data.len() as u32
    }
}

/// One listed source line: its address, up to 16 of its bytes, and
/// the source text.
#[derive(Clone, Debug)]
pub struct ListingLine {
    pub file: usize,
    pub line: usize,
    pub addr: u16,
    pub bytes: Vec<u8>,
    pub text: String,
}

/// A statement's address mapped back to its source line.
#[derive(Clone, Copy, Debug)]
pub struct AddrLine {
    pub addr: u16,
    pub file: usize,
    pub line: usize,
}

/// The result of assembling: image, symbols, listing, problems.
#[derive(Clone, Debug, Default)]
pub struct AsmOutput {
    pub diagnostics: Vec<Diagnostic>,
    pub files: Vec<String>,
    pub symbols: BTreeMap<String, u16>,
    pub entry: Option<u16>,
    pub segments: Vec<Segment>,
    pub listing: Vec<ListingLine>,
    addr_index: Vec<AddrLine>,
}

impl AsmOutput {
    pub fn has_errors(&self) -> bool {
        !self.diagnostics.is_empty()
    }

    /// The whole image flattened to one run from the lowest to the
    /// highest segment address, gaps filled with `fill`. None when
    /// nothing was emitted.
    pub fn flat_image(&self, fill: u8) -> Option<(u16, Vec<u8>)> {
        let base = self.segments.iter().map(|s| s.start).min()?;
        let end = self.segments.iter().map(|s| s.end()).max()?;
        let mut image = vec![fill; (end - base as u32) as usize];
        for seg in &self.segments {
            let off = (seg.start - base) as usize;
            image[off..off + seg.data.len()].copy_from_slice(&seg.data);
        }
        Some((base, image))
    }

    /// The source line a program address belongs to (the last line
    /// starting at or before `addr`).
    pub fn source_line_at(&self, addr: u16) -> Option<(usize, usize)> {
        let idx = self
            .addr_index
            .partition_point(|e| e.addr <= addr)
            .checked_sub(1)?;
        let e = self.addr_index[idx];
        Some((e.file, e.line))
    }
}

/// Assemble `main` (and everything it includes) into an image.
pub fn assemble(main: &str, resolver: &mut dyn SourceResolver) -> AsmOutput {
    let mut parser = Parser::new(resolver);
    parser.parse_file(main);
    let Parser {
        files,
        lines,
        mut diags,
        ..
    } = parser;

    // ---- pass 1: addresses and labels ----

    struct Info {
        addr: u16,
        size: usize,
        scope: String,
    }

    let mut syms: BTreeMap<String, i32> = BTreeMap::new();
    // name, expr, scope, pc at definition, span
    let mut pending: Vec<(String, Expr, String, u16, Span)> = Vec::new();
    let mut incbin: Vec<Option<Vec<u8>>> = vec![None; lines.len()];
    let mut infos: Vec<Info> = Vec::with_capacity(lines.len());

    let mut pc: u16 = 0;
    let mut scope = String::new();

    for (idx, line) in lines.iter().enumerate() {
        // Define the label (EQU goes to the fixpoint pile).
        if let Some((name, lspan)) = &line.label {
            let cname = canon(name, &scope);
            if !name.starts_with('.') {
                scope = cname.clone();
            }
            match &line.stmt {
                Stmt::Equ(expr) => {
                    pending.push((cname, expr.clone(), scope.clone(), pc, *lspan));
                }
                _ => {
                    if syms.insert(cname, pc as i32).is_some() {
                        diags.push(Diagnostic::error(
                            *lspan,
                            format!("duplicate symbol {name}"),
                        ));
                    }
                }
            }
        }

        let mut size = 0usize;
        match &line.stmt {
            Stmt::Empty | Stmt::Equ(_) => {}
            Stmt::Org(e) => match eval(e, pc, &scope, &syms) {
                Ok(v) => match u16::try_from(v) {
                    Ok(a) => pc = a,
                    Err(_) => diags.push(Diagnostic::error(
                        line.span,
                        format!("ORG address {v} out of range"),
                    )),
                },
                Err((span, msg)) => diags.push(Diagnostic::error(
                    span,
                    format!("{msg} (ORG must not use forward references)"),
                )),
            },
            Stmt::Db(items) => {
                for item in items {
                    size += match item {
                        DbItem::Expr(_) => 1,
                        DbItem::Str(b) => b.len(),
                    };
                }
            }
            Stmt::Dw(exprs) => size = exprs.len() * 2,
            Stmt::Ds { size: e, .. } => match eval(e, pc, &scope, &syms) {
                Ok(v) if (0..=0x10000).contains(&v) => size = v as usize,
                Ok(v) => diags.push(Diagnostic::error(
                    line.span,
                    format!("DS size {v} out of range"),
                )),
                Err((span, msg)) => diags.push(Diagnostic::error(
                    span,
                    format!("{msg} (DS sizes must not use forward references)"),
                )),
            },
            Stmt::IncBin {
                path,
                offset,
                length,
            } => {
                match resolver.read_bytes(path) {
                    Ok(data) => {
                        let off = match offset {
                        Some(e) => eval(e, pc, &scope, &syms)
                            .map(|v| v.max(0) as usize)
                            .unwrap_or_else(|(span, msg)| {
                                diags.push(Diagnostic::error(
                                    span,
                                    format!("{msg} (INCBIN offsets must not use forward references)"),
                                ));
                                usize::MAX
                            }),
                        None => 0,
                    };
                        let len = match length {
                            Some(e) => eval(e, pc, &scope, &syms)
                                .map(|v| v.max(0) as usize)
                                .unwrap_or(usize::MAX),
                            None => usize::MAX,
                        };
                        let data = if off >= data.len() {
                            if off != usize::MAX {
                                diags.push(Diagnostic::error(
                                    line.span,
                                    format!("INCBIN offset {off} past the end of {path:?}"),
                                ));
                            }
                            Vec::new()
                        } else {
                            let end = off.saturating_add(len).min(data.len());
                            data[off..end].to_vec()
                        };
                        size = data.len();
                        incbin[idx] = Some(data);
                    }
                    Err(e) => diags.push(Diagnostic::error(
                        line.span,
                        format!("cannot read {path}: {e}"),
                    )),
                }
            }
            Stmt::Align(e) => match eval(e, pc, &scope, &syms) {
                Ok(v) if v > 0 && v <= 0x10000 && (v as u32).is_power_of_two() => {
                    let a = v as u32;
                    size = ((a - pc as u32 % a) % a) as usize;
                }
                Ok(v) => diags.push(Diagnostic::error(
                    line.span,
                    format!("ALIGN must be a positive power of two, not {v}"),
                )),
                Err((span, msg)) => diags.push(Diagnostic::error(
                    span,
                    format!("{msg} (ALIGN must not use forward references)"),
                )),
            },
            Stmt::End(_) => {}
            Stmt::Instr { kind, .. } => size = instr_size(*kind),
        }

        // Advance the PC, watching the 64 KiB address space.
        let next = pc as u32 + size as u32;
        if next > 0x10000 {
            diags.push(Diagnostic::error(
                line.span,
                "address space exhausted at this statement",
            ));
            pc = 0xFFFF;
        } else {
            pc = next as u16;
        }
        infos.push(Info {
            addr: pc.wrapping_sub(size as u16),
            size,
            scope: scope.clone(),
        });
    }

    // ---- resolve the EQU pile to a fixpoint ----
    let mut progress = true;
    while progress && !pending.is_empty() {
        progress = false;
        pending.retain(|(name, expr, scope, def_pc, span)| {
            match eval_inner(expr, *def_pc, scope, &syms) {
                Ok(v) => {
                    if syms.insert(name.clone(), v).is_some() {
                        diags.push(Diagnostic::error(*span, format!("duplicate symbol {name}")));
                    }
                    progress = true;
                    false
                }
                Err(EvalErr::Undefined(..)) => true, // keep waiting
                Err(EvalErr::Hard(span, msg)) => {
                    diags.push(Diagnostic::error(span, msg));
                    false
                }
            }
        });
    }
    for (name, _, _, _, span) in &pending {
        diags.push(Diagnostic::error(
            *span,
            format!("symbol {name} is undefined or circular"),
        ));
    }

    // ---- pass 2: encode, segments, listing, entry ----
    let mut segments: Vec<Segment> = Vec::new();
    let mut listing: Vec<ListingLine> = Vec::new();
    let mut entry: Option<u16> = None;

    for (idx, line) in lines.iter().enumerate() {
        let info = &infos[idx];
        let mut bytes: Vec<u8> = Vec::new();
        match &line.stmt {
            Stmt::Empty | Stmt::Equ(_) | Stmt::Org(_) => {}
            Stmt::Db(items) => {
                for item in items {
                    match item {
                        DbItem::Str(b) => bytes.extend_from_slice(b),
                        DbItem::Expr(e) => match eval(e, info.addr, &info.scope, &syms) {
                            Ok(v) if (-128..=255).contains(&v) => bytes.push(v as u8),
                            Ok(v) => {
                                diags.push(Diagnostic::error(
                                    e.span,
                                    format!("byte value {v} out of range"),
                                ));
                                bytes.push(0);
                            }
                            Err((span, msg)) => {
                                diags.push(Diagnostic::error(span, msg));
                                bytes.push(0);
                            }
                        },
                    }
                }
            }
            Stmt::Dw(exprs) => {
                for e in exprs {
                    match eval(e, info.addr, &info.scope, &syms) {
                        Ok(v) if (-32768..=65535).contains(&v) => {
                            bytes.extend_from_slice(&(v as u16).to_le_bytes())
                        }
                        Ok(v) => {
                            diags.push(Diagnostic::error(
                                e.span,
                                format!("word value {v} out of range"),
                            ));
                            bytes.extend_from_slice(&[0, 0]);
                        }
                        Err((span, msg)) => {
                            diags.push(Diagnostic::error(span, msg));
                            bytes.extend_from_slice(&[0, 0]);
                        }
                    }
                }
            }
            Stmt::Ds { fill, .. } => {
                let fill = fill
                    .as_ref()
                    .map(|e| eval(e, info.addr, &info.scope, &syms))
                    .unwrap_or(Ok(0))
                    .ok()
                    .filter(|v| (-128..=255).contains(v))
                    .unwrap_or(0) as u8;
                bytes.resize(info.size, fill);
            }
            Stmt::Align(_) => bytes.resize(info.size, 0),
            Stmt::IncBin { .. } => {
                if let Some(data) = &incbin[idx] {
                    bytes = data.clone();
                }
            }
            Stmt::End(e) => {
                if let Some(e) = e {
                    match eval(e, info.addr, &info.scope, &syms) {
                        Ok(v) if (-32768..=65535).contains(&v) => entry = Some(v as u16),
                        Ok(v) => diags.push(Diagnostic::error(
                            e.span,
                            format!("entry point {v} out of range"),
                        )),
                        Err((span, msg)) => diags.push(Diagnostic::error(span, msg)),
                    }
                }
            }
            Stmt::Instr { kind, ops } => match encode(*kind, line.span, ops, |e| {
                eval(e, info.addr, &info.scope, &syms)
            }) {
                Ok(mut b) => bytes.append(&mut b),
                Err((span, msg)) => {
                    diags.push(Diagnostic::error(span, msg));
                    // Keep the size so later addresses hold.
                    bytes.resize(instr_size(*kind), 0);
                }
            },
        }

        if !bytes.is_empty() {
            place(&mut segments, info.addr, &bytes, line.span, &mut diags);
        }
        let shown = bytes.len().min(16);
        listing.push(ListingLine {
            file: line.span.file,
            line: line.span.line,
            addr: info.addr,
            bytes: bytes[..shown].to_vec(),
            text: line.text.clone(),
        });
    }

    // Address -> first source line, for the debugger.
    let mut addr_index: Vec<AddrLine> = listing
        .iter()
        .map(|l| AddrLine {
            addr: l.addr,
            file: l.file,
            line: l.line,
        })
        .collect();
    addr_index.sort_by_key(|e| e.addr);
    addr_index.dedup_by_key(|e| e.addr);

    AsmOutput {
        symbols: syms.iter().map(|(k, v)| (k.clone(), *v as u16)).collect(),
        diagnostics: diags,
        files,
        entry,
        segments,
        listing,
        addr_index,
    }
}

/// Append `bytes` at `addr`: continue the last segment when it ends
/// exactly there, splice (with a diagnostic) on overlap, start a new
/// segment otherwise.
fn place(
    segments: &mut Vec<Segment>,
    addr: u16,
    bytes: &[u8],
    span: Span,
    diags: &mut Vec<Diagnostic>,
) {
    match segments.last_mut() {
        Some(s) if s.end() == addr as u32 => s.data.extend_from_slice(bytes),
        Some(s) if addr >= s.start && (addr as u32) < s.end() => {
            diags.push(Diagnostic::error(
                span,
                "this statement overlaps the previous segment",
            ));
            let off = (addr - s.start) as usize;
            s.data.truncate(off);
            s.data.extend_from_slice(bytes);
        }
        _ => segments.push(Segment {
            start: addr,
            data: bytes.to_vec(),
        }),
    }
}

/// Canonical symbol name: uppercase, locals (`.x`) prefixed with the
/// enclosing global label.
fn canon(name: &str, scope: &str) -> String {
    let n = name.to_ascii_uppercase();
    if n.starts_with('.') && !scope.is_empty() {
        format!("{scope}{n}")
    } else {
        n
    }
}

enum EvalErr {
    Undefined(Span, String),
    Hard(Span, String),
}

/// Evaluate an expression: `i32` arithmetic, `$` as `pc`, symbols
/// canonicalized in `scope`. Undefined symbols are errors.
fn eval(
    expr: &Expr,
    pc: u16,
    scope: &str,
    syms: &BTreeMap<String, i32>,
) -> Result<i32, (Span, String)> {
    match eval_inner(expr, pc, scope, syms) {
        Ok(v) => Ok(v),
        Err(EvalErr::Undefined(span, name)) => Err((span, format!("undefined symbol {name}"))),
        Err(EvalErr::Hard(span, msg)) => Err((span, msg)),
    }
}

fn eval_inner(
    expr: &Expr,
    pc: u16,
    scope: &str,
    syms: &BTreeMap<String, i32>,
) -> Result<i32, EvalErr> {
    Ok(match &expr.kind {
        ExprKind::Num(v) => *v,
        ExprKind::Pc => pc as i32,
        ExprKind::Sym(name) => {
            let c = canon(name, scope);
            match syms.get(&c) {
                Some(v) => *v,
                None => return Err(EvalErr::Undefined(expr.span, c)),
            }
        }
        ExprKind::Neg(e) => -eval_inner(e, pc, scope, syms)?,
        ExprKind::Not(e) => !eval_inner(e, pc, scope, syms)?,
        ExprKind::Bin(op, a, b) => {
            let (va, vb) = (
                eval_inner(a, pc, scope, syms)?,
                eval_inner(b, pc, scope, syms)?,
            );
            apply_bin(*op, va, vb).map_err(|m| EvalErr::Hard(a.span, m))?
        }
    })
}
