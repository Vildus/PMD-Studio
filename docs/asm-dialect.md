# The PMD Studio assembler dialect

PMD Studio ships its own Intel 8080 assembler (`pmd85-asm`, in
[`crates/pmd85-asm`](../crates/pmd85-asm)). The dialect is
deliberately lenient in the ways the Monitor listings and
hand-written sources want: case-insensitive everywhere, labels with
or without a colon, directive aliases, local labels, macros. The
editor's syntax highlighting runs on the assembler's own lexer, so
what looks right is what assembles right.

## Source model

The line is the statement unit, classic assembler style:

```text
[label[:]] [opcode|directive [operands]] [; comment]
```

- Comments run from `;` (or `//`) to the end of the line; blank and
  comment-only lines are free.
- Everything is case-insensitive: mnemonics, directives, symbols,
  macro names.
- A label may carry a trailing colon or not. A bare identifier is
  taken as a label when the next word is an opcode or the line ends;
  `name EQU expr` defines a constant instead.
- Identifiers may contain letters, digits, `_ . ? @` and must not
  start with a digit. An identifier starting with `.` is a **local
  label**, scoped to the enclosing global label — the next `.loop`
  under a different global label is a different symbol:

  ```asm
  BLIT:   MVI B,22H       ; global label
  .ROW:   MVI C,3         ; local to BLIT
          DCR C
          JNZ .ROW
          ...
  DRAW:   ...
  .ROW:   ...             ; a fresh .ROW, local to DRAW
  ```

## Numbers and strings

- `123` — decimal
- `0x1F`, `1FH` — hexadecimal
- `1010B` — binary
- `'A'` — a character literal, usable anywhere an expression can;
  escapes `\n \r \t \0 \\ \'`
- `"..."` — a string (same escapes), in `DB` data and in
  `INCLUDE`/`INCBIN` paths

## Expressions

Expressions are signed 32-bit integer arithmetic; the use site range
checks the result. Unary `-` and `~`, parentheses, and the binary
operators below, with this precedence (tightest first):

| Class | Operators |
|---|---|
| unary | `-` `~` |
| multiplicative | `*` `/` `%` |
| additive | `+` `-` |
| shift | `<<` `>>` |
| bitwise | `&` then `^` then `|` |

`$` evaluates to the address of the current instruction — the
classic way to jump over data:

```asm
        JMP $+9          ; skip this 3-byte jump plus the next 6 bytes
        DB 1,2,3,4,5,6
BACK:   HLT
```

## Directives

| Directive | Meaning |
|---|---|
| `ORG expr` | Assemble the following code at `expr`. |
| `name EQU expr` | Define a constant. `EQU`s resolve to a fixpoint in any order, so they may reference each other freely. |
| `END [expr]` | End of source; the operand is the entry point (see [targets](#projects-and-targets)). |
| `DB` / `BYTE` / `DEFB` | Comma-separated items: values or strings, one byte each. |
| `DW` / `WORD` / `DEFW` | Comma-separated words, little-endian. |
| `DS` / `DEFS` / `RESB` | `DS size[, fill]` — reserve `size` bytes, default fill 0. |
| `ALIGN expr` | Pad with zeros to the next multiple of `expr` (a power of two). |
| `INCBIN "path"` | `INCBIN "path"[, offset[, length]]` — splice raw bytes from a file. |
| `INCLUDE "path"` | Splice another source file at parse time (cycles are reported). |
| `name MACRO ...` / `ENDM` / `LOCAL` | See [macros](#macros) below. |

## Macros

```asm
; Delay N times through the Monitor 2/2A delay routine (about a
; frame per call; a Monitor 3 in 85-2 compatibility mode works the
; same).
DELAY:  EQU 8BCBH        ; wait about A/256 + D ticks

WAITN   MACRO N
        LOCAL LOOP
        MVI B,N          ; token-wise: the N argument lands here
LOOP:   MVI A,0
        MVI D,0FH
        CALL DELAY
        DCR B
        JNZ LOOP
        ENDM
```

Invoking `WAITN 5` splices the body in with `N` replaced by `5`,
and its `LOOP` renamed to something unique — so a second `WAITN 3`
further down cannot collide with it.

- `name MACRO p1, p2, ...` captures the following lines up to
  `ENDM` as a body; `name arg1, arg2, ...` invokes it.
- Parameters are substituted **token-wise**: a parameter name is
  replaced by the invocation argument wherever it appears as a whole
  token.
- `LOCAL name, ...` inside a body declares labels that are renamed
  to a unique symbol on every invocation, so repeated expansions do
  not collide.
- Definitions must precede uses (as with `INCLUDE`); nesting a
  `MACRO` definition inside a body is rejected — but bodies may
  invoke other macros, up to a fixed depth.

## The two passes

Pass 1 assigns addresses and defines labels — instruction sizes
never depend on operand values, since the 8080 has no PC-relative
operands — then `EQU`s resolve iteratively, in any order. Pass 2
encodes, building the image as address-tagged segments plus a
listing with a line-to-address map.

The classic two-pass restriction applies to **first-pass
expressions** only: `ORG`, `DS`, `ALIGN` and `INCBIN` sizes must not
use forward references. Everything else (instruction operands, `DB`,
`DW`, `END`) may.

## Instructions

The full Intel 8080 instruction set, with the Monitor-listing
mnemonics:

- Data movement: `MOV r,r` `MVI r,n` `LXI rp,nn` `STAX` `LDAX`
  `SHLD` `LHLD` `STA` `LDA` `XCHG`
- Arithmetic/logic: `ADD` `ADC` `SUB` `SBB` `ANA` `XRA` `ORA` `CMP`
  `ADI` `ACI` `SUI` `SBI` `ANI` `XRI` `ORI` `CPI` `INR` `DCR`
  `DAD` `INX` `DCX` `DAA` `CMA`
- Control: `JMP` `CALL` `RET` `RST n` and the conditional
  `Jcc`/`Ccc`/`Rcc` forms with `NZ Z NC C PO PE P M` (`JNZ`, `CZ`,
  `RC`, …)
- Stack/rotates/misc: `PUSH` `POP` (pairs `B D H SP`, `PSW` for
  PUSH/POP) `SPHL` `PCHL` `XTHL` `RLC` `RRC` `RAL` `RAR` `STC`
  `CMC` `IN port` `OUT port` `EI` `DI` `NOP` `HLT`

Registers are `A B C D E H L` (and `M` for the memory operand);
register pairs `B D H SP`.

## Projects and targets

Source lives in a *project*: a folder with a `project.json` manifest —

```json
{
  "name": "my program",
  "main": "main.asm",
  "target": { "type": "tape", "name": "PROGRAM", "autostart": true }
}
```

— where every field has a default (`main.asm`, a `tape` target named
`PROGRAM` with autostart). The target is an open set:

- **`tape`** — a cassette file the Monitor loads (`MGLD`). `name` is
  the file name the Monitor shows (A–Z and digits, kept to 8
  characters); with `autostart` the tape carries the classic PMD
  autorun trick — the loader block at 0x7FD7 whose last word
  overwrites the block reader's return slot at 0x7FFB — so the
  program starts by itself once loaded.
- **`bin`** — a raw image injected into RAM at its `ORG`, CPU
  starting at the `END` entry (or the image base).
- **`rom-module`** — a `.rmm` cartridge image: the 12-byte module
  header the Monitor BIOS executes (the CALL into its own copy
  routine, the source/count/target parameter block and the entry
  jump) followed by the program.

See the [IDE documentation](ide.md) for the build, run and export
flows around these.

## Diagnostics

Errors carry spans (file, line, column) and surface twice: as red
underlines in the editor, and as entries in the Problems panel that
jump to the offending spot on click. The assembler never aborts
halfway — it reports everything it can and still produces whatever
bytes were unambiguous.

## Example

### Filling the screen

The starter template a fresh project gets (a complete, runnable
program — colored stripes straight into video RAM, no Monitor
routines needed):

```asm
; Fills the PMD 85 screen with colored stripes.
; Build with the Build button, run with Run (tape load) or Run direct.

        ORG 1000H
START:  LXI H,0C000H    ; video RAM: 256 rows of 64 bytes
        MVI B,0         ; stripe phase
FILL:   MOV A,B
        ANI 03H         ; color bits: 0 green, 1 red, 2 blue, 3 violet
        ORI 2AH         ; pixels 001010 within the color
        MOV M,A
        INX H
        MOV A,H
        ORA A           ; H wrapped to 0 = all 64 KiB done
        JNZ FILL
        INR B
        HLT

        END START
```