//! `pmd85-asm`: an Intel 8080 assembler for the PMD 85 IDE.
//!
//! Our own dialect, lenient in the ways the Monitor's listings and
//! hand-written sources want: case-insensitive mnemonics and
//! symbols, labels with or without a colon, `DB`/`DEFB`/`BYTE`-style
//! directive aliases, decimal numbers by default with `0x`/`H`/`B`
//! forms, `$` for the current address, local labels (`.loop`) scoped
//! to the enclosing global label, `INCLUDE`/`INCBIN` for splicing
//! files in, and `MACRO`/`ENDM` definitions (with `LOCAL` labels
//! renamed per invocation) invoked by name.
//!
//! The lexer is public — the editor's syntax highlighting runs on the
//! same tokens the assembler sees, so the two can never disagree.
//!
//! Two passes: addresses and labels first (sizes never depend on
//! operand values), then encoding. `EQU`s resolve to a fixpoint
//! afterwards, in any order. First-pass expressions (`ORG`, `DS`,
//! `ALIGN`, `INCBIN` sizes) must not use forward references.
//!
//! The output carries everything the IDE needs: the image as
//! address-tagged segments, the symbol table, a listing with
//! line-to-address mapping, and spanned diagnostics.

mod asm;
mod ast;
mod diags;
mod encode;
mod lexer;
mod parse;

pub use asm::{assemble, AddrLine, AsmOutput, ListingLine, Segment};
pub use diags::{Diagnostic, Span};
pub use encode::{lookup_mnemonic, Slot};
pub use lexer::{lex_lines, LexLine, Tok, Token};
pub use parse::is_directive;

/// Where a project's files come from: the app reads the project
/// directory, tests use a map. Paths are as written in `INCLUDE` /
/// `INCBIN`.
pub trait SourceResolver {
    fn read_text(&mut self, path: &str) -> Result<String, String>;
    fn read_bytes(&mut self, path: &str) -> Result<Vec<u8>, String>;
}
