//! Emulator application state: the machine, run state, speed/pacing,
//! audio, settings and the ROM catalog. UI-agnostic (the `ui` module
//! draws it), windowing-agnostic (main.rs drives it).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pmd85_core::machine::CPU_CLOCK_HZ;
use pmd85_core::tape::{self, Tape};
use pmd85_core::tapedeck::PlayItem;
use pmd85_core::vram::{self, ColorProfile, HEIGHT, WIDTH};
use pmd85_core::{Machine, Model};

use crate::audio;
use crate::catalog::RomCatalog;
use crate::config::{self, AppSettings};
use crate::gamepad::GamepadInput;
use crate::project::Project;
use crate::speed::{Pacer, SPEED_MAX, SPEED_MIN, TURBO_BUDGET};
use crate::ui::theme::Theme;
use crate::ui::UiState;

/// egui_dock's serde support writes runtime-only rect coordinates
/// (`x`/`y` of `Pos2`) as JSON `null` when they hold non-finite
/// floats — and then cannot read its own output back (`null` where
/// an `f32` is expected). Those coordinates are stale runtime state
/// (recomputed on the next UI pass), so replace the nulls with
/// zeros and the layout parses again. Other nulls (e.g.
/// `focused_surface`) are legitimate and stay.
pub(crate) fn sanitize_layout(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, val) in map.iter_mut() {
                if (key == "x" || key == "y") && val.is_null() {
                    *val = serde_json::json!(0.0);
                } else {
                    sanitize_layout(val);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(sanitize_layout),
        _ => {}
    }
}

/// Remembered window geometry, stored alongside the dock layout in
/// `layout.json` (under its own `"window"` key): the inner size from
/// the last time the window was not maximized, and whether it was
/// maximized when the app exited.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(crate) struct WindowLayout {
    /// Inner size in points, `[width, height]`.
    pub size: [f32; 2],
    pub maximized: bool,
}

impl Default for WindowLayout {
    fn default() -> Self {
        WindowLayout {
            size: [980.0, 760.0],
            maximized: false,
        }
    }
}

impl WindowLayout {
    /// The layout with its size forced to something sane: a corrupt
    /// or hand-edited file must not produce a zero, negative or
    /// absurd window.
    fn sanitized(self) -> Self {
        let default = Self::default().size;
        let mut size = self.size;
        for (index, dim) in size.iter_mut().enumerate() {
            *dim = if dim.is_finite() {
                dim.clamp(320.0, 16384.0)
            } else {
                default[index]
            };
        }
        WindowLayout { size, ..self }
    }
}

/// Emulated screen size (pixels).
pub const SCREEN: (usize, usize) = (WIDTH, HEIGHT);
/// How long a notification popup stays on screen.
const NOTIFICATION_TTL: Duration = Duration::from_secs(6);

/// Cold-start configuration of the machine.
#[derive(Clone, Debug, PartialEq)]
pub struct MachineConfig {
    pub model: Model,
    /// Monitor ROM: `None` = the model's default from the ROM
    /// directory; `Some(name)` resolves through the catalog, falling
    /// back to treating the string as a path.
    pub monitor: Option<String>,
    /// ROM module: `None` = none; `Some(name)` as above.
    pub rom_module: Option<String>,
}

impl MachineConfig {
    /// The machine this configuration will produce is not the one
    /// currently running (i.e. a restart is needed)?
    fn differs_from(&self, other: &MachineConfig) -> bool {
        self != other
    }
}

/// A cached snapshot of the machine right after the monitor booted
/// to its command prompt — on the 85-3, after Monitor 3's `JUMP
/// FFF0` compatibility switch and its reboot. IDE runs restore it
/// instead of rebooting for real, skipping the boot-time memory
/// test. Keyed by the monitor's identity; invalidated whenever the
/// machine is rebuilt.
#[derive(Clone, Debug)]
pub(crate) struct BootState {
    /// Monitor identity (length + CRC) the snapshot was taken with.
    sig: (u16, u8),
    bytes: Vec<u8>,
}

/// Emulation speed: a real-time multiplier plus a turbo toggle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpeedState {
    /// Multiplier of real time (1.0 = 50 Hz as on hardware).
    pub multiplier: f64,
    /// Unthrottled: run as many frames as fit in the compute budget,
    /// audio muted.
    pub turbo: bool,
}

impl Default for SpeedState {
    fn default() -> Self {
        SpeedState {
            multiplier: 1.0,
            turbo: false,
        }
    }
}

/// A file on the tape: its block (the header + body pair) plus any
/// headerless continuation blocks that follow it (pictures, saved
/// positions).
#[derive(Clone, Debug)]
pub struct TapeFile {
    /// Index of the file's block in `Tape::blocks`. The block has no
    /// header for a headerless run with no file ahead of it (e.g. a
    /// truncated tape).
    pub block: usize,
    /// Headerless continuation block indices, in tape order.
    pub continuations: Vec<usize>,
}

impl TapeFile {
    /// Contiguous block-index span `[start, start + len)` of the file.
    fn span(&self) -> (usize, usize) {
        let end = self.continuations.last().copied().unwrap_or(self.block);
        (self.block, end - self.block + 1)
    }
}

/// Cassette tape editor and transport state: the tape image being
/// edited, the selected file and the playback session feeding the
/// machine. Also harvests blocks the machine records.
#[derive(Debug)]
pub struct TapeState {
    /// The tape image being edited (empty = no tape).
    pub tape: Tape,
    /// File the tape was loaded from / was last saved to.
    pub path: Option<PathBuf>,
    /// Unsaved edits since the last load/save.
    pub dirty: bool,
    /// Selected file, an index into [`TapeState::files`].
    pub selected: Option<usize>,
    /// Stop playback after the selected file's blocks (at the next
    /// file header); when off, the rest of the tape follows.
    pub auto_stop: bool,
    /// Fast-load played files: body blocks are served straight to the
    /// monitor's read loops instead of through the tape signal, so
    /// MGLD (and multi-block loaders) finish in a fraction of the
    /// time while checksums, filters and autorun still run for real.
    pub flash: bool,
    /// Active playback session (the block queue itself lives in the
    /// machine's deck, which advances blocks synchronously).
    play: bool,
    /// The recorder was active at the previous pump (edge detector
    /// for harvesting the recorded stream).
    was_recording: bool,
}

impl Default for TapeState {
    fn default() -> Self {
        TapeState {
            tape: Tape::default(),
            path: None,
            dirty: false,
            selected: None,
            auto_stop: true,
            flash: true,
            play: false,
            was_recording: false,
        }
    }
}

impl TapeState {
    /// The tape's blocks grouped into files, in tape order.
    pub fn files(&self) -> Vec<TapeFile> {
        let mut files: Vec<TapeFile> = Vec::new();
        for (i, block) in self.tape.blocks.iter().enumerate() {
            if block.header.is_some() || files.is_empty() {
                // A new file, or a headerless run at the tape start
                // with no file ahead of it.
                files.push(TapeFile {
                    block: i,
                    continuations: Vec::new(),
                });
            } else {
                files.last_mut().unwrap().continuations.push(i);
            }
        }
        files
    }

    /// Load a `.ptp`/`.pmd` image, replacing the current tape. The
    /// caller is responsible for stopping playback first.
    pub fn open(&mut self, path: &Path) -> Result<(), String> {
        let tape = tape::load(path).map_err(|e| format!("{}: {e}", path.display()))?;
        self.play = false;
        self.tape = tape;
        self.path = Some(path.to_path_buf());
        self.dirty = false;
        self.selected = None;
        Ok(())
    }

    /// Start a fresh empty tape. The caller stops playback first.
    pub fn clear(&mut self) {
        self.play = false;
        self.tape = Tape::default();
        self.path = None;
        self.dirty = false;
        self.selected = None;
    }

    /// Save to the tape's current path.
    pub fn save(&mut self) -> Result<(), String> {
        let path = self.path.clone().ok_or("no file chosen yet")?;
        self.save_as(&path)
    }

    /// Save the tape as a PTP image to `path`.
    pub fn save_as(&mut self, path: &Path) -> Result<(), String> {
        tape::save(path, &self.tape).map_err(|e| format!("{}: {e}", path.display()))?;
        self.path = Some(path.to_path_buf());
        self.dirty = false;
        Ok(())
    }

    /// Append a new file built from raw content (import). Returns
    /// the new file's index.
    pub fn import_file(
        &mut self,
        number: u8,
        block_type: u8,
        name: &str,
        start: u16,
        content: &[u8],
    ) -> Result<usize, String> {
        let block = tape::make_file(number, block_type, name, start, content)?;
        self.tape.blocks.push(block);
        self.dirty = true;
        let index = self.files().len() - 1;
        self.selected = Some(index);
        Ok(index)
    }

    /// The loadable content of a file (its block's body payload).
    pub fn export_file(&self, file: usize) -> Option<Vec<u8>> {
        let files = self.files();
        Some(
            self.tape
                .blocks
                .get(files.get(file)?.block)?
                .content()
                .to_vec(),
        )
    }

    /// Delete a file: its block and continuation blocks.
    pub fn delete_file(&mut self, file: usize) {
        let files = self.files();
        let Some(f) = files.get(file) else {
            return;
        };
        let (start, len) = f.span();
        for i in (start..start + len).rev() {
            self.tape.blocks.remove(i);
        }
        self.dirty = true;
        let count = self.files().len();
        self.selected = self.selected.map(|s| s.min(count.saturating_sub(1)));
    }

    /// Move a file one place up (`delta < 0`) or down the tape.
    pub fn move_file(&mut self, file: usize, delta: i32) {
        let delta = delta.signum();
        let files = self.files();
        let target = file as i64 + delta as i64;
        if !((0..files.len() as i64).contains(&target)) {
            return;
        }
        let (m_start, m_len) = files[file].span();
        let (o_start, o_len) = files[target as usize].span();
        let moved: Vec<_> = self.tape.blocks.drain(m_start..m_start + m_len).collect();
        // After the drain the neighbour below shifted up by m_len;
        // the neighbour above did not move.
        let insert_at = if delta > 0 {
            o_start + o_len - m_len
        } else {
            o_start
        };
        self.tape.blocks.splice(insert_at..insert_at, moved);
        self.dirty = true;
        self.selected = Some(target as usize);
    }

    /// Request playback of `file`: its header, body and continuation
    /// blocks; with `auto_stop` off the rest of the tape follows. The
    /// blocks are queued in the machine's deck, which feeds them one
    /// after another. With `flash` set, body blocks are served through
    /// the flash-load intercepts instead of the tape signal. Returns
    /// whether playback started.
    pub fn request_play(&mut self, file: usize, machine: &mut Machine) -> bool {
        let files = self.files();
        let Some(f) = files.get(file) else {
            return false;
        };
        self.selected = Some(file);
        let flash = self.flash;
        let mut rows: Vec<(usize, bool)> = Vec::new();
        if self.tape.blocks[f.block].header.is_some() {
            rows.push((f.block, true));
        }
        rows.push((f.block, false));
        for &c in &f.continuations {
            rows.push((c, false));
        }
        if !self.auto_stop {
            let (_, len) = f.span();
            for i in f.block + len..self.tape.blocks.len() {
                if self.tape.blocks[i].header.is_some() {
                    rows.push((i, true));
                }
                rows.push((i, false));
            }
        }
        if rows.is_empty() {
            return false;
        }
        let queue: Vec<PlayItem> = rows
            .into_iter()
            .map(|(idx, head)| {
                let block = &self.tape.blocks[idx];
                PlayItem {
                    data: if head {
                        block.header_bytes.clone()
                    } else {
                        block.body_bytes.clone()
                    },
                    head,
                    flash,
                }
            })
            .collect();
        machine.bus.tape_play_session(queue);
        self.play = true;
        true
    }

    /// Stop playback (nothing more is fed to the machine).
    pub fn stop_playback(&mut self, machine: &mut Machine) {
        self.play = false;
        machine.bus.tape_stop();
    }

    /// Play a project run's blocks as a deck session, straight from
    /// the build: they never enter the tape editor, so repeated runs
    /// do not accumulate files on the tape. Returns whether the
    /// session started.
    pub fn play_run(&mut self, items: Vec<PlayItem>, machine: &mut Machine) -> bool {
        if items.is_empty() {
            return false;
        }
        machine.bus.tape_play_session(items);
        self.play = true;
        true
    }

    /// Whether a playback session is active.
    pub fn is_playing(&self) -> bool {
        self.play
    }

    /// Feed the machine from the playback session and harvest
    /// recorded blocks. Called once per emulated frame (and once per
    /// rendered frame while paused); returns a user notification when
    /// something was recorded.
    pub fn pump(&mut self, machine: &mut Machine) -> Option<String> {
        // A session that played to the end, or that the machine
        // reset out from under us.
        if self.play && !machine.bus.tape.is_playing() {
            self.play = false;
        }

        // Recording: harvest the stream once the recorder goes quiet.
        let recording = machine.bus.tape.is_recording();
        let mut notification = None;
        if self.was_recording && !recording {
            let recorded = machine.bus.tape.take_recorded();
            let before = self.tape.blocks.len();
            self.tape.append_ptp_stream(&recorded);
            let added = self.tape.blocks.len() - before;
            if added > 0 {
                self.dirty = true;
                notification = Some(format!(
                    "Tape: recorded {added} block{} from the machine",
                    if added == 1 { "" } else { "s" }
                ));
            }
        }
        self.was_recording = recording;
        notification
    }
}

/// A keystroke sequence the app types into the emulated keyboard
/// (the project Run's monitor command). Keys are held and released
/// like a human's press so the monitor's scan sees them. The first
/// key waits for the monitor to settle at its command prompt —
/// detected by the keyboard scan column actively cycling, which the
/// command loop does and the boot (RAM test) does not — with a
/// failsafe timeout for other monitors.
#[derive(Debug)]
pub(crate) struct AutoType {
    queue: VecDeque<AutoTypeItem>,
    current: Option<pmd85_core::keyboard::Key>,
    /// Frames left in the current press/release cycle.
    phase: u16,
    /// Still waiting for the command prompt before the next key.
    waiting: bool,
    /// A tape session to start when the last key of the command goes
    /// down (the monitor dispatches on that Enter, so the tape is
    /// already rolling when its read loop starts; starting any
    /// earlier would burn the flash session's patience while the
    /// machine still boots).
    play_on_start: Option<RunSession>,
    /// The command-prompt detector (the failsafe clock is
    /// `waited`).
    detector: PromptDetect,
    /// Frames waited so far (the failsafe clock).
    waited: u32,
}

/// Detects the monitor's command prompt: while the command loop
/// polls the keyboard, the scan column cycles constantly, and while
/// the boot code (the RAM test) runs it stands still. One observation
/// per frame; [`PromptDetect::step`] is true once the column changed
/// [`AUTOTYPE_SCAN_FRAMES`] frames in a row.
#[derive(Debug, Default)]
pub(crate) struct PromptDetect {
    /// The last-seen keyboard scan column, and how many consecutive
    /// frames it changed in (the scan running).
    last_scan: u8,
    changes: u8,
}

impl PromptDetect {
    /// Observe the machine for one frame; true at the prompt.
    fn step(&mut self, machine: &Machine) -> bool {
        let scan = machine.bus.ppi_system_scan();
        self.changes = if scan != self.last_scan {
            self.changes.saturating_add(1)
        } else {
            0
        };
        self.last_scan = scan;
        self.changes >= AUTOTYPE_SCAN_FRAMES
    }
}

/// A tape playback session prepared by a project run: the built
/// blocks queued straight into the deck (they never enter the tape
/// editor, so repeated runs do not pile files onto the tape), with
/// the notification text for the flash-load popup.
#[derive(Debug)]
pub(crate) struct RunSession {
    items: Vec<PlayItem>,
    flash: bool,
    name: String,
}

/// One step of the auto-typer's script.
#[derive(Clone, Copy, Debug)]
enum AutoTypeItem {
    /// Press (and release) the key.
    Press(pmd85_core::keyboard::Key),
    /// Wait for the monitor's command prompt again — used between
    /// the 85-3's `JUMP FFF0` (whose compatibility switch reboots
    /// the machine) and the `MGLD` that follows.
    WaitPrompt,
}

/// After the prompt shows, a couple of frames of grace.
const AUTOTYPE_SETTLE: u16 = 2;
/// Give up waiting for the prompt after this many frames and type
/// anyway (some 12 s at 50 Hz — far beyond any monitor boot).
const AUTOTYPE_TIMEOUT: u32 = 600;
/// Consecutive frames of a cycling scan column that mean "prompt".
const AUTOTYPE_SCAN_FRAMES: u8 = 3;
/// Frames the boot-state capture runs per phase before giving up
/// (the slowest monitor boots in well under this; a blank ROM never
/// reaches a prompt and the run flows fall back to a real reset).
const BOOT_CAPTURE_MAX_FRAMES: u32 = 3000;

impl AutoType {
    fn new(script: &[AutoTypeItem], play_on_start: Option<RunSession>) -> Self {
        AutoType {
            queue: script.iter().copied().collect(),
            current: None,
            phase: 0,
            waiting: true,
            play_on_start,
            detector: PromptDetect::default(),
            waited: 0,
        }
    }
}

/// What the UI asks the window manager to do. The UI draws with only
/// `&mut App` and cannot touch the window, so the custom titlebar
/// queues these and the winit loop drains them after each frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowRequest {
    /// Begin an interactive move (a titlebar drag).
    Drag,
    /// Begin an interactive resize from the given screen edge.
    Resize(winit::window::ResizeDirection),
    /// Minimize the window.
    Minimize,
    /// Maximize the window, or restore it if it already is.
    ToggleMaximize,
    /// Show/hide the system window frame (the custom-titlebar
    /// setting; the system frame is shown exactly when the custom
    /// titlebar is not).
    Decorate(bool),
    /// Quit the app, like the system close button (saves the session).
    Close,
}

/// What happens once the unsaved-changes guard is resolved. `Some`
/// in [`App::close_guard`] means the guard dialog is up: closing the
/// project (or the app) with dirty buffers waits for the user to
/// pick save / discard / cancel first.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AfterClose {
    /// Close the project, nothing else.
    Project,
    /// Close the project, then open a project folder.
    Open(PathBuf),
    /// Close the project, then create a project folder.
    Create(PathBuf),
    /// Exit the app.
    Exit,
}

/// What a successful build produces for the project's target, and
/// what the Export dialog and the `release/` folder write.
#[derive(Debug)]
pub(crate) enum Artifact {
    /// The tape image's file blocks: the built program as one tape
    /// file, or the autorun loader pair.
    Tape {
        /// Blocks in tape order, ready for [`pmd85_core::tape::save`].
        blocks: Vec<pmd85_core::tape::TapeBlock>,
    },
    /// A `.rmm` ROM module image, with the entry it boots from.
    Rom { module: Vec<u8>, entry: u16 },
    /// The flat image (a raw `.bin`; the run paths inject it at its
    /// assembler address).
    Bin { image: Vec<u8> },
}

pub struct App {
    pub ctx: egui::Context,
    pub machine: Machine,
    /// Configuration the running machine was built from.
    pub config: MachineConfig,
    /// Working copy edited by the settings window.
    pub pending_config: MachineConfig,

    pub rom_dir: PathBuf,
    pub catalog: RomCatalog,

    /// The monitor ROM bytes the running machine was built from —
    /// what the scratch machine that captures the booted state (see
    /// [`BootState`]) is built from.
    pub(crate) monitor_rom: Vec<u8>,
    /// The configured ROM module image (from `MachineConfig`): what
    /// the connector returns to when a project-run module is
    /// ejected.
    pub(crate) config_module: Option<Vec<u8>>,
    /// The ROM module image attached by the last project run (the
    /// `rom-module` target): transient — a reboot ejects it, like
    /// pulling the cartridge.
    pub(crate) ide_module: Option<Vec<u8>>,
    /// Cached booted machine state for IDE runs (memory test
    /// skipped). Cleared whenever the machine is rebuilt.
    pub(crate) boot_state: Option<BootState>,

    pub profile: ColorProfile,
    pub audio: Option<audio::SpeakerOut>,

    pub running: bool,
    pub speed: SpeedState,
    pub pacer: Pacer,

    /// Cassette tape editor/deck state.
    pub tape: TapeState,

    /// Host gamepads, mapped to the 4004/482 joysticks.
    pub gamepads: GamepadInput,

    /// The open IDE project (sources + build), if any.
    pub project: Option<Project>,

    /// The unsaved-changes guard: `Some` while its dialog is up (see
    /// [`AfterClose`]).
    pub(crate) close_guard: Option<AfterClose>,
    /// Set when the guard's user answered and the exit may proceed;
    /// the queued window close re-checks dirty buffers, this is the
    /// "yes, really" flag.
    pub(crate) exit_confirmed: bool,

    /// A keystroke sequence being typed into the emulated keyboard
    /// (see [`AutoType`]).
    pub(crate) auto_type: Option<AutoType>,

    /// Emulated frames since the last machine (re)start.
    pub frames_done: u64,

    pub ui: UiState,
    pub settings: AppSettings,
    pub themes: Vec<Theme>,
    /// Name of the theme actually applied (for the settings picker).
    pub active_theme: String,

    /// Emulated screen texture (created lazily on first UI pass).
    pub screen: Option<egui::TextureHandle>,
    pub decode_buf: Vec<u8>,

    /// App icon texture for the custom titlebar (lazy, like
    /// [`App::screen`]).
    pub icon: Option<egui::TextureHandle>,
    /// Whether the window is maximized (published by the winit loop
    /// before each frame; drives the titlebar and hides the resize
    /// border).
    pub(crate) window_maximized: bool,
    /// Inner size of the window in points, tracked while the window
    /// is not maximized — what the next start restores (the maximized
    /// size is not a useful restore size).
    pub(crate) window_size: egui::Vec2,
    /// Whether this run's window should start maximized (from the
    /// restored layout; consumed at window creation).
    pub(crate) window_start_maximized: bool,
    /// Window-management requests from the UI, drained by the winit
    /// loop after each frame.
    pub(crate) window_requests: Vec<WindowRequest>,

    /// Transient error/info popups.
    notifications: VecDeque<(String, Instant)>,
    /// Where persisted settings are written (`None` = do not persist).
    settings_dir: Option<PathBuf>,
    /// Rendered frames in the last second (UI fps).
    render_times: VecDeque<Instant>,
    /// (time, total frames) samples for the emulated-speed readout.
    frame_samples: VecDeque<(Instant, u64)>,
}

impl App {
    /// Build the app state. The machine is constructed from the merged
    /// CLI/settings configuration; a missing ROM is not fatal (the
    /// machine comes up with a blank monitor and a notification).
    pub fn new(ctx: egui::Context, args: &crate::args::Args, settings: AppSettings) -> Self {
        let rom_dir = args.rom_dir.clone().unwrap_or_else(default_rom_dir);
        let catalog = RomCatalog::scan(&rom_dir);

        let model = args
            .model
            .or_else(|| {
                // CLI wins; otherwise the persisted setting.
                Model::from_str_loose(&settings.model)
            })
            .unwrap_or_default();
        let rom_module = args
            .rom_module
            .clone()
            .or_else(|| settings.rom_module.clone());
        let monitor = args.monitor.clone().or_else(|| settings.monitor.clone());
        let config = MachineConfig {
            model,
            monitor,
            rom_module,
        };

        let mut app = App {
            ctx,
            // Placeholder until rebuild_machine below; sized to satisfy
            // the monitor-size invariant.
            machine: {
                let blank = vec![0u8; model.monitor_size()];
                Machine::new(model, &blank, None)
            },
            config: config.clone(),
            pending_config: config.clone(),
            rom_dir,
            catalog,
            monitor_rom: Vec::new(),
            config_module: None,
            ide_module: None,
            boot_state: None,
            profile: ColorProfile::Rgb,
            audio: None,
            running: true,
            speed: SpeedState {
                multiplier: settings.speed.clamp(SPEED_MIN, SPEED_MAX),
                turbo: false,
            },
            pacer: Pacer::new(),
            frames_done: 0,
            tape: TapeState {
                auto_stop: settings.tape_autostop,
                flash: settings.tape_warp,
                ..TapeState::default()
            },
            gamepads: GamepadInput::new(),
            project: None,
            close_guard: None,
            exit_confirmed: false,
            auto_type: None,
            ui: UiState::default(),
            settings,
            themes: Vec::new(),
            active_theme: String::new(),
            screen: None,
            decode_buf: Vec::new(),
            icon: None,
            window_maximized: false,
            window_size: egui::vec2(980.0, 760.0),
            window_start_maximized: false,
            window_requests: Vec::new(),
            notifications: VecDeque::new(),
            settings_dir: config::config_dir(),
            render_times: VecDeque::new(),
            frame_samples: VecDeque::new(),
        };

        // Themes: built-ins plus custom files from the themes dir.
        app.themes = Theme::presets();
        if let Some(dir) = config::themes_dir() {
            app.themes.extend(Theme::load_dir(&dir));
        }
        let theme = app
            .themes
            .iter()
            .find(|t| t.name == app.settings.theme)
            .cloned()
            .unwrap_or_else(Theme::default);
        app.active_theme = theme.name.clone();
        Theme::install_fonts(&app.ctx);
        theme.apply(&app.ctx);
        // egui's built-in zoom hotkeys (Ctrl+=/-/0) live on chords the
        // machine owns: host Ctrl is STOP, so STOP + `{ }` (Ctrl+=)
        // would zoom the GUI instead of toggling Caps-Lock — likewise
        // STOP + `_ =` and STOP + `0`. Host shortcuts stay in the
        // left-Alt namespace; keyboard zoom is off entirely.
        app.ctx.options_mut(|options| options.zoom_with_keyboard = false);

        // Audio: open the device unless muted from the start.
        if !args.mute && !app.settings.mute {
            app.audio = audio::SpeakerOut::new();
        }

        // The real machine from the configuration.
        app.rebuild_machine(&config);
        app.running = true;
        app
    }
    /// Rebuild the machine from a cold-start configuration. Never
    /// panics: load failures produce a notification and a blank ROM.
    pub fn rebuild_machine(&mut self, config: &MachineConfig) {
        let (mut monitor, monitor_missing) = self.load_rom(
            &config.monitor,
            config.model.default_monitor(),
            config.model,
        );
        // The core's memory builder asserts the model's exact monitor
        // size, and the catalog offers every `.rom` in the directory
        // (1 KiB test ROMs, 16 KiB terminal ROMs, ...), so a size
        // mismatch must be caught here: like a missing file, it
        // boots a blank monitor of the right size instead of
        // panicking.
        let expected = config.model.monitor_size();
        if monitor.len() != expected {
            if !monitor_missing {
                let name = config
                    .monitor
                    .clone()
                    .unwrap_or_else(|| config.model.default_monitor().to_string());
                log::warn!(
                    "monitor ROM {name:?} is {} bytes, not the {expected} the {} expects",
                    monitor.len(),
                    config.model.name()
                );
                self.notify(format!(
                    "Monitor ROM {name} is {} bytes - the PMD {} needs {expected} - \
                     machine will not boot",
                    monitor.len(),
                    config.model.name(),
                ));
                monitor = vec![0u8; expected];
            } else {
                // A missing file whose name does not end in .rom comes
                // back empty (load_rom only pre-sizes .rom names);
                // pad it to keep the size invariant.
                monitor.resize(expected, 0);
            }
        }
        let module = config
            .rom_module
            .as_ref()
            .map(|name| self.load_rom(&Some(name.clone()), name, config.model).0);
        // Remember the parts a reboot must be able to return to, and
        // drop anything tied to the old machine: a project-run module
        // and the cached booted state do not survive a rebuild.
        self.monitor_rom = monitor.clone();
        self.config_module = module.clone();
        self.ide_module = None;
        self.boot_state = None;
        self.machine = Machine::new(config.model, &monitor, module);
        self.config = config.clone();
        self.pending_config = config.clone();
        self.frames_done = 0;
        self.pacer.resync();
        self.frame_samples.clear();
        // The new machine's cycle counter starts at 0 again; the
        // speaker timeline must follow or it renders nothing ever again.
        if let Some(audio) = &mut self.audio {
            audio.resync(self.machine.bus.total_cycles());
        }
        if monitor_missing {
            self.notify("Monitor ROM could not be loaded - machine will not boot");
        }
    }

    /// Resolve a ROM reference to (bytes, missing). `None` falls back to
    /// `fallback` (a file name in the ROM directory). A name that is
    /// not in the catalog is treated as a path.
    fn load_rom(&self, name: &Option<String>, fallback: &str, model: Model) -> (Vec<u8>, bool) {
        let name = name.as_deref().unwrap_or(fallback);
        let path = match self.catalog.find(name) {
            Some(entry) => entry.path.clone(),
            None => self.rom_dir.join(name),
        };
        match std::fs::read(&path) {
            Ok(bytes) => (bytes, false),
            Err(e) => {
                log::warn!("cannot read ROM {name:?} ({}): {e}", path.display());
                let size = if name.to_ascii_lowercase().ends_with(".rom") {
                    model.monitor_size()
                } else {
                    0
                };
                (vec![0u8; size], true)
            }
        }
    }

    /// Apply a configuration change (settings window "restart").
    pub fn apply_config(&mut self, config: MachineConfig) {
        if config.differs_from(&self.config) {
            self.rebuild_machine(&config);
            self.running = true;
            // The chosen machine becomes the boot configuration for
            // the next start (CLI arguments still override it for the
            // run they were given on).
            self.settings.model = config.model.name().to_string();
            self.settings.monitor = config.monitor.clone();
            self.settings.rom_module = config.rom_module.clone();
            self.persist();
        }
    }

    // ----- transport -----

    pub fn set_running(&mut self, running: bool) {
        if running && !self.running {
            self.pacer.resync();
        }
        self.running = running;
    }

    /// Stop: reset the machine and hold it at the power-on state.
    /// A ROM module attached by a project run is ejected, like
    /// pulling the cartridge before switching the machine off.
    pub fn stop(&mut self) {
        self.eject_ide_module();
        self.machine.reset();
        self.frames_done = 0;
        self.set_running(false);
    }

    /// Reset: warm reboot (CPU, memory maps, peripherals; RAM
    /// preserved), keeping the run state. A ROM module attached by a
    /// project run is ejected — the connector returns to whatever the
    /// configuration says.
    pub fn reset(&mut self) {
        self.eject_ide_module();
        self.machine.reset();
        self.frames_done = 0;
        self.pacer.resync();
    }

    pub fn set_multiplier(&mut self, multiplier: f64) {
        self.speed.multiplier = multiplier.clamp(SPEED_MIN, SPEED_MAX);
        self.settings.speed = self.speed.multiplier;
        self.persist();
    }

    pub fn set_turbo(&mut self, turbo: bool) {
        if self.speed.turbo && !turbo {
            // Leaving turbo: drop the wall-clock debt.
            self.pacer.resync();
        }
        self.speed.turbo = turbo;
    }

    pub fn set_mute(&mut self, mute: bool) {
        self.settings.mute = mute;
        self.persist();
    }

    // ----- tape -----

    /// Open a tape image in the editor, stopping playback first.
    /// Parse warnings become notifications.
    pub fn open_tape(&mut self, path: &std::path::Path) -> Result<(), String> {
        self.tape.stop_playback(&mut self.machine);
        self.tape.open(path)?;
        let warnings = self.tape.tape.warnings.join("; ");
        if !warnings.is_empty() {
            self.notify(format!("Tape: {warnings}"));
        }
        Ok(())
    }

    /// Start a fresh empty tape, stopping playback first.
    pub fn new_tape(&mut self) {
        self.tape.stop_playback(&mut self.machine);
        self.tape.clear();
    }

    /// Begin playing tape file `file` into the machine (the blocks are
    /// queued in the deck immediately).
    pub fn play_tape_file(&mut self, file: usize) {
        let flash = self.tape.flash;
        if self.tape.request_play(file, &mut self.machine) && flash {
            let name = self
                .tape
                .files()
                .get(file)
                .and_then(|f| self.tape.tape.blocks.get(f.block))
                .and_then(|b| b.header.as_ref())
                .map(|h| h.name_str())
                .unwrap_or_else(|| "file".into());
            self.notify(format!("Tape: flash-loading {name}"));
        }
        let _ = self.tape.pump(&mut self.machine);
    }

    /// Stop tape playback.
    pub fn stop_tape_playback(&mut self) {
        self.tape.stop_playback(&mut self.machine);
    }

    /// Toggle the auto-stop-at-next-header behavior and persist it.
    pub fn set_tape_autostop(&mut self, auto_stop: bool) {
        self.tape.auto_stop = auto_stop;
        self.settings.tape_autostop = auto_stop;
        self.persist();
    }

    /// Toggle the tape data-tone monitor and persist it.
    pub fn set_tape_monitor(&mut self, on: bool) {
        self.settings.tape_monitor = on;
        self.persist();
    }

    /// Toggle flash loading and persist it.
    pub fn set_tape_flash(&mut self, on: bool) {
        self.tape.flash = on;
        self.settings.tape_warp = on;
        self.persist();
    }

    // ----- booted-state cache ------------------------------------------------

    /// Eject the ROM module a project run attached (the
    /// `rom-module` target): the connector returns to the configured
    /// module. A reboot must not keep the run's cartridge in — on
    /// real hardware it would survive a warm reset, but the IDE's
    /// module is a *run* artifact, not a machine configuration.
    pub(crate) fn eject_ide_module(&mut self) {
        if self.ide_module.take().is_some() {
            self.machine.bus.memory.rom_module = self.config_module.clone();
        }
    }

    /// Capture the machine's booted state for the current
    /// configuration: a scratch machine of the same model and monitor,
    /// reset and run — un-paced, as fast as the host can emulate —
    /// until the monitor sits at its command prompt. On the 85-3 the
    /// `JUMP FFF0` compatibility switch follows (the command only
    /// transfers control, so entering `0xFFF0` directly is the same
    /// journey), and the snapshot is the compatibility boot's prompt —
    /// the state the run flows want anyway. Returns `None` when no
    /// prompt shows (blank monitor, or a configured module owns the
    /// boot and never reaches one).
    fn capture_boot_state(&mut self) -> Option<BootState> {
        if self.config.rom_module.is_some() {
            // The BIOS transfers a configured module at power-on:
            // there is no monitor prompt to capture.
            return None;
        }
        let mut machine = Machine::new(self.config.model, &self.monitor_rom, None);
        machine.reset();
        let mut detector = PromptDetect::default();
        let mut to_prompt = |machine: &mut Machine| {
            for _ in 0..BOOT_CAPTURE_MAX_FRAMES {
                machine.step_frame();
                if detector.step(machine) {
                    return true;
                }
            }
            false
        };
        if !to_prompt(&mut machine) {
            log::warn!("boot state not captured: no monitor prompt");
            return None;
        }
        if self.config.model == Model::Pmd853 {
            machine.cpu.pc = 0xFFF0; // `JUMP FFF0`
            if !to_prompt(&mut machine) {
                log::warn!("boot state not captured: no compatibility prompt");
                return None;
            }
        }
        let state = BootState {
            sig: machine.bus.memory.monitor_signature(),
            bytes: machine.save_state().ok()?,
        };
        self.boot_state = Some(state.clone());
        Some(state)
    }

    /// Put the machine into its booted state for an IDE run: restore
    /// the cached snapshot (capturing it first if needed), skipping
    /// the monitor's boot and its memory test. Any module a previous
    /// run attached is ejected first — the run wants the monitor's
    /// own boot state. Returns `false` when no snapshot could be made
    /// or restored; the caller then falls back to a real reset.
    fn restore_booted(&mut self) -> bool {
        self.eject_ide_module();
        if self.config.rom_module.is_some() {
            // A configured module owns the boot; nothing to restore.
            return false;
        }
        let state = match &self.boot_state {
            Some(state) if state.sig == self.machine.bus.memory.monitor_signature() => {
                state.clone()
            }
            _ => match self.capture_boot_state() {
                Some(state) => state,
                None => return false,
            },
        };
        if let Err(e) = self.machine.restore_state(&state.bytes) {
            log::warn!("cannot restore the booted state: {e}");
            return false;
        }
        self.frames_done = 0;
        self.pacer.resync();
        self.tape.play = false;
        self.tape.was_recording = false;
        // The restored machine's cycle counter jumped to the
        // snapshot's timeline; the speaker timeline must follow it
        // (not just start over at 0, which would render the whole
        // gap — minutes of nothing — in one frame).
        if let Some(audio) = &mut self.audio {
            audio.resync(self.machine.bus.total_cycles());
        }
        self.refresh_screen();
        true
    }

    // ----- project -----------------------------------------------------

    /// Open the project in `dir` (replacing any open project), show
    /// the Project tab and build once. Parse problems become a
    /// notification, never a failure to open.
    pub fn open_project(&mut self, dir: &Path) {
        // Dirty buffers wait for the guard; the folder is stashed in
        // the guard and opened once it resolves.
        if self.guard_close(AfterClose::Open(dir.to_path_buf())) {
            return;
        }
        match Project::open(dir) {
            Ok(mut project) => {
                project.build = Some(project.build());
                let name = project.manifest.name.clone();
                self.project = Some(project);
                crate::ui::dock::show_tab(&mut self.ui.dock, crate::ui::dock::Tab::Project);
                self.notify(format!("Project {name:?} opened"));
            }
            Err(e) => self.notify(e),
        }
    }

    /// Create and open a fresh project in `dir`.
    pub fn create_project(&mut self, dir: &Path) {
        let name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("project")
            .to_string();
        if self.guard_close(AfterClose::Create(dir.to_path_buf())) {
            return;
        }
        match Project::create(dir, &name) {
            Ok(mut project) => {
                project.build = Some(project.build());
                self.project = Some(project);
                crate::ui::dock::show_tab(&mut self.ui.dock, crate::ui::dock::Tab::Project);
                self.notify(format!("Project {name:?} created"));
            }
            Err(e) => self.notify(e),
        }
    }

    /// Close the project. Buffers with unsaved edits raise the
    /// unsaved-changes guard first ([`AfterClose::Project`]).
    pub fn close_project(&mut self) {
        if self.guard_close(AfterClose::Project) {
            return;
        }
        self.close_project_now();
    }

    /// Close the project unconditionally (buffers with unsaved edits
    /// are dropped; the files on disk stay as they are).
    fn close_project_now(&mut self) {
        if let Some(project) = &self.project {
            if project.files.iter().any(|f| f.dirty) {
                self.notify("Project closed; unsaved edits discarded");
            }
        }
        self.project = None;
        crate::ui::dock::hide_tab(&mut self.ui.dock, crate::ui::dock::Tab::Project);
        crate::ui::dock::hide_tab(&mut self.ui.dock, crate::ui::dock::Tab::Problems);
    }

    /// Whether closing needs the unsaved-changes guard: buffers with
    /// unsaved edits. If it does, raise the guard for `after` and
    /// return true (the caller must not act yet).
    fn guard_close(&mut self, after: AfterClose) -> bool {
        let dirty = self
            .project
            .as_ref()
            .is_some_and(|p| p.files.iter().any(|f| f.dirty));
        if dirty && self.close_guard.is_none() {
            self.close_guard = Some(after);
        }
        dirty
    }

    /// A window/app close was requested (the system button, the
    /// titlebar X). True when the app should exit now; false when
    /// the unsaved-changes guard went up instead.
    pub(crate) fn request_exit(&mut self) -> bool {
        if self.exit_confirmed {
            return true;
        }
        if self
            .project
            .as_ref()
            .is_some_and(|p| p.files.iter().any(|f| f.dirty))
        {
            if self.close_guard.is_none() {
                self.close_guard = Some(AfterClose::Exit);
                self.ctx.request_repaint();
            }
            return false;
        }
        true
    }

    /// The guard dialog's decision: `save` writes every dirty buffer
    /// first (a failed save keeps the guard up and reports why), then
    /// the pending close happens. Returns true when the guard closed.
    pub(crate) fn resolve_close_guard(&mut self, save: bool) -> bool {
        let Some(after) = self.close_guard.take() else {
            return true;
        };
        if save {
            match self.project.as_mut().map(|p| p.save_all()) {
                Some(Ok(saved)) if !saved.is_empty() => {
                    self.notify(format!("Saved {} file(s)", saved.len()))
                }
                Some(Err(e)) => {
                    // The guard stays up: the user can retry or
                    // discard deliberately.
                    self.notify(e);
                    self.close_guard = Some(after);
                    return false;
                }
                _ => {}
            }
        }
        match after {
            AfterClose::Project => self.close_project_now(),
            AfterClose::Open(dir) => {
                self.close_project_now();
                self.open_project(&dir);
            }
            AfterClose::Create(dir) => {
                self.close_project_now();
                self.create_project(&dir);
            }
            AfterClose::Exit => {
                // The queued window close goes through
                // [`Self::request_exit`], which now passes.
                self.exit_confirmed = true;
                self.request_window(WindowRequest::Close);
            }
        }
        true
    }

    /// The guard dialog's Cancel: nothing closes, nothing is saved.
    pub(crate) fn cancel_close_guard(&mut self) {
        self.close_guard = None;
    }

    /// Build the project: assemble the current buffers,
    /// keep the result for the problems panel and the run commands.
    /// Returns whether the build succeeded.
    pub fn build_project(&mut self) -> bool {
        let Some(project) = self.project.as_mut() else {
            return false;
        };
        let build = project.build();
        let ok = !build.has_errors();
        project.build = Some(build);
        if ok {
            self.notify("Build OK");
        } else {
            let count = project
                .build
                .as_ref()
                .map(|b| b.asm.diagnostics.len())
                .unwrap_or(0);
            self.notify(format!("Build failed: {count} problem(s)"));
            crate::ui::dock::show_tab(&mut self.ui.dock, crate::ui::dock::Tab::Problems);
        }
        ok
    }

    /// Save the active source buffer.
    pub fn save_project_file(&mut self) {
        let Some(project) = self.project.as_mut() else {
            return;
        };
        match project.save_active() {
            Ok(path) => self.notify(format!("Saved {path}")),
            Err(e) => self.notify(e),
        }
    }

    /// The current build's artifact for the configured target — what
    /// the runs load, the Export dialog saves and the `release/`
    /// folder holds. The same target constraints the runs check are
    /// checked here (with the same notifications), so an artifact
    /// that exists is one the run can use. `None` when nothing is
    /// built, the build produced no code, or the target's
    /// constraints are not met.
    fn project_artifact(&mut self) -> Option<Artifact> {
        let built = self
            .project
            .as_ref()
            .and_then(|p| p.build.as_ref())
            .and_then(|b| {
                b.asm
                    .flat_image(0)
                    .map(|(base, image)| (base, image, b.asm.entry))
            });
        let Some((base, image, entry_hint)) = built else {
            self.notify("Nothing was assembled: no code emitted");
            return None;
        };
        let target = self.project.as_ref().map(|p| p.manifest.target.clone())?;
        let top = base.wrapping_add(image.len() as u16);
        let entry = entry_hint
            .filter(|&e| (base..top).contains(&e))
            .unwrap_or(base);
        match target {
            crate::project::Target::Tape { name, autostart } => {
                // With autostart, the tape carries the wiki's autorun
                // pair: a '?' file whose body is the small loader
                // saved at 0x7FD7 (its last word overwrites the block
                // reader's return slot at 0x7FFB), followed by a
                // headerless continuation with the program itself.
                // Without autostart, a plain '?' block that MGLD loads
                // and leaves in memory. (The monitors' MGLD expects
                // the conventional '?' type — the block-type variable
                // C136 is initialized to 0x3F at warm start and no
                // command ever changes it.)
                let blocks: Vec<pmd85_core::tape::TapeBlock> = if autostart {
                    // The loader lives at 0x7FD7..=0x7FFC; the program
                    // must not overlap it (it would be loaded over
                    // the loader's own return-slot word).
                    if base <= 0x7FFC && top > 0x7FD7 && base <= top {
                        self.notify(format!(
                            "Autostart needs 0x7FD7..0x7FFC free, but the program spans {base:04X}..{top:04X}"
                        ));
                        return None;
                    }
                    let loader = pmd85_core::tape::autorun_loader(
                        self.machine.bus.model,
                        base,
                        entry,
                        image.len() as u16,
                    );
                    match pmd85_core::tape::make_file(0, b'?', &name, 0x7FD7, &loader) {
                        Ok(block) => vec![block, pmd85_core::tape::make_continuation(&image)],
                        Err(e) => {
                            self.notify(format!("Cannot build the tape file: {e}"));
                            return None;
                        }
                    }
                } else {
                    match pmd85_core::tape::make_file(0, b'?', &name, base, &image) {
                        Ok(block) => vec![block],
                        Err(e) => {
                            self.notify(format!("Cannot build the tape file: {e}"));
                            return None;
                        }
                    }
                };
                Some(Artifact::Tape { blocks })
            }
            crate::project::Target::RomModule => {
                // The payload must live in the RAM below the
                // monitors' boot-time stack (they run the header with
                // return addresses just under 0x8000), and the image
                // must fit the 32 KB module the connector addresses.
                if top > 0x7FFB || base > top {
                    self.notify(format!(
                        "ROM module needs the program inside 0000h..7FFBh, but it spans {base:04X}..{top:04X}"
                    ));
                    return None;
                }
                if image.len() + pmd85_core::module::HEADER_LEN > 0x8000 {
                    self.notify("ROM module images are limited to 32 KB");
                    return None;
                }
                // On the 85-1 the header executes at RAM 0 (the BASIC
                // transfer's first stage lands it there): the payload
                // must not overwrite it mid-copy.
                if self.machine.bus.model == pmd85_core::Model::Pmd851 && base < 0x10 {
                    self.notify(
                        "The 85-1's module transfer needs the program above 0010h (its header runs at RAM 0)",
                    );
                    return None;
                }
                let module = pmd85_core::module::make_rom_module(
                    self.machine.bus.model,
                    &image,
                    base,
                    entry,
                );
                Some(Artifact::Rom { module, entry })
            }
            crate::project::Target::Bin => Some(Artifact::Bin { image }),
        }
    }

    /// The artifact plus the file name it is exported as: the
    /// project's name (sanitized for a file system) with the
    /// target's extension.
    fn artifact_and_name(&mut self) -> Option<(String, Artifact)> {
        let artifact = self.project_artifact()?;
        let stem = self
            .project
            .as_ref()
            .map(|p| {
                let name: String = p
                    .manifest
                    .name
                    .chars()
                    .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                    .collect();
                if name.is_empty() {
                    "program".to_string()
                } else {
                    name
                }
            })
            .unwrap_or_else(|| "program".to_string());
        let ext = match artifact {
            Artifact::Tape { .. } => "ptp",
            Artifact::Rom { .. } => "rmm",
            Artifact::Bin { .. } => "bin",
        };
        Some((format!("{stem}.{ext}"), artifact))
    }

    /// Write the artifact to `path`, in the target's format.
    fn write_artifact(&self, path: &Path, artifact: &Artifact) -> Result<(), String> {
        match artifact {
            Artifact::Tape { blocks } => {
                let tape = pmd85_core::tape::Tape {
                    blocks: blocks.clone(),
                    warnings: Vec::new(),
                };
                pmd85_core::tape::save(path, &tape).map_err(|e| format!("{}: {e}", path.display()))
            }
            Artifact::Rom { module, .. } => {
                std::fs::write(path, module).map_err(|e| format!("{}: {e}", path.display()))
            }
            Artifact::Bin { image, .. } => {
                std::fs::write(path, image).map_err(|e| format!("{}: {e}", path.display()))
            }
        }
    }

    /// The Build button: assemble, and on success also write the
    /// artifact into the project's `release/` folder (created on
    /// demand) — a build you can hand around.
    pub fn build_and_release_project(&mut self) -> bool {
        let ok = self.build_project();
        if ok {
            if let Some(path) = self.write_release() {
                self.notify(format!("Release: {}", path.display()));
            }
        }
        ok
    }

    /// Write the current build's artifact into `<project>/release/`,
    /// overwriting what is there. Returns the path written.
    fn write_release(&mut self) -> Option<PathBuf> {
        let (name, artifact) = self.artifact_and_name()?;
        let dir = self.project.as_ref().map(|p| p.dir.join("release"))?;
        let path = dir.join(name);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.notify(format!("{}: {e}", dir.display()));
            return None;
        }
        match self.write_artifact(&path, &artifact) {
            Ok(()) => Some(path),
            Err(e) => {
                self.notify(e);
                None
            }
        }
    }

    /// Export…: build, then a save dialog writing the artifact — a
    /// `.ptp` tape, a `.rmm` ROM module or a raw `.bin`, per the
    /// project's target.
    pub fn export_project(&mut self) {
        if !self.build_project() {
            return;
        }
        let Some((name, artifact)) = self.artifact_and_name() else {
            return;
        };
        let (filter_name, ext): (&str, &str) = match &artifact {
            Artifact::Tape { .. } => ("PMD tape image", "ptp"),
            Artifact::Rom { .. } => ("PMD ROM module", "rmm"),
            Artifact::Bin { .. } => ("Raw binary", "bin"),
        };
        if let Some(mut path) = rfd::FileDialog::new()
            .add_filter(filter_name, &[ext])
            .set_file_name(&name)
            .save_file()
        {
            if path.extension().is_none() {
                path = path.with_extension(ext);
            }
            match self.write_artifact(&path, &artifact) {
                Ok(()) => self.notify(format!("Exported {}", path.display())),
                Err(e) => self.notify(e),
            }
        }
    }

    /// Run the project, the authentic way: the machine goes to
    /// its booted state (the cached snapshot when possible — the
    /// boot's memory test skipped — a real reset otherwise), the
    /// built file plays into the (flash) tape deck and the monitor's
    /// load command `MGLD` is typed into the emulated keyboard. The
    /// monitor reads the header, flash-loads the blocks, checksums
    /// and — with the autostart target — the loader block hijacks the
    /// reader's return slot and runs the program, all for real.
    pub fn run_project(&mut self) {
        if !self.build_project() {
            return;
        }
        let Some(artifact) = self.project_artifact() else {
            return;
        };
        // The tape arm needs the target's name back; the others
        // carry everything in the artifact.
        let target = self.project.as_ref().map(|p| p.manifest.target.clone());
        match (artifact, target) {
            (Artifact::Tape { blocks }, Some(crate::project::Target::Tape { name, autostart })) => {
                // A clean machine at the monitor's command prompt for
                // the typed MGLD to land: the cached booted state when
                // possible (memory test skipped), a real reset
                // otherwise.
                let booted = self.restore_booted();
                if !booted {
                    self.machine.reset();
                    self.frames_done = 0;
                }
                // The built blocks play straight into the deck as a
                // session — the tape editor (and the saved session)
                // stay untouched, so repeated runs do not pile files
                // onto the tape.
                let flash = self.tape.flash;
                let mut items: Vec<PlayItem> = Vec::new();
                for block in &blocks {
                    if block.header.is_some() {
                        items.push(PlayItem {
                            data: block.header_bytes.clone(),
                            head: true,
                            flash,
                        });
                    }
                    items.push(PlayItem {
                        data: block.body_bytes.clone(),
                        head: false,
                        flash,
                    });
                }
                // The monitor's load command, with our file number
                // (0): MGLD 00. The tape session starts when the
                // auto-typer's last key (the Enter of the command)
                // goes down — the flash body would burn its waiting
                // budget if it started during the boot.
                use pmd85_core::keyboard::Key as key;
                let mut script = vec![];
                if autostart && self.machine.bus.model == pmd85_core::Model::Pmd853 && !booted {
                    // Monitor 3 keeps its stack at 0xBF00, out of the
                    // autorun return slot's (0x7FFB) reach — but its
                    // `JUMP FFF0` switches it into the 85-2
                    // compatibility mode: a relocated copy at 0x8000
                    // and a fresh boot, after which the whole 85-2
                    // convention (the 7FFB slot, the reader at 8DC2)
                    // holds verbatim. Only needed without the cached
                    // state — the snapshot already sits in the
                    // compatibility mode.
                    script.extend(
                        [
                            key::J,
                            key::U,
                            key::M,
                            key::P,
                            key::Space,
                            key::F,
                            key::F,
                            key::F,
                            key::Digit0,
                        ]
                        .map(AutoTypeItem::Press),
                    );
                    script.push(AutoTypeItem::Press(key::Enter));
                    script.push(AutoTypeItem::WaitPrompt);
                }
                script.extend(
                    [
                        key::M,
                        key::G,
                        key::L,
                        key::D,
                        key::Space,
                        key::Digit0,
                        key::Digit0,
                        key::Enter,
                    ]
                    .map(AutoTypeItem::Press),
                );
                let session = (!items.is_empty()).then(|| RunSession {
                    items,
                    flash,
                    name: name.clone(),
                });
                self.auto_type = Some(AutoType::new(&script, session));
                self.set_running(true);
            }
            // The direct run rebuilds and injects by itself.
            (Artifact::Bin { .. }, _) => self.run_project_direct(),
            (Artifact::Rom { module, entry }, _) => {
                // The authentic module flow: the image is plugged
                // into the connector and the machine rebooted. The
                // monitor BIOS detects the module and transfers the
                // payload by itself; on the 85-1 it needs the
                // `BASIC G` command instead (Monitor 1 has no
                // boot-time detection). The constraints were checked
                // when the artifact was built.
                let model = self.machine.bus.model;
                self.ide_module = Some(module.clone());
                self.machine.bus.memory.rom_module = Some(module);
                self.machine.reset();
                self.frames_done = 0;
                if model == pmd85_core::Model::Pmd851 {
                    use pmd85_core::keyboard::Key as key;
                    self.auto_type = Some(AutoType::new(
                        &[
                            key::B,
                            key::A,
                            key::S,
                            key::I,
                            key::C,
                            key::Space,
                            key::G,
                            key::Enter,
                        ]
                        .map(AutoTypeItem::Press),
                        None,
                    ));
                }
                self.set_running(true);
                self.notify(format!("ROM module attached - booting from {entry:04X}"));
            }
            // A tape artifact with a non-tape target cannot happen
            // (the artifact is built from the target).
            _ => {}
        }
    }

    /// Run the project directly: inject the built image
    /// into RAM at its address and start the CPU at the entry point.
    /// No monitor, no tape — the fast inner loop for development.
    /// The machine starts from the cached booted state when possible
    /// (so the monitor's RAM variables are initialized and monitor
    /// routines are callable); a bare reset otherwise, with the
    /// boot-time shadow map dropped — until the monitor's first I/O
    /// write the ROM is mirrored over the RAM the program lives in,
    /// and an inject into that map is unreadable: the CPU would run
    /// the ROM instead of the program.
    pub fn run_project_direct(&mut self) {
        if !self.build_project() {
            return;
        }
        let Some(project) = self.project.as_ref() else {
            return;
        };
        let Some(build) = project.build.as_ref() else {
            return;
        };
        let Some((base, image)) = build.asm.flat_image(0) else {
            self.notify("Nothing was assembled: no code emitted");
            return;
        };
        let entry = build
            .asm
            .entry
            .filter(|&e| (base..base.wrapping_add(image.len() as u16)).contains(&e))
            .unwrap_or(base);
        if !self.restore_booted() {
            self.machine.reset();
            self.frames_done = 0;
            // The same thing the monitor's first I/O write does —
            // without it the injected image cannot be read back.
            self.machine.bus.memory.note_io_write();
        }
        for (i, byte) in image.iter().enumerate() {
            self.machine
                .bus
                .memory
                .write(base.wrapping_add(i as u16), *byte);
        }
        self.machine.cpu.pc = entry;
        // A monitor load typed by an earlier Run would keep typing
        // into the program — a direct run owns the machine now.
        self.auto_type = None;
        self.set_running(true);
        self.notify(format!("Running from {:04X} (direct)", entry));
    }

    /// One render frame of the auto-typer: wait for the monitor's
    /// command prompt, then hold each key a few frames (long enough
    /// for the monitor's keyboard scan), release, gap, next key.
    /// The tape session (if any) starts when the final key — the
    /// Enter of the command — goes down: the monitor's MGLD dispatch
    /// happens on that key press (after the key-release wait), so the
    /// leader is already rolling by the time the ROM starts reading.
    pub(crate) fn pump_auto_type(&mut self) {
        let mut start_play = None;
        {
            let Some(auto) = self.auto_type.as_mut() else {
                return;
            };
            if auto.waiting {
                // The monitor's command loop cycles the keyboard
                // scan column many times per frame; a change between
                // consecutive frames means it is up and polling.
                let prompt = auto.detector.step(&self.machine);
                auto.waited += 1;
                if prompt || auto.waited > AUTOTYPE_TIMEOUT {
                    auto.waiting = false;
                    auto.phase = AUTOTYPE_SETTLE;
                } else {
                    return;
                }
            }
        }
        let Some(auto) = self.auto_type.as_mut() else {
            return;
        };
        if auto.phase > 0 {
            auto.phase -= 1;
            if auto.phase == 2 {
                // Release before the gap so each press is a clean
                // edge, not one long key-down.
                if let Some(key) = auto.current.take() {
                    self.machine.bus.keyboard.set_key(key, false);
                }
            }
        } else if let Some(item) = auto.queue.pop_front() {
            match item {
                AutoTypeItem::Press(key) => {
                    self.machine.bus.keyboard.set_key(key, true);
                    auto.current = Some(key);
                    auto.phase = 6;
                    if auto.queue.is_empty() {
                        // The last key (Enter) just went down: start
                        // the tape now, so its leader plays while the
                        // monitor finishes the key-release wait and
                        // dispatches.
                        start_play = auto.play_on_start.take();
                    }
                }
                AutoTypeItem::WaitPrompt => {
                    // The previous command reboots the machine (the
                    // 85-3's compatibility switch): wait for its
                    // prompt before typing the rest.
                    auto.waiting = true;
                    auto.waited = 0;
                    auto.detector = PromptDetect::default();
                }
            }
        } else {
            self.auto_type = None;
        }
        if let Some(session) = start_play {
            self.tape.play_run(session.items, &mut self.machine);
            if session.flash {
                self.notify(format!("Tape: flash-loading {}", session.name));
            }
            let _ = self.tape.pump(&mut self.machine);
        }
    }

    // ----- save states -------------------------------------------------

    /// Whether a save state can be taken right now: the cassette deck
    /// must be idle (states never cover a running tape session).
    pub fn can_save_state(&self) -> bool {
        !self.machine.bus.tape.is_active()
    }

    /// Serialize the machine into `path` as a save state. Fails while
    /// the tape plays or records (see [`App::can_save_state`]).
    pub fn save_state_to(&mut self, path: &Path) -> Result<(), String> {
        let bytes = self
            .machine
            .save_state()
            .map_err(|e| format!("State: {e}"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("State save failed: {e}"))?;
        }
        std::fs::write(path, &bytes).map_err(|e| format!("State save failed: {e}"))
    }

    /// Restore the machine from a save state at `path`. A tape session
    /// currently running is discarded (a snapshot can only ever have
    /// been taken with the deck idle) and the screen repaints, also
    /// while paused.
    pub fn load_state_from(&mut self, path: &Path) -> Result<(), String> {
        let bytes = std::fs::read(path).map_err(|e| format!("State load failed: {e}"))?;
        self.machine
            .restore_state(&bytes)
            .map_err(|e| format!("State: {e}"))?;
        self.tape.play = false;
        self.tape.was_recording = false;
        // The restored machine's cycle counter jumped to the snapshot's
        // timeline; the speaker timeline must follow it, or every edge
        // until the counter catches up would fold into one frame.
        if let Some(audio) = &mut self.audio {
            audio.resync(self.machine.bus.total_cycles());
        }
        self.refresh_screen();
        Ok(())
    }

    /// The quick-save slot next to the persisted settings.
    fn quick_state_path(&self) -> Option<PathBuf> {
        self.settings_dir
            .as_ref()
            .map(|dir| dir.join("states").join("quick.pss"))
    }

    /// Quick save (F5): overwrite the quick slot without a dialog.
    pub fn quick_save_state(&mut self) {
        let Some(path) = self.quick_state_path() else {
            return;
        };
        match self.save_state_to(&path) {
            Ok(()) => self.notify("State saved (quick slot)"),
            Err(e) => self.notify(e),
        }
    }

    /// Quick load (F9): restore the quick slot without a dialog.
    pub fn quick_load_state(&mut self) {
        let Some(path) = self.quick_state_path() else {
            return;
        };
        match self.load_state_from(&path) {
            Ok(()) => self.notify("State restored (quick slot)"),
            Err(e) => self.notify(e),
        }
    }

    // ----- session (remembered across restarts) -----------------------

    /// Where the session snapshot lives: `session/` next to the
    /// persisted settings.
    fn session_dir(&self) -> Option<PathBuf> {
        self.settings_dir.as_ref().map(|dir| dir.join("session"))
    }

    /// Remember the current session: the tape editor's contents
    /// (unsaved edits included), its file path, dirty flag and
    /// selection. Written when the app exits; never fatal.
    pub fn save_session(&self) {
        let Some(dir) = self.session_dir() else {
            return;
        };
        if let Err(e) = std::fs::create_dir_all(&dir)
            .and_then(|()| pmd85_core::tape::save(&dir.join("tape.ptp"), &self.tape.tape))
        {
            log::warn!("cannot write the session tape: {e}");
            return;
        }
        crate::config::SessionState {
            tape_path: self.tape.path.clone(),
            tape_dirty: self.tape.dirty,
            selected: self.tape.selected,
        }
        .save(&dir);
    }

    /// Restore the session remembered by [`App::save_session`]: the
    /// tape comes back exactly as it was, unsaved edits included. A
    /// missing or corrupt snapshot is a clean start (logged, never
    /// fatal); a snapshot that parses with warnings reports them like
    /// opening a tape would.
    pub fn restore_session(&mut self) {
        let Some(dir) = self.session_dir() else {
            return;
        };
        let session = crate::config::SessionState::load(&dir);
        match pmd85_core::tape::load(&dir.join("tape.ptp")) {
            Ok(tape) => {
                let warnings = tape.warnings.join("; ");
                self.tape.tape = tape;
                self.tape.path = session.tape_path;
                self.tape.dirty = session.tape_dirty;
                self.tape.selected = session.selected;
                if !warnings.is_empty() {
                    self.notify(format!("Tape: {warnings}"));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log::warn!("cannot read the session tape: {e}"),
        }
    }

    // ----- dock layout (remembered across restarts) ------------------

    /// Where the dock layout lives: `layout.json` next to the
    /// persisted settings.
    pub(crate) fn layout_path(&self) -> Option<PathBuf> {
        self.settings_dir
            .as_ref()
            .map(|dir| dir.join("layout.json"))
    }

    /// Remember the dock layout: which panels are visible and where
    /// they are docked (including floating windows), plus the window
    /// geometry under its own key. Written when the app exits; never
    /// fatal.
    pub fn save_layout(&self) {
        let Some(path) = self.layout_path() else {
            return;
        };
        let mut json = match serde_json::to_value(&self.ui.dock) {
            Ok(json) => json,
            Err(e) => {
                log::warn!("cannot serialize the dock layout: {e}");
                return;
            }
        };
        // The window geometry rides along under its own key. The dock
        // deserializer ignores unknown keys, so both directions are
        // compatible: old files have no window, and old builds
        // reading new files skip it.
        if let serde_json::Value::Object(map) = &mut json {
            if let Ok(window) = serde_json::to_value(self.window_layout()) {
                map.insert("window".into(), window);
            }
        }
        sanitize_layout(&mut json);
        if let Err(e) = std::fs::write(&path, json.to_string()) {
            log::warn!("cannot write the dock layout: {e}");
        }
    }

    /// Restore the dock layout saved by [`App::save_layout`]. A
    /// missing, corrupt or screen-less layout falls back to the
    /// default (the lone screen) — never fatal.
    pub fn restore_layout(&mut self) {
        let Some(path) = self.layout_path() else {
            return;
        };
        let parsed = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .map(|mut json| {
                sanitize_layout(&mut json);
                // The window geometry rides along under its own key
                // (see [`App::save_layout`]); take it before the dock
                // parse, which ignores it.
                let window = json
                    .get("window")
                    .cloned()
                    .and_then(|w| serde_json::from_value::<WindowLayout>(w).ok())
                    .map(WindowLayout::sanitized);
                let dock =
                    serde_json::from_value::<egui_dock::DockState<crate::ui::dock::Tab>>(json);
                (window, dock)
            });
        let Some((window, dock)) = parsed else {
            log::info!("dock layout not restored; using the default");
            return;
        };
        if let Some(window) = window {
            self.window_size = egui::vec2(window.size[0], window.size[1]);
            self.window_start_maximized = window.maximized;
        }
        match dock.ok() {
            Some(dock) if dock.find_tab(&crate::ui::dock::Tab::Screen).is_some() => {
                self.ui.dock = dock;
            }
            _ => log::info!("dock layout not restored; using the default"),
        }
    }

    /// The window geometry to remember for the next start: the last
    /// size seen while not maximized, and whether the window is
    /// maximized now.
    fn window_layout(&self) -> WindowLayout {
        WindowLayout {
            size: [self.window_size.x, self.window_size.y],
            maximized: self.window_maximized,
        }
    }

    // ----- emulation -----

    /// Debugger: execute exactly one instruction. The transport
    /// pauses for it (a running machine would immediately race past
    /// the step).
    pub fn debug_step(&mut self) {
        self.set_running(false);
        self.machine.step_once();
    }

    /// Debugger: step over a CALL (or a single instruction).
    pub fn debug_step_over(&mut self) {
        self.set_running(false);
        self.machine.step_over();
    }

    /// Debugger: continue — re-arm breakpoints after a hit and run.
    /// Away from a breakpoint it is a plain Run.
    pub fn debug_continue(&mut self) {
        if self.machine.breakpoint_hit().is_some() {
            self.machine.resume();
        }
        self.set_running(true);
    }

    /// Run `frames` emulated frames, pumping the tape deck between
    /// them. The pump must run per *emulated* frame, not per rendered
    /// frame: at a speedup several emulated frames pass per render
    /// frame, and the deck's block transitions (and the session
    /// bookkeeping around them) must stay in lockstep with the
    /// machine or loads corrupt.
    pub(crate) fn run_frames(&mut self, frames: u64) {
        for _ in 0..frames {
            self.machine.step_frame();
            if self.machine.breakpoint_hit().is_some() {
                // A breakpoint stopped the machine mid-frame: stop
                // pumping the tape deck (it must stay in lockstep
                // with the machine) and pause the transport.
                self.running = false;
                self.notify(format!(
                    "Breakpoint at {:04X}",
                    self.machine.breakpoint_hit().unwrap()
                ));
                return;
            }
            if let Some(message) = self.tape.pump(&mut self.machine) {
                self.notify(message);
            }
        }
    }

    /// Run the emulation for this render frame, feed the speaker, and
    /// update the screen texture. Called once per render frame.
    pub fn advance(&mut self) {
        // Gamepads first: pump events, merge the pads' buttons with
        // any the Joystick panel holds via mouse, and feed the
        // emulated sticks (level-based, so pause and turbo keep the
        // state fresh without losing presses).
        for message in self.gamepads.poll() {
            self.notify(message);
        }
        self.apply_joysticks();

        // The project Run's auto-typed command: only while running
        // (a paused machine would not scan the keyboard anyway).
        if self.running {
            self.pump_auto_type();
        }

        let frames = if self.running {
            if self.speed.turbo {
                let start = Instant::now();
                let mut frames = 0u64;
                while start.elapsed() < TURBO_BUDGET {
                    self.run_frames(1);
                    frames += 1;
                }
                frames
            } else {
                let frames = self.pacer.take_frames(self.speed.multiplier) as u64;
                self.run_frames(frames);
                frames
            }
        } else {
            // While paused keep the pacing anchored so resuming does
            // not fast-forward through the pause.
            self.pacer.resync();
            if let Some(message) = self.tape.pump(&mut self.machine) {
                self.notify(message);
            }
            0
        };
        self.frames_done += frames;

        // Speaker (and the tape data-tone monitor, when enabled):
        // only at exactly 1x real time; otherwise drain (the edge logs
        // are bounded either way).
        let edges = self.machine.take_speaker_edges();
        let tape_edges = self.machine.bus.tape.take_monitor_edges();
        if self.wants_audio() {
            if let Some(audio) = &mut self.audio {
                if self.settings.tape_monitor {
                    audio.submit_mixed(edges, tape_edges, self.machine.bus.total_cycles());
                } else {
                    audio.submit(edges, self.machine.bus.total_cycles());
                }
            }
        }

        // Screen texture: decode VRAM and upload.
        self.refresh_screen();
    }

    /// Decode the current VRAM into the screen texture. Runs every
    /// emulated frame; also called directly after loading a save state
    /// so a paused machine repaints immediately.
    fn refresh_screen(&mut self) {
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [SCREEN.0, SCREEN.1],
            vram::decode_into(&self.machine.bus.memory, self.profile, &mut self.decode_buf),
        );
        match &mut self.screen {
            Some(tex) => tex.set_partial([0, 0], image, egui::TextureOptions::NEAREST),
            None => {
                self.screen = Some(self.ctx.load_texture(
                    "pmd85-screen",
                    image,
                    egui::TextureOptions::NEAREST,
                ));
            }
        }
    }

    /// Feed the emulated joysticks: every pad assigned to a connector
    /// is ORed in (so two pads can share GPIO/0), plus a button held
    /// by mouse in the Joystick panel. Disabled means a released
    /// stick, and so does an unassigned pad.
    fn apply_joysticks(&mut self) {
        let mut ports = [0u8; 2];
        if self.settings.joystick_enabled {
            for (uuid, pad) in self.gamepads.pads() {
                let port = self
                    .settings
                    .gamepad_ports
                    .get(uuid)
                    .copied()
                    .unwrap_or_default();
                if let Some(connector) = port.connector() {
                    ports[connector] |= pad.mask;
                }
            }
            if let Some((connector, button)) = self.ui.mouse_joystick {
                ports[connector] |= button.bit();
            }
        }
        self.machine.bus.joysticks[0].set_mask(ports[0]);
        self.machine.bus.joysticks[1].set_mask(ports[1]);
    }

    /// Whether speaker audio should be submitted this frame: only at
    /// exactly 1x real time (otherwise tones would play at the wrong
    /// pitch relative to wall time).
    pub fn wants_audio(&self) -> bool {
        self.running
            && !self.settings.mute
            && !self.speed.turbo
            && (self.speed.multiplier - 1.0).abs() < 1e-9
    }

    // ----- pacing for the event loop -----

    /// Whether the emulator wants a redraw right now (frame due).
    ///
    /// Also true while paused/stopped: the event loop then wakes lazily
    /// (~10 fps, plus on every input event), and those repaints keep
    /// the UI responsive so the machine can be unpaused again.
    pub fn wants_redraw(&self, now: Instant) -> bool {
        if !self.running {
            return true;
        }
        self.speed.turbo || now >= self.pacer.next_due()
    }

    /// When the event loop should wake up next.
    pub fn next_wake(&self, now: Instant) -> Instant {
        if self.running {
            if self.speed.turbo {
                now
            } else {
                self.pacer.next_due()
            }
        } else {
            // Paused/stopped: lazy UI refresh.
            now + Duration::from_millis(100)
        }
    }

    /// Bookkeeping after presenting a frame (fps meters).
    pub fn on_frame_presented(&mut self) {
        let now = Instant::now();
        while self
            .render_times
            .front()
            .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(1))
        {
            self.render_times.pop_front();
        }
        self.render_times.push_back(now);

        self.frame_samples.push_back((now, self.frames_done));
        while self
            .frame_samples
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_secs(1))
        {
            self.frame_samples.pop_front();
        }

        // Expire notifications.
        self.notifications
            .retain(|(_, t)| now.duration_since(*t) < NOTIFICATION_TTL);
    }

    /// Rendered frames per second (UI).
    pub fn render_fps(&self) -> f64 {
        match (self.render_times.front(), self.render_times.back()) {
            (Some(first), Some(last)) if first != last => {
                let secs = last.duration_since(*first).as_secs_f64().max(1e-9);
                (self.render_times.len() - 1) as f64 / secs
            }
            _ => 0.0,
        }
    }

    /// Emulated frames per second (averaged over the last second).
    pub fn emulated_fps(&self) -> f64 {
        let (Some((t0, f0)), Some((t1, f1))) = (
            self.frame_samples.front().copied(),
            self.frame_samples.back().copied(),
        ) else {
            return 0.0;
        };
        let frames = f1.saturating_sub(f0) as f64;
        let secs = t1.duration_since(t0).as_secs_f64().max(1e-9);
        frames / secs
    }

    /// Emulated wall time since (re)start, as (h, m, s).
    pub fn emulated_time(&self) -> (u64, u64, u64) {
        let secs = self.machine.bus.total_cycles() / CPU_CLOCK_HZ;
        (secs / 3600, (secs / 60) % 60, secs % 60)
    }

    // ----- misc -----

    pub fn notify(&mut self, message: impl Into<String>) {
        let message: String = message.into();
        log::warn!("{message}");
        self.notifications.push_back((message, Instant::now()));
        // Keep the queue bounded even if nothing expires.
        while self.notifications.len() > 5 {
            self.notifications.pop_front();
        }
    }

    pub fn notifications(&self) -> &VecDeque<(String, Instant)> {
        &self.notifications
    }

    /// Set and apply a theme by index in the theme list.
    pub fn set_theme(&mut self, index: usize) {
        if let Some(theme) = self.themes.get(index).cloned() {
            self.active_theme = theme.name.clone();
            self.settings.theme = theme.name.clone();
            theme.apply(&self.ctx);
            self.persist();
        }
    }

    /// The currently applied theme (clone; used by UI panels that
    /// need direct palette access).
    pub fn active_theme_data(&self) -> Theme {
        self.themes
            .iter()
            .find(|t| t.name == self.active_theme)
            .cloned()
            .unwrap_or_default()
    }

    /// Apply and remember a modified theme (in-place customization).
    /// The theme replaces any same-name entry in the theme list so the
    /// settings picker reflects the change.
    pub fn apply_theme(&mut self, theme: Theme) {
        if let Some(slot) = self.themes.iter_mut().find(|t| t.name == theme.name) {
            *slot = theme.clone();
        } else {
            self.themes.push(theme.clone());
        }
        self.active_theme = theme.name.clone();
        self.settings.theme = theme.name.clone();
        theme.apply(&self.ctx);
        self.persist();
    }

    /// Write the persisted settings (model, mute, theme, speed...).
    pub fn persist(&self) {
        if let Some(dir) = &self.settings_dir {
            self.settings.save(dir);
        }
    }

    /// Queue a window-management request for the winit loop (the UI
    /// cannot reach the window itself).
    pub(crate) fn request_window(&mut self, request: WindowRequest) {
        self.window_requests.push(request);
    }

    /// Take the pending window requests (the winit loop applies them
    /// after each frame).
    pub(crate) fn take_window_requests(&mut self) -> Vec<WindowRequest> {
        std::mem::take(&mut self.window_requests)
    }

    /// Point persisted settings at `dir` (tests write to a scratch
    /// directory instead of the user's real config).
    #[cfg(test)]
    pub(crate) fn set_settings_dir(&mut self, dir: PathBuf) {
        self.settings_dir = Some(dir);
    }
}

/// Default ROM directory: the repo's `Rom/` next to the crate.
pub fn default_rom_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Rom")
}
