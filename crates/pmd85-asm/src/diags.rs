//! Assembler diagnostics and source spans.

/// Where a token or statement sits: index into the assembler's file
/// table (see [`crate::AsmOutput::files`]), 1-based line and column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub file: usize,
    pub line: usize,
    pub col: usize,
}

impl Span {
    pub fn new(file: usize, line: usize, col: usize) -> Self {
        Span { file, line, col }
    }
}

/// One problem found while assembling. Errors stop emission for the
/// offending line (its bytes become zeros of the right size, so the
/// rest of the program stays put); warnings never do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub file: usize,
    pub line: usize,
    pub col: usize,
    pub message: String,
}

impl Diagnostic {
    pub fn error(span: Span, message: impl Into<String>) -> Self {
        Diagnostic {
            file: span.file,
            line: span.line,
            col: span.col,
            message: message.into(),
        }
    }
}
