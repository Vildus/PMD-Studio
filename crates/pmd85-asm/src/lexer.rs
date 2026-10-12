//! The lexer: source text to per-line token lists.
//!
//! The line is the statement unit (classic assembler style): blank
//! and comment-only lines are dropped, comments run from `;` or `//`
//! to the end of the line. Identifiers may contain letters, digits,
//! `_ . ? @` and must not start with a digit; an identifier starting
//! with `.` is a local label (scoped to the enclosing global label).
//!
//! Numbers: `123` decimal, `0x1F`, `1FH` hex, `1010B` binary. Char
//! literals `'A'` (with `\n \r \t \0 \\ \'` escapes) are byte values
//! and may appear anywhere an expression can. Strings `"..."` (same
//! escapes) appear in `DB` and `INCBIN`/`INCLUDE` paths.

use crate::diags::{Diagnostic, Span};

/// One token.
#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    Number(i32),
    Str(Vec<u8>),
    Comma,
    Colon,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    LParen,
    RParen,
    Amp,
    Pipe,
    Caret,
    Tilde,
    Shl,
    Shr,
    Dollar,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
    /// The token's length in characters (for the editor's
    /// highlighter, which colors from this token stream).
    pub len: usize,
}

/// One source line: its tokens (never empty) and the trimmed text
/// (for the listing). A line with only a comment carries no tokens
/// but is kept (with `comment` set) — the editor's highlighter colors
/// from the token stream and needs every line.
#[derive(Clone, Debug)]
pub struct LexLine {
    pub file: usize,
    pub line: usize,
    pub text: String,
    pub tokens: Vec<Token>,
    /// Where a `;`/`//` comment starts on this line: the 1-based
    /// character column; the comment runs to the end of the line.
    pub comment: Option<usize>,
}

/// Lex `text` (file index `file`) into lines. Bad tokens produce
/// diagnostics and are skipped; lexing always yields a line list.
pub fn lex_lines(file: usize, text: &str, diags: &mut Vec<Diagnostic>) -> Vec<LexLine> {
    let mut out = Vec::new();
    for (lineno, raw) in text.lines().enumerate() {
        let lineno = lineno + 1;
        let mut chars = raw.char_indices().peekable();
        let mut tokens = Vec::new();
        let mut comment = None;
        let col = |byte: usize| raw[..byte].chars().count() + 1;
        let push = |tok: Tok, byte: usize, tokens: &mut Vec<Token>| {
            tokens.push(Token {
                tok,
                span: Span::new(file, lineno, col(byte)),
                len: 0, // patched below, once the token is consumed
            });
        };
        while let Some(&(byte, ch)) = chars.peek() {
            let pushed_before = tokens.len();
            match ch {
                ' ' | '\t' | '\r' => {
                    chars.next();
                }
                ';' => {
                    comment = Some(col(byte));
                    break;
                }
                '/' if raw[byte + 1..].starts_with('/') => {
                    comment = Some(col(byte));
                    break;
                }
                ',' => {
                    chars.next();
                    push(Tok::Comma, byte, &mut tokens);
                }
                ':' => {
                    chars.next();
                    push(Tok::Colon, byte, &mut tokens);
                }
                '+' => {
                    chars.next();
                    push(Tok::Plus, byte, &mut tokens);
                }
                '-' => {
                    chars.next();
                    push(Tok::Minus, byte, &mut tokens);
                }
                '*' => {
                    chars.next();
                    push(Tok::Star, byte, &mut tokens);
                }
                '/' => {
                    chars.next();
                    push(Tok::Slash, byte, &mut tokens);
                }
                '%' => {
                    chars.next();
                    push(Tok::Percent, byte, &mut tokens);
                }
                '(' => {
                    chars.next();
                    push(Tok::LParen, byte, &mut tokens);
                }
                ')' => {
                    chars.next();
                    push(Tok::RParen, byte, &mut tokens);
                }
                '&' => {
                    chars.next();
                    push(Tok::Amp, byte, &mut tokens);
                }
                '|' => {
                    chars.next();
                    push(Tok::Pipe, byte, &mut tokens);
                }
                '^' => {
                    chars.next();
                    push(Tok::Caret, byte, &mut tokens);
                }
                '~' => {
                    chars.next();
                    push(Tok::Tilde, byte, &mut tokens);
                }
                '$' => {
                    chars.next();
                    push(Tok::Dollar, byte, &mut tokens);
                }
                '<' if raw[byte + 1..].starts_with('<') => {
                    chars.next();
                    chars.next();
                    push(Tok::Shl, byte, &mut tokens);
                }
                '>' if raw[byte + 1..].starts_with('>') => {
                    chars.next();
                    chars.next();
                    push(Tok::Shr, byte, &mut tokens);
                }
                '"' => {
                    chars.next();
                    match lex_string(&mut chars, raw, byte) {
                        Ok(bytes) => push(Tok::Str(bytes), byte, &mut tokens),
                        Err(msg) => {
                            diags.push(Diagnostic::error(Span::new(file, lineno, col(byte)), msg))
                        }
                    }
                }
                '\'' => {
                    chars.next();
                    match lex_char(&mut chars) {
                        Ok(v) => push(Tok::Number(v as i32), byte, &mut tokens),
                        Err(msg) => {
                            diags.push(Diagnostic::error(Span::new(file, lineno, col(byte)), msg))
                        }
                    }
                }
                _ if ch.is_ascii_digit() => {
                    let word = take_word(&mut chars, raw, byte);
                    match lex_number(&word) {
                        Ok(v) => push(Tok::Number(v), byte, &mut tokens),
                        Err(msg) => {
                            diags.push(Diagnostic::error(Span::new(file, lineno, col(byte)), msg))
                        }
                    }
                }
                _ if ch.is_ascii_alphabetic()
                    || ch == '_'
                    || ch == '.'
                    || ch == '?'
                    || ch == '@' =>
                {
                    let word = take_word(&mut chars, raw, byte);
                    push(Tok::Ident(word), byte, &mut tokens);
                }
                _ => {
                    chars.next();
                    diags.push(Diagnostic::error(
                        Span::new(file, lineno, col(byte)),
                        format!("unexpected character {ch:?}"),
                    ));
                }
            }
            // A token was pushed: fill in its length from wherever
            // the consuming arm left the cursor.
            if tokens.len() > pushed_before {
                let end = chars.peek().map_or(raw.len(), |&(i, _)| i);
                let start_col = tokens[pushed_before].span.col;
                tokens[pushed_before].len = col(end) - start_col;
            }
        }
        if !tokens.is_empty() || comment.is_some() {
            out.push(LexLine {
                file,
                line: lineno,
                text: raw.trim().to_string(),
                tokens,
                comment,
            });
        }
    }
    out
}

/// Consume an identifier/number word starting at `byte`.
fn take_word(
    chars: &mut std::iter::Peekable<std::str::CharIndices>,
    raw: &str,
    byte: usize,
) -> String {
    let mut end = byte;
    while let Some(&(i, ch)) = chars.peek() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' || ch == '?' || ch == '@' {
            end = i + ch.len_utf8();
            chars.next();
        } else {
            break;
        }
    }
    raw[byte..end].to_string()
}

/// A number word: decimal, `0x`/`H` hex, `B` binary.
fn lex_number(word: &str) -> Result<i32, String> {
    let (digits, radix, kind) =
        if let Some(rest) = word.strip_prefix("0x").or_else(|| word.strip_prefix("0X")) {
            (rest, 16, "hex")
        } else if let Some(rest) = word.strip_prefix("0b").or_else(|| word.strip_prefix("0B")) {
            (rest, 2, "binary")
        } else if word.len() > 1
            && (word.ends_with('H') || word.ends_with('h'))
            && word[..word.len() - 1]
                .chars()
                .all(|c| c.is_ascii_hexdigit())
        {
            (&word[..word.len() - 1], 16, "hex")
        } else if word.len() > 1 && (word.ends_with('B') || word.ends_with('b')) {
            (&word[..word.len() - 1], 2, "binary")
        } else {
            (word, 10, "decimal")
        };
    let mut value: i64 = 0;
    for c in digits.chars() {
        let d = c
            .to_digit(radix)
            .ok_or_else(|| format!("invalid {kind} digit {c:?} in {word}"))?;
        value = value * radix as i64 + d as i64;
        if value > i32::MAX as i64 {
            return Err(format!("number {word} overflows"));
        }
    }
    Ok(value as i32)
}

/// A char literal after the opening quote.
fn lex_char(chars: &mut std::iter::Peekable<std::str::CharIndices>) -> Result<u8, String> {
    let (_, first) = chars.next().ok_or("unterminated character literal")?;
    let value = if first == '\\' {
        let (_, esc) = chars.next().ok_or("unterminated character literal")?;
        match esc {
            'n' => b'\n',
            'r' => b'\r',
            't' => b'\t',
            '0' => 0,
            '\\' => b'\\',
            '\'' => b'\'',
            other => return Err(format!("unknown escape \\{other}")),
        }
    } else if first.is_ascii() {
        first as u8
    } else {
        return Err("character literal must be ASCII".to_string());
    };
    let (_, close) = chars.next().ok_or("unterminated character literal")?;
    if close != '\'' {
        return Err("unterminated character literal".to_string());
    }
    Ok(value)
}

/// A quoted string after the opening quote. Consumes up to (not
/// including) the closing quote.
fn lex_string(
    chars: &mut std::iter::Peekable<std::str::CharIndices>,
    _raw: &str,
    _byte: usize,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    loop {
        let (_, ch) = chars.next().ok_or("unterminated string")?;
        match ch {
            '"' => return Ok(bytes),
            '\\' => {
                let (_, esc) = chars.next().ok_or("unterminated string")?;
                match esc {
                    'n' => bytes.push(b'\n'),
                    'r' => bytes.push(b'\r'),
                    't' => bytes.push(b'\t'),
                    '0' => bytes.push(0),
                    '\\' => bytes.push(b'\\'),
                    '"' => bytes.push(b'"'),
                    other => return Err(format!("unknown escape \\{other}")),
                }
            }
            ch if ch.is_ascii() => bytes.push(ch as u8),
            _ => return Err("strings must be ASCII".to_string()),
        }
    }
}
