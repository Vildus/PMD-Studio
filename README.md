# PMD85-rs

An emulator of the Tesla PMD 85 family of 8-bit Czechoslovak home computers
(PMD 85-1, 85-2, 85-2A, 85-3), written in Rust.

The CPU core is a cycle-counted Intel 8080 validated against the classic
CP/M diagnostic ROMs (`8080PRE`, `TST8080`, `CPUTEST`, `8080EXM`).
The machine side follows MAME's `pmd85.cpp` / `pmd85_m.cpp` driver
(memory maps including the boot-time startup shadow map, keyboard matrix
through the system 8255, speaker and keyboard LED, PMD 85-3 ROM/VRAM
banking), plus a cassette deck wired to the 8251 with flash-loading.

## Crates

| Crate | Contents |
|---|---|
| `crates/pmd85-core` | Pure emulation logic: i8080 CPU, memory maps, 8255/8253/8251 chips, keyboard matrix, VRAM decode, cassette tape images and deck, disassembler, save states, machine glue. No dependencies. |
| `crates/pmd85-asm` | PMD 85 assembler: lenient monitor-style dialect, two passes, segments and symbol table, spanned diagnostics, round-trips the bundled ROM images bit-exactly. No dependencies. |
| `crates/pmd85-app` | Desktop application: winit window + wgpu blit of the 288x256 framebuffer, egui UI with dockable panels, 50 Hz real-time pacing, debugger, tape editor, assembler IDE, speaker audio. |

## Running

```
cargo run -p pmd85-app --release
```

Options:

- `--model <name>` — `85-1`, `85-2`, `85-2A` or `85-3` (default `85-3`)
- `--monitor <path>` — monitor ROM image (default `Rom/<model's monitor>.rom`)
- `--rom-module <path>` — optional 8 KiB ROM module (`.rmm`) at 0x8000
- `--rom-dir <dir>` — ROM directory (default: the repository's `Rom/`)
- `--mute` — disable speaker output

Speaker audio plays through the default output device. The PMD 85 sound
circuit is emulated as a whole: PC2 of the system 8255 drives the piezo
transducer directly (software-generated square wave, e.g. BASIC beeps),
while PC0 and PC1 gate fixed 1 kHz / 4 kHz tones derived from the video
divider — which is how the monitor ROMs make their key clicks. The app
converts the cycle-stamped speaker edges into a mono stream (any output
sample rate, exact integer timing) and feeds cpal through a small ring
buffer; without an audio device it runs silent.

The Monitor-3 command line works: type e.g. `DUMP 0100` (addresses are
4-digit hex) and press Enter. `JUMP FFF0` switches the PMD 85-3 into the
PMD 85-2 compatibility mode.

## Interface

A transport bar (run/pause/stop/reset, save/load state, speed and turbo,
mute, panel toggles, settings) sits above a docking area: the emulated
screen is the permanent center tab, and the Keyboard, Joystick,
Cassette tape, Debugger and Memory panels dock around it (drag tabs,
split, float them as windows). Which panels are open and where they
sit — plus the window geometry and maximized state — persist in
`layout.json` across restarts.

The settings window covers the machine configuration (model, monitor
ROM, ROM module, applied on reboot), audio, speed, tape behaviour and
the theme: built-in presets plus a color customizer; custom themes are
JSON files in the themes directory. When the custom titlebar is on (the
default), the app draws its own titlebar — app icon, window buttons,
drag anywhere on the bar, double-click to maximize, invisible resize
borders — instead of the system frame; it can be switched off in
Settings to get the system frame back.

Emulation speed runs from 0.25x to 50x real time plus an unthrottled
turbo mode; transient errors and infos surface as popups, bottom-right.

## Debugger

The Debugger panel merges a live register strip (AF/BC/DE/HL, SP, PC,
flags, cycle count), the step controls (Step, Step Over a call,
Continue after a breakpoint — also `Alt+F10`/`Alt+F11`/`Alt+F12` from
anywhere) and the disassembly listing: monitor-style mnemonics for all
256 opcodes, a clickable breakpoint gutter, the current PC highlighted.
The listing follows the PC by default — scrolling only peeks around it,
never loses it; unchecking "follow PC" freezes the view for free
navigation (scroll, goto). Breakpoints are session-only. The Memory
panel is a live hex+ASCII dump of the full 64 KiB, read through the
bus (so 85-3 banking is reflected), with region annotations and goto.

## Assembler IDE

The Project panel is a small IDE built around `pmd85-asm`, the
bundled PMD 85 assembler (the same one the disassembler round-trips
through, so highlighting and building agree). A *project* is a
folder with a `project.json` manifest:

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

The panel opens a project folder (or creates one with a starter
template), lists the `.asm`/`.inc` sources with dirty markers, and
edits them in a syntax-highlighting editor whose colors come from
the assembler's own lexer: mnemonics glow, literals are warm,
comments dim. `Ctrl+B` builds the current buffers (unsaved edits
included, `INCLUDE`s read from disk when not open); problems appear
in the editor as red underlines and in the Problems panel as a
clickable list that jumps to the offending spot. `Ctrl+S` saves,
`Ctrl+F` opens find/replace (Enter/Shift+Enter cycle matches,
matches are backlit in the editor), `Ctrl+G` goes to a line.

Running a built program is one key, two ways, both through the
Project tab:

- **F5 — authentic.** The program is assembled into a cassette file,
  the machine goes to its booted state, the file plays into the
  (flash-loading) tape deck and the monitor's `MGLD` command is typed
  into the emulated keyboard for you. The monitor does the leader
  sync, the header, the loading and the checksum — exactly as with a
  real tape, just without the transfer time. The booted state comes
  from a cached snapshot (captured once per machine configuration, so
  the boot's memory test is skipped on every run); when no snapshot
  can be captured the flow falls back to a real reset. With the
  default autostart target the tape carries the classic PMD autorun
  trick (the loader block at 0x7FD7 whose last word overwrites the
  block reader's return slot at 0x7FFB), so the program starts by
  itself once loaded. On the 85-3 the snapshot already sits in
  Monitor 3's 85-2 compatibility mode (`JUMP FFF0`); without the
  snapshot the flow types that command first — Monitor 3's native
  stack sits too low for the autorun slot. The played file is a
  session straight from the build: the tape editor is never touched,
  so repeated runs do not pile files onto the tape.
- **Ctrl+F5 — direct.** The image is written straight into RAM at
  its address and the CPU starts at the `END` entry point. No tape:
  the fast inner loop while developing. The machine starts from the
  cached booted state (so the monitor's RAM variables are initialized
  and monitor routines are callable); without it, from a reset with
  the boot-time shadow map dropped — until the monitor's first I/O
  write the ROM is mirrored over the RAM, and an image injected into
  that map is unreadable.

With the `rom-module` target, F5 instead builds the program into a
`.rmm` image — the 12-byte module header the monitor BIOS executes
(the CALL into its own copy routine, the source/count/target
parameter block and the entry jump) followed by the program —
plugs it into the ROM-module connector and reboots: the monitor
detects the module, transfers the payload to RAM through the
connector's 8255 and runs it, exactly like a real cartridge. On
the 85-1 (whose Monitor 1 has no boot-time detection) the app
types the `BASIC G` transfer command instead. The run's module is
transient: the status bar shows it as `(project)` while attached,
and the next reboot or restart ejects it back to the configured
module.

The assembler dialect is deliberately lenient: case-insensitive,
labels with or without colon, `EQU` in any order, numbers as `123`,
`0x1F`, `1FH`, `1010B` or `'A'`, the usual directives (`ORG`, `EQU`,
`DB`/`DW`/`DS`, `INCBIN`, `ALIGN`, `INCLUDE`, `END`) with the
classic aliases (`DEFB`, `WORD`, `RESB`, …), and expressions with
`+ - * / % & | ^ ~ << >>`, `$` for the current address. Macros:
`name MACRO p1, p2 ...` captures a body up to `ENDM`, invoked as
`name arg1, arg2` with token-wise parameter substitution; `LOCAL`
inside a body declares labels renamed per invocation so repeated
expansions do not collide.

## Cassette tapes

The tape panel edits and plays `.ptp` (and old `.pmd`) tape images:
files are listed with position, length, name, type, start address and
checksum status; raw binaries can be imported with tape metadata,
files exported, moved, deleted. Select a file, press Play, then type
`MGLD nn` on the PMD to load it; saves made with `MGSV` are harvested
back into the tape automatically. Playback normally feeds the real
modulated signal through the 8251 (data tone optionally audible in the
speaker); flash-loading can instead intercept the monitor's read loops
and deliver block data directly, with a fallback to the real signal
for custom readers. The loaded tape (including unsaved edits) is
restored on the next start.

## Save states

The whole machine — CPU, RAM, banking and the peripheral chips —
snapshots to `.pss` files (Save/Load in the transport bar, refused
while the tape runs; the cassette deck is deliberately not covered, a
state can only be taken while it is idle). `Alt+F5`/`Alt+F9`
quick-save/restore a slot in the app's config directory without a
dialog.

## Keyboard mapping

Physical, PMD 85 is QWERTZ — host Y/Z map to PMD Z/Y:

- letters, digits, `Space`, `Enter`, `Tab`, `Backspace`, `Delete`, `Insert`,
  arrows, `Home`/`End` map directly
- `Shift` → SHIFT, `Esc`/`Ctrl` → STOP
- `F1`..`F12` → function keys `K0`..`K11`

While Left `Alt` is held the keyboard is in host-shortcut mode and no
keys reach the machine: `Alt+F5`/`Alt+F9` quick-save/load state,
`Alt+F10` step, `Alt+F11` step over, `Alt+F12` continue. The Keyboard
panel is a reference view of the PMD layout: caps light up while the
corresponding emulated key is held, and they are clickable (pointer
held on a cap presses the key, sliding across caps slides the
keypress).

## Joysticks

The 4004/482 joystick (five buttons: down, up, right, left, fire) is
emulated on both I/O board connectors, GPIO/0 (K3, port `4Ch`) and
GPIO/1 (K4, port `4Dh`), behind the MH8286 driver as on hardware: a
program that has not opened the driver transistor (PC4/PC0 after the
`92h`/`11h` init) reads a released stick, just like the real machine.

A gamepad (e.g. a PS4 controller) drives the joysticks: the D-pad and
left stick move, `Cross`/`Circle`/`Square`/`Triangle` and the
shoulders fire. Each pad is assigned to a connector (or off) in the
Joystick panel — a dockable tab next to the Keyboard reference, with a
live diagram of both sticks that is also mouse-clickable, the
per-connector driver state and the master enable switch. Assignments
persist in the settings.

## Desktop integration

On Wayland, taskbars take the application icon from the desktop entry
database, not from the window (there is no client-side window icon);
on X11 the same entry makes taskbars group and identify the window.
One-time, user-level (no root):

```
cargo build --release
dist/install-user.sh
```

This installs `pmd85.desktop` (pointing at the built binary) and the
icon into `~/.local/share`, and refreshes the desktop databases. If
the taskbar does not pick it up immediately, relog or restart the
panel. Re-run the script after moving the repository or rebuilding at
a different path.

## ROM images

Monitor ROMs live in `Rom/` (see `Rom/rom-list.txt`). They are not part of
this repository's source; the boot tests require them to be present.

## Testing

```
cargo test --workspace
```

- `pmd85-core` unit tests: every instruction group, memory maps, chips,
  keyboard, VRAM decode, speaker edge log and edge-to-sample expansion,
  disassembler golden tables, tape image parse/build, save-state
  round-trips
- CP/M diagnostic ROMs run headless through the machine (`diag_tests`);
  `8080EXM` is `#[ignore]`d (run it in release: ~20 s)
- `boot_tests`: every model boots its monitor ROM to a working command
  line, keyboard echo lands in VRAM, Monitor-3 executes a real `DUMP`
  command end-to-end, and keypresses produce the speaker click
- `module_tests`: the monitor BIOS itself detects an attached ROM module
  through the module 8255, copies it to RAM 0 and runs it (basic1/2/2A/3,
  one test per model, plus a no-module control)
- `pmd85-app`: ring-buffer and live audio-stream tests (skipped silently
  when no output device is present), plus headless egui frame-building
  tests that render the UI in every state (dock tabs, debugger views,
  settings, tape, titlebar) and round-trip tests for settings, sessions
  and the dock/window layout files. The IDE's run flows are covered end
  to end against the real monitor ROMs: F5 (tape autorun over every
  model, including the 85-3's cached compatibility boot), Ctrl+F5
  (direct RAM inject over every model), the ROM-module target, the
  booted-state cache and the run module's ejection on reboot, plus a
  highlighter test that keeps the editor's colors aligned across blank
  lines.

## Debug tools

`crates/pmd85-core/examples/`:

- `boot_dump` — boots a model, optionally presses keys, writes the decoded
  screen to a PNG and a coarse ASCII rendering (e.g.
  `cargo run -p pmd85-core --example boot_dump -- --model 85-3 --frames 400
  --press "DUMP 0100[ENTER]" --out screen.png`); the press string also
  supports `[SHIFT]`/`[UNSHIFT]` and `[WAIT:n]` to idle n frames (useful
  after a command that boots a ROM module)
- `kbd_trace` — IN/OUT, VRAM and RAM-state tracing while typing

## Status

Done: CPU, memory, keyboard, video, boot of all four models, monitor
command execution, ROM module support (monitor BIOS detects/transfers/runs
BASIC modules), wgpu display, speaker audio, docking UI with layout
persistence, themes, save states, speed control/turbo, cassette tape
images with a full editor, playback and flash-loading, debugger
(registers, disassembly, breakpoints, step/step-over/continue), memory
dump, custom titlebar, desktop integration, assembler IDE (projects,
highlighting editor, problems panel, authentic and direct run).

Not yet: serial I/O beyond the cassette path (the 8251 serves the
deck), disk image support.

## References

- [pmd85.borik.net wiki](https://pmd85.borik.net/wiki/) — hardware docs
- MAME `tesla/pmd85.cpp` driver (memory maps, I/O decoding)
- superzazu/8080 `i8080.c` (undocumented flag semantics)
