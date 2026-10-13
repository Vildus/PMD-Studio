# The assembler IDE

The Project panel is a small IDE built around `pmd85-asm`, the
bundled PMD 85 assembler (the same one the disassembler round-trips
through, so highlighting and building agree). The dialect it
accepts is documented in [the assembler reference](asm-dialect.md).

A *project* is a folder with a `project.json` manifest:

```json
{
  "name": "my program",
  "main": "main.asm",
  "target": { "type": "tape", "name": "PROGRAM", "autostart": true }
}
```

Every field has a default (`main.asm`, a `tape` target named
`PROGRAM` with autostart), and the target is an open set — `tape`
(a cassette file the monitor loads), `bin` (a raw image injected
into RAM), and `rom-module` (a `.rmm` cartridge image).

## The editor

The panel opens a project folder (or creates one with a starter
template), lists the `.asm`/`.inc` sources with dirty markers, and
edits them in a syntax-highlighting editor whose colors come from
the assembler's own lexer: mnemonics glow, literals are warm,
comments dim. **Build** assembles the current buffers (unsaved edits
included, `INCLUDE`s read from disk when not open) and writes the
artifact into the project's `release/` folder; problems appear in
the editor as red underlines and in the Problems panel as a
clickable list that jumps to the offending spot. **Save** writes the
edited buffer to disk, the find button opens find/replace, the
go-to button jumps to a line. In the file tree, right-click a file
for **New file**, **Rename** and **Delete** (delete removes the
file from disk, behind a confirmation). Closing a project (or the
app) with unsaved edits raises a guard dialog: save, discard or
cancel.

### Keyboard shortcuts

Shortcuts exist only while the editor itself has keyboard focus —
then egui consumes the keystrokes, so they can never leak into the
emulated machine (whose keyboard they would otherwise be: F5 is the
PMD's K4 key, Ctrl its STOP key). Elsewhere the host keyboard stays
the machine's and the IDE is driven by its buttons.

| Shortcut | Action |
|---|---|
| `Ctrl+F` | Toggle find/replace (focuses the query on open) |
| `Ctrl+G` | Go to a line |
| `Ctrl+B` | Build |
| `Ctrl+S` | Save the active file |
| `F5` | Run (authentic tape load) |
| `Ctrl+F5` | Run direct |

### Find and replace

Matches are backlit in the editor while the bar is open. `Enter`
cycles forward, `Shift+Enter` back, `Escape` closes. The view
scrolls to the active match when it *changes* — cycling, editing
the query, replacing — and then leaves scrolling to you: the match
stays backlit and findable, but nothing pins the view in place.

## Running a built program

One button, two ways, both in the Project tab's toolbar:

- **Run — authentic.** The program is assembled into a cassette
  file, the machine goes to its booted state, the file plays into
  the (flash-loading) tape deck and the monitor's `MGLD` command is
  typed into the emulated keyboard for you. The monitor does the
  leader sync, the header, the loading and the checksum — exactly
  as with a real tape, just without the transfer time. The booted
  state comes from a cached snapshot (captured once per machine
  configuration, so the boot's memory test is skipped on every
  run); when no snapshot can be captured the flow falls back to a
  real reset. With the default autostart target the tape carries
  the classic PMD autorun trick (the loader block at 0x7FD7 whose
  last word overwrites the block reader's return slot at 0x7FFB),
  so the program starts by itself once loaded. On the 85-3 the
  snapshot already sits in Monitor 3's 85-2 compatibility mode
  (`JUMP FFF0`); without the snapshot the flow types that command
  first — Monitor 3's native stack sits too low for the autorun
  slot. The played file is a session straight from the build: the
  tape editor is never touched, so repeated runs do not pile files
  onto the tape.
- **Run direct.** The image is written straight into RAM at its
  address and the CPU starts at the `END` entry point. No tape:
  the fast inner loop while developing. The machine starts from the
  cached booted state (so the monitor's RAM variables are
  initialized and monitor routines are callable); without it, from
  a reset with the boot-time shadow map dropped — until the
  monitor's first I/O write the ROM is mirrored over the RAM, and
  an image injected into that map is unreadable.

**Export…** builds and writes the artifact wherever you pick: a
`.ptp` tape image, a `.rmm` ROM module or a raw `.bin`, per the
project's target — the same bytes the runs load.

## The ROM-module target

With the `rom-module` target, Run instead builds the program into a
`.rmm` image — the 12-byte module header the monitor BIOS executes
(the CALL into its own copy routine, the source/count/target
parameter block and the entry jump) followed by the program — plugs
it into the ROM-module connector and reboots: the monitor detects
the module, transfers the payload to RAM through the connector's
8255 and runs it, exactly like a real cartridge. On the 85-1 (whose
Monitor 1 has no boot-time detection) the app types the `BASIC G`
transfer command instead. The run's module is transient: the status
bar shows it as `(project)` while attached, and the next reboot or
restart ejects it back to the configured module.
