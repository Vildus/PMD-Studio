//! The parser: lexed lines to [`Line`] statements.
//!
//! Line grammar, classic assembler style with a lenient eye:
//!
//! ```text
//! [label[:]] [opcode|directive [operands]] [; comment]
//! ```
//!
//! A bare identifier is a label when the next word is an opcode or
//! the line ends; `name EQU expr` defines a constant. `INCLUDE
//! "file"` splices another file at parse time (cycles are reported).
//!
//! Macros: `name MACRO p1, p2, ...` captures the following lines up
//! to `ENDM` as a body; `name arg1, arg2, ...` invokes it, splicing
//! the body back with the parameters substituted token-wise. Labels
//! inside a body can be declared `LOCAL name, ...`: each invocation
//! renames them to a unique symbol, so repeated invocations do not
//! collide. Definitions must precede uses (as with INCLUDE), and
//! nesting definitions inside bodies is rejected — but bodies may
//! invoke other macros, up to a fixed depth.

use crate::ast::*;
use crate::diags::{Diagnostic, Span};
use crate::encode::{lookup_mnemonic, pattern, Slot};
use crate::lexer::{lex_lines, LexLine, Tok, Token};
use crate::SourceResolver;

/// A macro definition: its name, parameter names (canonical,
/// uppercase) and the raw body lines up to the matching `ENDM`.
#[derive(Clone, Debug)]
pub struct MacroDef {
    pub name: String,
    pub params: Vec<String>,
    pub body: Vec<LexLine>,
}

/// How deep macro invocations may nest before the assembler gives up
/// (a self-invoking macro would otherwise expand forever).
const MACRO_DEPTH_MAX: usize = 32;

pub struct Parser<'a> {
    resolver: &'a mut dyn SourceResolver,
    /// File paths by index; `Span::file` indexes this.
    pub files: Vec<String>,
    /// Parsed lines, in emission order (INCLUDE spliced in place).
    pub lines: Vec<Line>,
    pub diags: Vec<Diagnostic>,
    /// INCLUDE chain, for cycle detection.
    stack: Vec<String>,
    /// The current file's lexed lines, for `MACRO` bodies to pull
    /// from; INCLUDE (and macro definitions inside expansions) swap
    /// it out and restore it.
    iter: Option<std::vec::IntoIter<LexLine>>,
    /// Macros defined so far — definitions must precede their uses,
    /// as with INCLUDE.
    pub macros: Vec<MacroDef>,
    /// Current macro expansion depth.
    macro_depth: usize,
    /// Counter for the per-invocation names of `LOCAL` labels.
    local_seq: usize,
}

impl<'a> Parser<'a> {
    pub fn new(resolver: &'a mut dyn SourceResolver) -> Self {
        Parser {
            resolver,
            files: Vec::new(),
            lines: Vec::new(),
            diags: Vec::new(),
            stack: Vec::new(),
            iter: None,
            macros: Vec::new(),
            macro_depth: 0,
            local_seq: 0,
        }
    }

    /// Parse `path` and everything it includes.
    pub fn parse_file(&mut self, path: &str) {
        if self.stack.iter().any(|p| p == path) {
            // The cycle is reported at the offending INCLUDE below;
            // just do not recurse again.
            return;
        }
        match self.resolver.read_text(path) {
            Ok(text) => {
                let file = self.files.len();
                self.files.push(path.to_string());
                self.stack.push(path.to_string());
                let lines = lex_lines(file, &text, &mut self.diags);
                // The lines are pulled one by one (not in a `for`
                // loop) so that a `MACRO` definition can reach into
                // the ones that follow for its body.
                let prev = self.iter.replace(lines.into_iter());
                while let Some(line) = self.iter.as_mut().unwrap().next() {
                    self.parse_line(line);
                }
                self.iter = prev;
                self.stack.pop();
            }
            Err(e) => self.diags.push(Diagnostic {
                file: self
                    .stack
                    .len()
                    .saturating_sub(1)
                    .min(self.files.len().saturating_sub(1)),
                line: 1,
                col: 1,
                message: format!("cannot read {path}: {e}"),
            }),
        }
    }

    /// Error at `span`, and swallow the rest of the line.
    fn error(&mut self, span: Span, message: impl Into<String>) {
        self.diags.push(Diagnostic::error(span, message));
    }

    fn parse_line(&mut self, line: LexLine) {
        let LexLine {
            file,
            line: lineno,
            text,
            tokens,
            ..
        } = line;
        let mut t = Tokens {
            tokens: &tokens,
            pos: 0,
            file,
            line: lineno,
        };

        // --- the label ---
        let mut label = None;
        let first_word = match t.peek_tok() {
            Some(Tok::Ident(name)) => Some(name.clone()),
            _ => None,
        };
        if let Some(name) = first_word {
            let span = t.peek_span();
            let next_is_colon = matches!(t.peek_at(1), Some(Tok::Colon));
            let next_is_equ =
                matches!(&t.peek_at(1), Some(Tok::Ident(w)) if w.eq_ignore_ascii_case("EQU"));
            let is_opcode = lookup_mnemonic(&name).is_some() || is_directive(&name);
            if next_is_colon {
                label = Some((name.clone(), span));
                t.advance();
                t.advance();
                // `name: EQU expr` — the same as `name EQU expr`.
                if matches!(&t.peek_tok(), Some(Tok::Ident(w)) if w.eq_ignore_ascii_case("EQU")) {
                    t.advance();
                    let expr = match t.parse_expr(&mut self.diags) {
                        Some(e) => e,
                        None => return,
                    };
                    t.expect_end(&mut self.diags);
                    self.lines.push(Line {
                        label: Some((name.clone(), span)),
                        stmt: Stmt::Equ(expr),
                        span,
                        text,
                    });
                    return;
                }
            } else if next_is_equ && !is_opcode {
                // `name EQU expr`
                t.advance();
                t.advance();
                let expr = match t.parse_expr(&mut self.diags) {
                    Some(e) => e,
                    None => return,
                };
                t.expect_end(&mut self.diags);
                self.lines.push(Line {
                    label: Some((name.clone(), span)),
                    stmt: Stmt::Equ(expr),
                    span,
                    text,
                });
                return;
            } else if !is_opcode {
                if self.find_macro(&name).is_some() {
                    // A macro invocation: the word is the opcode, not
                    // a label (`name:` still defines a label).
                } else {
                    // A bare identifier: label when followed by
                    // another opcode or nothing; a typo'd opcode
                    // otherwise.
                    let next_ok = match t.peek_at(1) {
                        None => true,
                        Some(Tok::Ident(w)) => {
                            lookup_mnemonic(w).is_some()
                                || is_directive(w)
                                || self.find_macro(w).is_some()
                        }
                        _ => false,
                    };
                    if next_ok {
                        label = Some((name.clone(), span));
                        t.advance();
                    } else {
                        self.error(span, format!("unknown instruction {name:?}"));
                        return;
                    }
                }
            }
        }

        // --- the statement ---
        let span = t.peek_span();
        let stmt = if t.peek_tok().is_none() {
            if label.is_some() {
                Some(Stmt::Empty)
            } else {
                return; // unreachable: lex_lines drops empty lines
            }
        } else if let Some(Tok::Ident(word)) = t.peek_tok().cloned() {
            t.advance();
            if let Some(kind) = lookup_mnemonic(&word) {
                self.parse_instr(kind, &mut t, span)
            } else if let Some(m) = self.find_macro(&word) {
                self.invoke_macro(m, &mut t, span);
                None
            } else {
                match word.to_ascii_uppercase().as_str() {
                    "ORG" => self.finish_expr(&mut t, span).map(Stmt::Org),
                    "ALIGN" => self.finish_expr(&mut t, span).map(Stmt::Align),
                    "END" => match t.peek_tok() {
                        None => Some(Stmt::End(None)),
                        Some(_) => self.finish_expr(&mut t, span).map(|e| Stmt::End(Some(e))),
                    },
                    "DB" | "BYTE" | "DEFB" => self.parse_db(&mut t, span).map(Stmt::Db),
                    "DW" | "WORD" | "DEFW" => {
                        let items = self.parse_expr_list(&mut t, span);
                        Some(Stmt::Dw(items))
                    }
                    "DS" | "DEFS" | "RESB" => {
                        let size = match t.parse_expr(&mut self.diags) {
                            Some(e) => e,
                            None => return,
                        };
                        let fill = if matches!(t.peek_tok(), Some(Tok::Comma)) {
                            t.advance();
                            t.parse_expr(&mut self.diags)
                        } else {
                            None
                        };
                        t.expect_end(&mut self.diags);
                        Some(Stmt::Ds { size, fill })
                    }
                    "INCBIN" => match t.peek_tok().cloned() {
                        Some(Tok::Str(path)) => {
                            t.advance();
                            let mut offset = None;
                            let mut length = None;
                            if matches!(t.peek_tok(), Some(Tok::Comma)) {
                                t.advance();
                                offset = t.parse_expr(&mut self.diags);
                                if matches!(t.peek_tok(), Some(Tok::Comma)) {
                                    t.advance();
                                    length = t.parse_expr(&mut self.diags);
                                }
                            }
                            t.expect_end(&mut self.diags);
                            Some(Stmt::IncBin {
                                path: String::from_utf8_lossy(&path).into_owned(),
                                offset,
                                length,
                            })
                        }
                        _ => {
                            self.error(span, "INCBIN needs a quoted file name");
                            return;
                        }
                    },
                    "INCLUDE" => match t.peek_tok().cloned() {
                        Some(Tok::Str(path)) => {
                            t.advance();
                            t.expect_end(&mut self.diags);
                            let path = String::from_utf8_lossy(&path).into_owned();
                            if self.stack.contains(&path) {
                                self.error(span, format!("INCLUDE cycle on {path:?}"));
                            } else {
                                self.parse_file(&path);
                            }
                            None
                        }
                        _ => {
                            self.error(span, "INCLUDE needs a quoted file name");
                            return;
                        }
                    },
                    "EQU" => {
                        self.error(span, "EQU needs a name before it");
                        return;
                    }
                    "MACRO" => {
                        self.define_macro(label.clone(), &mut t, span);
                        None
                    }
                    "ENDM" => {
                        self.error(span, "ENDM without a MACRO definition");
                        None
                    }
                    "LOCAL" => {
                        self.error(span, "LOCAL is only valid inside a macro body");
                        None
                    }
                    _ => {
                        self.error(span, format!("unknown instruction {word:?}"));
                        return;
                    }
                }
            }
        } else {
            self.error(span, "expected an instruction or directive");
            return;
        };

        if let Some(stmt) = stmt {
            self.lines.push(Line {
                label,
                stmt,
                span,
                text,
            });
        }
    }

    /// Parse an instruction's operands per the mnemonic's pattern.
    fn parse_instr(&mut self, kind: InstrKind, t: &mut Tokens, _span: Span) -> Option<Stmt> {
        let mut ops = Vec::new();
        for (i, slot) in pattern(kind).iter().enumerate() {
            if i > 0 {
                if !matches!(t.peek_tok(), Some(Tok::Comma)) {
                    self.error(
                        t.peek_span(),
                        format!("expected a comma (operand {})", i + 1),
                    );
                    return None;
                }
                t.advance();
            }
            match slot {
                Slot::Reg => match t.peek_tok().cloned() {
                    Some(Tok::Ident(w)) if parse_reg(&w).is_some() => {
                        t.advance();
                        ops.push(Op::Reg(parse_reg(&w).unwrap()));
                    }
                    _ => {
                        self.error(t.peek_span(), "expected a register (A B C D E H L M)");
                        return None;
                    }
                },
                Slot::Pair => match t.peek_tok().cloned() {
                    Some(Tok::Ident(w)) if parse_pair(&w, false).is_some() => {
                        t.advance();
                        ops.push(Op::Pair(parse_pair(&w, false).unwrap()));
                    }
                    _ => {
                        self.error(t.peek_span(), "expected B, D, H or SP");
                        return None;
                    }
                },
                Slot::PairPsw => match t.peek_tok().cloned() {
                    Some(Tok::Ident(w)) if parse_pair(&w, true).is_some() => {
                        t.advance();
                        ops.push(Op::Pair(parse_pair(&w, true).unwrap()));
                    }
                    _ => {
                        self.error(t.peek_span(), "expected B, D, H, SP or PSW");
                        return None;
                    }
                },
                Slot::Expr => {
                    let e = t.parse_expr(&mut self.diags)?;
                    ops.push(Op::Expr(e))
                }
            }
        }
        t.expect_end(&mut self.diags);
        Some(Stmt::Instr { kind, ops })
    }

    /// `DB` items: values or strings, comma-separated.
    fn parse_db(&mut self, t: &mut Tokens, span: Span) -> Option<Vec<DbItem>> {
        let mut items = Vec::new();
        loop {
            match t.peek_tok().cloned() {
                Some(Tok::Str(bytes)) => {
                    t.advance();
                    items.push(DbItem::Str(bytes));
                }
                _ => {
                    let e = t.parse_expr(&mut self.diags)?;
                    items.push(DbItem::Expr(e))
                }
            }
            if matches!(t.peek_tok(), Some(Tok::Comma)) {
                t.advance();
                // A trailing comma is fine.
                if t.peek_tok().is_none() {
                    break;
                }
            } else {
                break;
            }
        }
        t.expect_end(&mut self.diags);
        let _ = span;
        Some(items)
    }

    /// A comma-separated expression list.
    fn parse_expr_list(&mut self, t: &mut Tokens, _span: Span) -> Vec<Expr> {
        let mut items = Vec::new();
        loop {
            match t.parse_expr(&mut self.diags) {
                Some(e) => items.push(e),
                None => return items,
            }
            if matches!(t.peek_tok(), Some(Tok::Comma)) {
                t.advance();
                if t.peek_tok().is_none() {
                    break;
                }
            } else {
                break;
            }
        }
        t.expect_end(&mut self.diags);
        items
    }

    /// One expression, then end of line.
    /// Find a macro by name (case-insensitive); returns its index.
    fn find_macro(&self, word: &str) -> Option<usize> {
        let w = word.to_ascii_uppercase();
        self.macros.iter().position(|m| m.name == w)
    }

    /// `name MACRO p1, p2, ...` — parse the parameters, then pull the
    /// body lines from the current file until the matching `ENDM`.
    fn define_macro(&mut self, label: Option<(String, Span)>, t: &mut Tokens, span: Span) {
        let Some((name, _)) = label else {
            self.error(span, "MACRO needs a name before it");
            return;
        };
        if self.macro_depth > 0 {
            self.error(span, "nested MACRO definitions are not supported");
            return;
        }
        if self.find_macro(&name).is_some() {
            self.error(span, format!("macro {name:?} is already defined"));
            return;
        }
        // Parameters: comma-separated plain identifiers.
        let mut params = Vec::new();
        let mut expect_param = true;
        loop {
            match t.peek_tok() {
                Some(Tok::Ident(p)) if expect_param => {
                    params.push(p.to_ascii_uppercase());
                    t.advance();
                    expect_param = false;
                    if matches!(t.peek_tok(), Some(Tok::Comma)) {
                        t.advance();
                        expect_param = true;
                    }
                }
                None => break,
                _ => {
                    t.expect_end(&mut self.diags);
                    break;
                }
            }
        }
        // The body: every line until a lone ENDM. Nested definitions
        // would corrupt the END matching, so they are rejected here.
        let mut body = Vec::new();
        loop {
            let Some(next) = self.iter.as_mut().and_then(Iterator::next) else {
                self.diags.push(Diagnostic::error(
                    span,
                    format!("macro {name:?} has no matching ENDM"),
                ));
                return;
            };
            let first = next.tokens.first().map(|t| &t.tok);
            match first {
                Some(Tok::Ident(w)) if w.eq_ignore_ascii_case("ENDM") => {
                    if next.tokens.len() > 1 {
                        self.diags.push(Diagnostic::error(
                            next.tokens[1].span,
                            "ENDM takes no operands",
                        ));
                    }
                    break;
                }
                _ if next.tokens.iter().any(
                    |t| matches!(&t.tok, Tok::Ident(w) if w.eq_ignore_ascii_case("MACRO")),
                ) =>
                {
                    // `MACRO` in a body — labeled or not — would
                    // swallow the ENDM matching.
                    self.diags.push(Diagnostic::error(
                        next.tokens[0].span,
                        "nested MACRO definitions are not supported",
                    ));
                    return;
                }
                _ => body.push(next),
            }
        }
        self.macros.push(MacroDef {
            name: name.to_ascii_uppercase(),
            params,
            body,
        });
    }

    /// Invoke macro `index`: split the rest of the line into
    /// comma-separated arguments, then parse the body lines with the
    /// parameters (and `LOCAL` labels, renamed per invocation)
    /// substituted token-wise. Diagnostics inside an expansion point
    /// at the macro body; substituted operands keep the invocation
    /// site's spans.
    fn invoke_macro(&mut self, index: usize, t: &mut Tokens, span: Span) {
        if self.macro_depth >= MACRO_DEPTH_MAX {
            self.error(
                span,
                format!("macro expansion nested deeper than {MACRO_DEPTH_MAX} levels"),
            );
            return;
        }
        let def = self.macros[index].clone();
        // Arguments: the remaining tokens split on top-level commas
        // (parenthesized commas stay inside their argument).
        let mut args: Vec<Vec<Token>> = vec![Vec::new()];
        let mut depth = 0;
        while let Some(token) = t.tokens.get(t.pos).cloned() {
            t.advance();
            match token.tok {
                Tok::Comma if depth == 0 => args.push(Vec::new()),
                Tok::LParen => {
                    depth += 1;
                    args.last_mut().unwrap().push(token);
                }
                Tok::RParen if depth > 0 => {
                    depth -= 1;
                    args.last_mut().unwrap().push(token);
                }
                _ => args.last_mut().unwrap().push(token),
            }
        }
        if args.len() > def.params.len() {
            self.error(
                span,
                format!(
                    "macro {:?} takes {} argument(s), got {}",
                    def.name,
                    def.params.len(),
                    args.len()
                ),
            );
            return;
        }
        while args.len() < def.params.len() {
            args.push(Vec::new());
        }
        // `LOCAL name, ...` declarations: every occurrence of the
        // name in the body is replaced by a per-invocation unique
        // symbol (built with a `!` the lexer cannot produce, so it
        // can never collide with a user label).
        self.local_seq += 1;
        let seq = self.local_seq;
        let mut locals: Vec<(String, String)> = Vec::new();
        for line in &def.body {
            if is_local_decl(line) {
                for tok in &line.tokens[1..] {
                    if let Tok::Ident(w) = &tok.tok {
                        if !w.eq_ignore_ascii_case("LOCAL")
                            && !locals.iter().any(|(from, _)| from.eq_ignore_ascii_case(w))
                        {
                            locals.push((w.to_ascii_uppercase(), format!("M!{seq}{w}")));
                        }
                    }
                }
            }
        }
        self.macro_depth += 1;
        for body_line in &def.body {
            if is_local_decl(body_line) {
                continue;
            }
            let tokens = body_line
                .tokens
                .iter()
                .flat_map(|token| match &token.tok {
                    Tok::Ident(w) => {
                        let upper = w.to_ascii_uppercase();
                        if let Some(i) = def.params.iter().position(|p| *p == upper) {
                            args[i].clone()
                        } else if let Some((_, to)) = locals.iter().find(|(from, _)| *from == upper)
                        {
                            vec![Token {
                                tok: Tok::Ident(to.clone()),
                                span: token.span,
                                len: token.len,
                            }]
                        } else {
                            vec![token.clone()]
                        }
                    }
                    _ => vec![token.clone()],
                })
                .collect();
            self.parse_line(LexLine {
                file: body_line.file,
                line: body_line.line,
                text: body_line.text.clone(),
                tokens,
                comment: body_line.comment,
            });
        }
        self.macro_depth -= 1;
    }

    fn finish_expr(&mut self, t: &mut Tokens, _span: Span) -> Option<Expr> {
        let e = t.parse_expr(&mut self.diags)?;
        t.expect_end(&mut self.diags);
        Some(e)
    }
}

/// Whether `word` (any case) is a directive name.
pub fn is_directive(word: &str) -> bool {
    matches!(
        word.to_ascii_uppercase().as_str(),
        "ORG"
            | "EQU"
            | "DB"
            | "BYTE"
            | "DEFB"
            | "DW"
            | "WORD"
            | "DEFW"
            | "DS"
            | "DEFS"
            | "RESB"
            | "INCBIN"
            | "ALIGN"
            | "END"
            | "INCLUDE"
            | "MACRO"
            | "ENDM"
            | "LOCAL"
    )
}

/// Whether a lexed line is a `LOCAL name, ...` declaration (label-less,
/// as captured inside a macro body).
fn is_local_decl(line: &LexLine) -> bool {
    matches!(
        line.tokens.first().map(|t| &t.tok),
        Some(Tok::Ident(w)) if w.eq_ignore_ascii_case("LOCAL")
    )
}

fn parse_reg(word: &str) -> Option<Reg> {
    Some(match word.to_ascii_uppercase().as_str() {
        "A" => Reg::A,
        "B" => Reg::B,
        "C" => Reg::C,
        "D" => Reg::D,
        "E" => Reg::E,
        "H" => Reg::H,
        "L" => Reg::L,
        "M" => Reg::M,
        _ => return None,
    })
}

fn parse_pair(word: &str, psw: bool) -> Option<Pair> {
    Some(match word.to_ascii_uppercase().as_str() {
        "B" => Pair::B,
        "D" => Pair::D,
        "H" => Pair::H,
        "SP" => Pair::Sp,
        "PSW" if psw => Pair::Psw,
        _ => return None,
    })
}

/// Token cursor over one line, plus the Pratt expression parser.
struct Tokens<'t> {
    tokens: &'t [Token],
    pos: usize,
    file: usize,
    line: usize,
}

impl<'t> Tokens<'t> {
    fn peek_tok(&self) -> Option<&Tok> {
        self.tokens.get(self.pos).map(|t| &t.tok)
    }

    fn peek_at(&self, n: usize) -> Option<&Tok> {
        self.tokens.get(self.pos + n).map(|t| &t.tok)
    }

    fn peek_span(&self) -> Span {
        match self.tokens.get(self.pos) {
            Some(t) => t.span,
            None => Span::new(self.file, self.line, 1),
        }
    }

    fn advance(&mut self) {
        self.pos += 1;
    }

    fn expect_end(&mut self, diags: &mut Vec<Diagnostic>) {
        if self.peek_tok().is_some() {
            diags.push(Diagnostic::error(
                self.peek_span(),
                format!("unexpected {:?} after the statement", self.peek_tok()),
            ));
        }
    }

    /// Parse one expression (Pratt). `None` after reporting.
    fn parse_expr(&mut self, diags: &mut Vec<Diagnostic>) -> Option<Expr> {
        self.parse_expr_bp(0, diags)
    }

    fn parse_expr_bp(&mut self, min_bp: u8, diags: &mut Vec<Diagnostic>) -> Option<Expr> {
        let span = self.peek_span();
        let mut lhs = match self.peek_tok() {
            Some(Tok::Number(v)) => {
                let v = *v;
                self.advance();
                Expr {
                    kind: ExprKind::Num(v),
                    span,
                }
            }
            Some(Tok::Dollar) => {
                self.advance();
                Expr {
                    kind: ExprKind::Pc,
                    span,
                }
            }
            Some(Tok::Ident(name)) => {
                let name = name.clone();
                self.advance();
                Expr {
                    kind: ExprKind::Sym(name),
                    span,
                }
            }
            Some(Tok::Minus) => {
                self.advance();
                let inner = self.parse_expr_bp(9, diags)?;
                Expr {
                    kind: ExprKind::Neg(Box::new(inner)),
                    span,
                }
            }
            Some(Tok::Tilde) => {
                self.advance();
                let inner = self.parse_expr_bp(9, diags)?;
                Expr {
                    kind: ExprKind::Not(Box::new(inner)),
                    span,
                }
            }
            Some(Tok::LParen) => {
                self.advance();
                let inner = self.parse_expr_bp(0, diags)?;
                if !matches!(self.peek_tok(), Some(Tok::RParen)) {
                    diags.push(Diagnostic::error(self.peek_span(), "expected )"));
                    return None;
                }
                self.advance();
                inner
            }
            other => {
                diags.push(Diagnostic::error(
                    span,
                    format!("expected a value, found {other:?}"),
                ));
                return None;
            }
        };
        while let Some(op) = peek_binop(self.peek_tok()) {
            let bp = binop_bp(op);
            if bp < min_bp {
                break;
            }
            self.advance();
            let rhs = self.parse_expr_bp(bp + 1, diags)?;
            lhs = Expr {
                kind: ExprKind::Bin(op, Box::new(lhs), Box::new(rhs)),
                span,
            };
        }
        Some(lhs)
    }
}

fn peek_binop(tok: Option<&Tok>) -> Option<BinOp> {
    Some(match tok? {
        Tok::Plus => BinOp::Add,
        Tok::Minus => BinOp::Sub,
        Tok::Star => BinOp::Mul,
        Tok::Slash => BinOp::Div,
        Tok::Percent => BinOp::Mod,
        Tok::Amp => BinOp::And,
        Tok::Pipe => BinOp::Or,
        Tok::Caret => BinOp::Xor,
        Tok::Shl => BinOp::Shl,
        Tok::Shr => BinOp::Shr,
        _ => return None,
    })
}

/// Binding powers: higher binds tighter.
fn binop_bp(op: BinOp) -> u8 {
    match op {
        BinOp::Mul | BinOp::Div | BinOp::Mod => 7,
        BinOp::Add | BinOp::Sub => 6,
        BinOp::Shl | BinOp::Shr => 5,
        BinOp::And => 4,
        BinOp::Xor => 3,
        BinOp::Or => 2,
    }
}
