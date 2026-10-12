//! Immediate-mode UI: panels drawn from the [`App`] state each frame.
//!
//! Each panel is a standalone `draw(ctx, app)` function so panels can
//! later move into an `egui_dock` tree unchanged.

pub mod controls;
pub mod debug;
pub mod dock;
pub mod joystick;
pub mod keyboard;
pub mod problems;
pub mod project;
pub mod screen;
pub mod settings;
pub mod status;
pub mod tape;
pub mod theme;
pub mod titlebar;

use crate::app::App;

/// UI-only state (windows, dialogs), distinct from emulator state.
#[derive(Debug)]
pub struct UiState {
    /// The settings window (a floating dialog).
    pub settings_open: bool,
    /// The dock layout: which panels are visible and where they are
    /// docked. Replaces the former per-window `*_open` flags — the
    /// tree itself is the visibility state, and it persists in
    /// `layout.json`.
    pub dock: egui_dock::DockState<dock::Tab>,
    /// Debugger view state (disassembly anchor, memory dump).
    pub debug: crate::ui::debug::DebugState,
    /// Metadata entry form for a pending tape import.
    pub tape_import: Option<crate::ui::tape::ImportDraft>,
    /// A configuration awaiting the "restart machine?" confirmation.
    pub confirm_reboot: Option<crate::app::MachineConfig>,
    /// The color-customization expander in the settings window.
    pub theme_customize: bool,
    /// Unsaved working copy in the color editor (theme name + palette).
    pub theme_draft: Option<theme::Theme>,
    /// Emulated key currently held down via a click in the keyboard
    /// view (released when the pointer button goes up).
    pub mouse_key: Option<pmd85_core::keyboard::Key>,
    /// Whether the keyboard view's RST cap is pointer-held.
    pub mouse_rst: bool,
    /// Joystick button currently held down via a click in the
    /// joystick view, and which connector's stick it belongs to
    /// (released when the pointer button goes up). Merged with the
    /// gamepad state each frame by [`crate::app::App::advance`].
    pub mouse_joystick: Option<(usize, pmd85_core::joystick::Button)>,
    /// A jump the editor applies on its next draw (set by the
    /// Problems tab, among others).
    pub project_jump: Option<crate::ui::project::Jump>,
}

impl Default for UiState {
    fn default() -> Self {
        UiState {
            settings_open: false,
            dock: dock::default_dock(),
            debug: crate::ui::debug::DebugState::default(),
            tape_import: None,
            confirm_reboot: None,
            theme_customize: false,
            theme_draft: None,
            mouse_key: None,
            mouse_rst: false,
            mouse_joystick: None,
            project_jump: None,
        }
    }
}

/// Draw the whole UI for one frame (between `begin_pass`/`end_pass`).
/// `ui` is the root Ui covering the window.
pub fn draw(app: &mut App, ui: &mut egui::Ui) {
    // The custom titlebar replaces the system window frame when the
    // setting is on; the system frame is drawn exactly when it is
    // not (set at window creation, switched live by the setting).
    if app.settings.custom_titlebar {
        titlebar::draw(ui, app);
    }
    controls::draw(ui, app);
    status::draw(ui, app);
    // Everything between the bars is the dock area; the screen is its
    // non-closable center tab, the tools dock around it.
    dock::draw(ui, app);
    // The resize border overlays the window edges, above everything
    // except floating windows (settings, notifications) and popups.
    if app.settings.custom_titlebar {
        titlebar::border(ui, app);
    }
    let ctx = ui.ctx().clone();
    settings::draw(&ctx, app);
    draw_notifications(&ctx, app);
    // The unsaved-changes guard sits above everything: it can be
    // raised by the window close, not just from the Project tab.
    project::close_guard_modal(&ctx, app);
}

/// Transient error popups, bottom-right.
fn draw_notifications(ctx: &egui::Context, app: &mut App) {
    if app.notifications().is_empty() {
        return;
    }
    egui::Area::new(egui::Id::new("notifications"))
        .anchor(egui::Align2::RIGHT_BOTTOM, [-8.0, -8.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            let theme = app.active_theme_data();
            egui::Frame::new()
                .fill(theme::Theme::color(&theme.panel))
                .stroke(egui::Stroke::new(1.0, theme::Theme::color(&theme.danger)))
                .inner_margin(8)
                .corner_radius(2)
                .show(ui, |ui| {
                    ui.set_min_width(220.0);
                    ui.vertical(|ui| {
                        for (message, _) in app.notifications() {
                            ui.label(
                                egui::RichText::new(message)
                                    .color(theme::Theme::color(&theme.text))
                                    .size(12.0),
                            );
                        }
                    });
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::Args;
    use crate::config::AppSettings;
    use pmd85_core::Model;

    /// One named UI smoke-test case: a setup function run on a fresh
    /// `App` before frames are built.
    type UiCase = Box<dyn Fn(&mut App)>;

    /// Build an App against the repo's Rom/ directory (headless: no
    /// window, no audio device requirements). Persisted settings go
    /// to a scratch directory, never the user's real config.
    pub fn test_app() -> App {
        app_with_settings(AppSettings::default())
    }

    /// [`test_app`] with explicit initial settings.
    fn app_with_settings(settings: AppSettings) -> App {
        let mut app = App::new(egui::Context::default(), &Args::default(), settings);
        let dir = std::env::temp_dir().join(format!("pmd85-app-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        app.set_settings_dir(dir);
        app
    }

    /// A fresh scratch project (manifest + template main.asm) for
    /// the project-tab cases; its build runs clean.
    fn test_project(name: &str) -> crate::project::Project {
        let dir =
            std::env::temp_dir().join(format!("pmd85-ui-project-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let project = crate::project::Project::create(&dir, name).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        project
    }

    /// The UI must build a full frame without panicking in every
    /// interesting state.
    #[test]
    fn ui_builds_frames_in_all_states() {
        let cases: Vec<(&str, UiCase)> = vec![
            ("boot", Box::new(|_| {})),
            ("paused", Box::new(|a| a.set_running(false))),
            (
                "turbo",
                Box::new(|a| {
                    a.set_turbo(true);
                }),
            ),
            ("slow", Box::new(|a| a.set_multiplier(0.25))),
            ("fast", Box::new(|a| a.set_multiplier(10.0))),
            ("settings-open", Box::new(|a| a.ui.settings_open = true)),
            (
                "titlebar-off",
                Box::new(|a| a.settings.custom_titlebar = false),
            ),
            ("maximized", Box::new(|a| a.window_maximized = true)),
            (
                "keyboard-open",
                Box::new(|a| {
                    crate::ui::dock::show_tab(&mut a.ui.dock, crate::ui::dock::Tab::Keyboard)
                }),
            ),
            (
                "joystick-open",
                Box::new(|a| {
                    crate::ui::dock::show_tab(&mut a.ui.dock, crate::ui::dock::Tab::Joystick)
                }),
            ),
            (
                "settings-changed",
                Box::new(|a| {
                    a.ui.settings_open = true;
                    a.pending_config.model = Model::Pmd851;
                    a.pending_config.rom_module = Some("basic1.rmm".into());
                }),
            ),
            (
                "confirm-reboot",
                Box::new(|a| {
                    a.ui.confirm_reboot = Some(a.pending_config.clone());
                }),
            ),
            ("muted", Box::new(|a| a.set_mute(true))),
            ("error-notification", Box::new(|a| a.notify("test error"))),
            (
                "tape-open",
                Box::new(|a| crate::ui::dock::show_tab(&mut a.ui.dock, crate::ui::dock::Tab::Tape)),
            ),
            (
                "tape-with-file",
                Box::new(|a| {
                    crate::ui::dock::show_tab(&mut a.ui.dock, crate::ui::dock::Tab::Tape);
                    let block =
                        pmd85_core::tape::make_file(0, b'?', "TESTFILE", 0x1000, &[1u8, 2, 3, 4])
                            .unwrap();
                    a.tape.tape.blocks.push(block);
                    a.tape.selected = Some(0);
                }),
            ),
            (
                "tape-playing",
                Box::new(|a| {
                    crate::ui::dock::show_tab(&mut a.ui.dock, crate::ui::dock::Tab::Tape);
                    a.tape.flash = false;
                    let block =
                        pmd85_core::tape::make_file(0, b'?', "TESTFILE", 0x1000, &[1u8, 2, 3, 4])
                            .unwrap();
                    a.tape.tape.blocks.push(block);
                    a.play_tape_file(0);
                }),
            ),
            (
                "tape-flash-loading",
                Box::new(|a| {
                    crate::ui::dock::show_tab(&mut a.ui.dock, crate::ui::dock::Tab::Tape);
                    let block =
                        pmd85_core::tape::make_file(0, b'?', "TESTFILE", 0x1000, &[1u8, 2, 3, 4])
                            .unwrap();
                    a.tape.tape.blocks.push(block);
                    a.play_tape_file(0);
                }),
            ),
            (
                "tape-import-form",
                Box::new(|a| {
                    crate::ui::dock::show_tab(&mut a.ui.dock, crate::ui::dock::Tab::Tape);
                    let block =
                        pmd85_core::tape::make_file(0, b'?', "TESTFILE", 0x1000, &[1u8, 2, 3, 4])
                            .unwrap();
                    a.tape.tape.blocks.push(block);
                    a.ui.tape_import = Some(crate::ui::tape::ImportDraft {
                        path: "raw.bin".into(),
                        number: "01".into(),
                        block_type: "?".into(),
                        name: "RAW".into(),
                        start: "1000".into(),
                    });
                }),
            ),
            (
                "debugger-open",
                Box::new(|a| {
                    for tab in [crate::ui::dock::Tab::Debugger, crate::ui::dock::Tab::Memory] {
                        crate::ui::dock::show_tab(&mut a.ui.dock, tab);
                    }
                }),
            ),
            (
                "debugger-at-breakpoint",
                Box::new(|a| {
                    for tab in [crate::ui::dock::Tab::Debugger, crate::ui::dock::Tab::Memory] {
                        crate::ui::dock::show_tab(&mut a.ui.dock, tab);
                    }
                    // Stop the monitor somewhere in its boot.
                    a.machine.toggle_breakpoint(0x0000);
                    a.run_frames(1);
                }),
            ),
            (
                "debugger-halted",
                Box::new(|a| {
                    crate::ui::dock::show_tab(&mut a.ui.dock, crate::ui::dock::Tab::Debugger);
                    a.machine.cpu.halted = true;
                    a.set_running(false);
                }),
            ),
            (
                "debugger-follow-off",
                Box::new(|a| {
                    crate::ui::dock::show_tab(&mut a.ui.dock, crate::ui::dock::Tab::Debugger);
                    a.ui.debug.follow_pc = false;
                    a.ui.debug.anchor = 0xE000;
                }),
            ),
            (
                "debugger-scrolled",
                Box::new(|a| {
                    crate::ui::dock::show_tab(&mut a.ui.dock, crate::ui::dock::Tab::Debugger);
                    // Peeking as far down and up as the clamp allows.
                    a.ui.debug.offset = 1000;
                }),
            ),
            (
                "project-empty",
                Box::new(|a| {
                    crate::ui::dock::show_tab(&mut a.ui.dock, crate::ui::dock::Tab::Project);
                }),
            ),
            (
                "project-open",
                Box::new(|a| {
                    crate::ui::dock::show_tab(&mut a.ui.dock, crate::ui::dock::Tab::Project);
                    a.project = Some(test_project("open"));
                }),
            ),
            (
                "project-with-problems",
                Box::new(|a| {
                    crate::ui::dock::show_tab(&mut a.ui.dock, crate::ui::dock::Tab::Project);
                    crate::ui::dock::show_tab(&mut a.ui.dock, crate::ui::dock::Tab::Problems);
                    let mut p = test_project("errs");
                    let main = p.files.iter().position(|f| f.path == "main.asm").unwrap();
                    p.files[main].text = "MVI A,MISSING\n".to_string();
                    p.build = Some(p.build());
                    // And a pending jump for the editor to apply.
                    a.ui.project_jump = Some(crate::ui::project::Jump {
                        file: "main.asm".into(),
                        line: 1,
                        col: 5,
                    });
                    a.project = Some(p);
                }),
            ),
            (
                "project-find-and-goto",
                Box::new(|a| {
                    crate::ui::dock::show_tab(&mut a.ui.dock, crate::ui::dock::Tab::Project);
                    let mut p = test_project("find");
                    p.find.open = true;
                    p.find.query = "MVI".into();
                    p.goto_open = true;
                    a.project = Some(p);
                }),
            ),
            (
                "project-target-edit",
                Box::new(|a| {
                    crate::ui::dock::show_tab(&mut a.ui.dock, crate::ui::dock::Tab::Project);
                    let mut p = test_project("target");
                    p.target_open = true;
                    p.find.open = true;
                    a.project = Some(p);
                }),
            ),
            (
                "project-file-dialogs",
                Box::new(|a| {
                    crate::ui::dock::show_tab(&mut a.ui.dock, crate::ui::dock::Tab::Project);
                    let mut p = test_project("dialogs");
                    p.new_open = true;
                    p.new_name = "x.asm".into();
                    p.rename = Some(0);
                    p.rename_name = "main2.asm".into();
                    p.remove = Some(0);
                    a.project = Some(p);
                }),
            ),
            (
                "project-dirty-guard",
                Box::new(|a| {
                    crate::ui::dock::show_tab(&mut a.ui.dock, crate::ui::dock::Tab::Project);
                    let mut p = test_project("guard-frame");
                    p.files[0].text = "MVI A,1\n".to_string();
                    p.files[0].dirty = true;
                    a.project = Some(p);
                    a.close_guard = Some(crate::app::AfterClose::Project);
                }),
            ),
            (
                "project-running",
                Box::new(|a| {
                    crate::ui::dock::show_tab(&mut a.ui.dock, crate::ui::dock::Tab::Project);
                    a.project = Some(test_project("run"));
                    a.run_project();
                }),
            ),
        ];
        for (name, setup) in cases {
            let mut app = test_app();
            setup(&mut app);
            // A few frames, running emulation so the screen updates.
            for _ in 0..5 {
                app.advance();
                let ctx = app.ctx.clone();
                let out = ctx.run_ui(egui::RawInput::default(), |ui| draw(&mut app, ui));
                assert!(!out.shapes.is_empty(), "{name}: no shapes");
                out.drop_without_applying_deltas();
            }
        }
    }

    #[test]
    fn advance_emulates_and_updates_screen() {
        let mut app = test_app();
        let before = app.machine.bus.total_cycles();
        app.advance();
        assert!(app.machine.bus.total_cycles() > before);
        assert!(app.frames_done >= 1);
        assert!(app.screen.is_some(), "screen texture created");
    }

    /// F5 end to end, against every model's real monitor ROM: the
    /// built program becomes an autorun tape file (the loader block
    /// at 0x7FD7 plus the program continuation), the machine boots
    /// from the cached booted state (a real reset when the capture
    /// fails), the app types the monitor's `MGLD 00`, the monitor
    /// loads both blocks with the checksums for real, the loader
    /// hijacks the reader's return slot and the template program
    /// fills video RAM and halts. On the 85-3 the flow first types
    /// `JUMP FFF0`, switching Monitor 3 into its 85-2 compatibility
    /// mode (its native stack at 0xBF00 is out of the return slot's
    /// reach), and waits for that reboot before the load command —
    /// unless the cached state already sits in the compatibility
    /// mode, which is what the capture produces.
    #[test]
    fn run_project_loads_and_runs_the_program() {
        for model in [Model::Pmd852a, Model::Pmd852, Model::Pmd853, Model::Pmd851] {
            let mut app = test_app();
            app.rebuild_machine(&crate::app::MachineConfig {
                model,
                monitor: None,
                rom_module: None,
            });
            app.project = Some(test_project("e2e"));
            app.run_project();
            assert!(app.running, "running after F5");
            assert!(app.auto_type.is_some(), "the command is queued");
            assert!(
                app.tape.tape.blocks.is_empty(),
                "runs do not add blocks to the tape"
            );

            // Drive machine and auto-typer by frames, without pacing.
            // The 85-3 needs the most frames: the compatibility
            // switch's reboot (when the cached state missed), then the
            // load.
            for _ in 0..4000 {
                app.pump_auto_type();
                app.run_frames(1);
            }
            assert!(app.auto_type.is_none(), "typing finished");
            let ram = &app.machine.bus.memory.ram;
            assert_eq!(ram[0x1000], 0x21, "program loaded at its ORG");
            assert_eq!(ram[0x1001], 0x00, "LXI H,0C000 low byte");
            assert_eq!(ram[0x1002], 0xC0, "LXI H,0C000 high byte");
            assert_eq!(ram[0xC000], 0x2A, "the template filled video RAM");
            assert!(app.machine.cpu.halted, "the program ran to its HLT");
        }
    }

    /// The booted-state cache: F5 on the 85-3 boots from the captured
    /// snapshot (the monitor's memory test skipped) already sitting
    /// in Monitor 3's compatibility mode, so the typed `MGLD` lands
    /// within a handful of frames — far less than any real boot — and
    /// runs to the end without the `JUMP FFF0` dance.
    #[test]
    fn run_project_boots_from_the_cached_state() {
        let mut app = test_app();
        app.rebuild_machine(&crate::app::MachineConfig {
            model: Model::Pmd853,
            monitor: None,
            rom_module: None,
        });
        app.project = Some(test_project("e2e-snapshot"));
        app.run_project();
        assert!(app.boot_state.is_some(), "the booted state was captured");
        assert!(
            !app.machine.bus.memory.startup_map(),
            "past the boot-time shadow map"
        );
        assert!(app.auto_type.is_some(), "MGLD is queued (no JUMP needed)");

        // A real 85-3 boot takes well over this many frames; the
        // snapshot has the machine at the prompt already.
        for _ in 0..600 {
            app.pump_auto_type();
            app.run_frames(1);
        }
        assert!(app.auto_type.is_none(), "typing finished without a boot");
        assert_eq!(
            app.machine.bus.memory.ram[0xC000], 0x2A,
            "the template filled video RAM"
        );
        assert!(app.machine.cpu.halted, "the program ran to its HLT");

        // A second run restores the same cached state.
        app.run_project();
        assert!(!app.machine.bus.memory.startup_map());

        // The authentic fallback when no snapshot can be captured: a
        // real reset, the typed `JUMP FFF0` compatibility switch and
        // the wait for its reboot before the load — the flow as it
        // runs on a machine the cache cannot serve.
        app.boot_state = None;
        app.monitor_rom = vec![0u8; Model::Pmd853.monitor_size()];
        app.run_project();
        assert!(app.auto_type.is_some(), "JUMP FFF0 then MGLD is queued");
        for _ in 0..4000 {
            app.pump_auto_type();
            app.run_frames(1);
        }
        assert!(app.auto_type.is_none(), "typing finished");
        assert_eq!(
            app.machine.bus.memory.ram[0xC000], 0x2A,
            "the template filled video RAM"
        );
        assert!(app.machine.cpu.halted, "the program ran to its HLT");
    }

    /// Ctrl+F5 end to end, against every model's real monitor ROM:
    /// the built image is injected into RAM at its ORG and the CPU
    /// started at the entry point — from the cached booted state when
    /// possible, else from a reset with the boot-time shadow map
    /// dropped. (Until the monitor's first I/O write the ROM is
    /// mirrored over the RAM the program lives in: an inject into
    /// that map is unreadable, and the machine used to boot the
    /// monitor instead of running the program.)
    #[test]
    fn run_project_direct_loads_and_runs_the_program() {
        for model in [Model::Pmd852a, Model::Pmd852, Model::Pmd853, Model::Pmd851] {
            let mut app = test_app();
            app.rebuild_machine(&crate::app::MachineConfig {
                model,
                monitor: None,
                rom_module: None,
            });
            let mut project = test_project("e2e-direct");
            project.manifest.target = crate::project::Target::Bin;
            app.project = Some(project);
            app.run_project();
            assert!(app.running, "running after Ctrl+F5");
            assert!(app.auto_type.is_none(), "nothing is typed for a direct run");
            assert!(!app.machine.bus.memory.startup_map());

            for _ in 0..2000 {
                app.run_frames(1);
            }
            let ram = &app.machine.bus.memory.ram;
            assert_eq!(ram[0x1000], 0x21, "program injected at its ORG");
            assert_eq!(ram[0xC000], 0x2A, "the template filled video RAM");
            assert!(app.machine.cpu.halted, "the program ran to its HLT");

            // And once more with the cache defeated (no snapshot, no
            // capture): the bare-reset path, with the boot-time shadow
            // map dropped — the original bug: an image injected into
            // the shadowed map was unreadable and the machine booted
            // the monitor instead of running the program.
            app.boot_state = None;
            app.monitor_rom = vec![0u8; model.monitor_size()];
            app.run_project();
            for _ in 0..2000 {
                app.run_frames(1);
            }
            let ram = &app.machine.bus.memory.ram;
            assert_eq!(ram[0x1000], 0x21, "program injected at its ORG");
            assert_eq!(ram[0xC000], 0x2A, "the template filled video RAM");
            assert!(app.machine.cpu.halted, "the program ran to its HLT");
        }
    }

    /// F5 with the ROM-module target, against every model: the built
    /// program becomes a `.rmm` image, the app plugs it into the
    /// module connector and reboots, the monitor BIOS transfers the
    /// payload to 0x1000 through the connector's 8255 and runs it.
    /// On the 85-1 the app types the `BASIC G` transfer command
    /// instead (Monitor 1 has no boot-time detection).
    #[test]
    fn run_project_rom_module_boots_and_runs_the_program() {
        for model in [Model::Pmd852a, Model::Pmd852, Model::Pmd853, Model::Pmd851] {
            let mut app = test_app();
            app.rebuild_machine(&crate::app::MachineConfig {
                model,
                monitor: None,
                rom_module: None,
            });
            let mut project = test_project("e2e-module");
            project.manifest.target = crate::project::Target::RomModule;
            app.project = Some(project);
            app.run_project();
            assert!(app.running, "running after F5");
            assert!(
                app.machine.bus.memory.rom_module.is_some(),
                "the module is attached"
            );
            assert_eq!(
                app.auto_type.is_some(),
                model == Model::Pmd851,
                "only the 85-1 needs a typed command"
            );

            for _ in 0..3000 {
                app.pump_auto_type();
                app.run_frames(1);
            }
            assert!(app.auto_type.is_none(), "typing finished");
            let ram = &app.machine.bus.memory.ram;
            assert_eq!(ram[0x1000], 0x21, "program transferred at its ORG");
            assert_eq!(ram[0xC000], 0x2A, "the template filled video RAM");
            assert!(app.machine.cpu.halted, "the program ran to its HLT");
        }
    }

    /// The ROM module a project run attaches is transient: a warm
    /// reboot ejects it (the connector returns to the configured
    /// module, if any) — it never lingers past a reset.
    #[test]
    fn rom_module_run_is_ejected_by_reset() {
        let mut app = test_app();
        app.rebuild_machine(&crate::app::MachineConfig {
            model: Model::Pmd852,
            monitor: None,
            rom_module: None,
        });
        let mut project = test_project("e2e-module-eject");
        project.manifest.target = crate::project::Target::RomModule;
        app.project = Some(project);
        app.run_project();
        assert!(
            app.machine.bus.memory.rom_module.is_some(),
            "the module is attached"
        );
        assert!(app.ide_module.is_some(), "and tracked as transient");
        app.reset();
        assert!(
            app.machine.bus.memory.rom_module.is_none(),
            "the reboot ejects the run's module"
        );
        assert!(app.ide_module.is_none());

        // With a configured module, the reboot returns to it.
        app.rebuild_machine(&crate::app::MachineConfig {
            model: Model::Pmd852,
            monitor: None,
            rom_module: Some("basic2.rmm".into()),
        });
        assert!(app.machine.bus.memory.rom_module.is_some());
        app.run_project();
        assert!(app.machine.bus.memory.rom_module.is_some());
        app.reset();
        assert!(
            app.machine.bus.memory.rom_module.is_some(),
            "the configured module stays through reboots"
        );
    }

    /// Window requests from the UI queue up for the winit loop and
    /// are drained once, in order.
    #[test]
    fn window_requests_queue_and_drain() {
        use crate::app::WindowRequest;
        let mut app = test_app();
        assert!(app.take_window_requests().is_empty(), "starts quiet");

        app.request_window(WindowRequest::Minimize);
        app.request_window(WindowRequest::ToggleMaximize);
        app.request_window(WindowRequest::Close);
        assert_eq!(
            app.take_window_requests(),
            vec![
                WindowRequest::Minimize,
                WindowRequest::ToggleMaximize,
                WindowRequest::Close,
            ]
        );
        // Drained: a second take must not repeat them.
        assert!(app.take_window_requests().is_empty());
    }

    /// A project kept on disk (unlike [`test_project`], which deletes
    /// its directory): for the tests that save, close and reopen it.
    fn disk_project(name: &str) -> crate::project::Project {
        let dir = std::env::temp_dir().join(format!(
            "pmd85-ui-disk-project-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::project::Project::create(&dir, name).unwrap()
    }

    /// The Build button writes the artifact into the project's
    /// release/ folder, in the target's format, byte-identical to
    /// what the run path loads: the autostart tape pair (loader at
    /// 0x7FD7 + the program), a `.rmm` ROM module, or the raw image.
    #[test]
    fn build_writes_the_release_artifact_per_target() {
        // ---- tape (the default target) ----
        let mut app = test_app();
        let mut project = disk_project("rel");
        project.manifest.name = "Rel Name!".to_string();
        let dir = project.dir.clone();
        app.project = Some(project);
        assert!(app.build_and_release_project());

        let release = dir.join("release");
        assert!(release.is_dir(), "the release folder exists");
        let tape = pmd85_core::tape::load(&release.join("RelName.ptp")).unwrap();
        let (base, image) = app
            .project
            .as_ref()
            .unwrap()
            .build
            .as_ref()
            .unwrap()
            .asm
            .flat_image(0)
            .unwrap();
        assert_eq!(tape.blocks.len(), 2, "the autostart pair");
        assert_eq!(
            tape.blocks[0].header.as_ref().unwrap().name_str(),
            "PROGRAM",
            "the tape file's name (the target's, not the project's)"
        );
        assert_eq!(tape.blocks[0].header.as_ref().unwrap().start, 0x7FD7);
        assert_eq!(
            tape.blocks[1].header, None,
            "the program rides as a headerless continuation"
        );
        assert_eq!(
            tape.blocks[1].body_bytes,
            pmd85_core::tape::make_continuation(&image).body_bytes,
            "the continuation holds the built image"
        );
        assert!(!base.to_string().is_empty()); // (base read; the image is the point)

        // ---- ROM module ----
        app.project.as_mut().unwrap().manifest.target = crate::project::Target::RomModule;
        assert!(app.build_and_release_project());
        let module_bytes = std::fs::read(release.join("RelName.rmm")).unwrap();
        let model = app.machine.bus.model;
        assert_eq!(
            module_bytes,
            pmd85_core::module::make_rom_module(model, &image, base, 0x1000),
            "the .rmm is the module the run would attach"
        );

        // ---- raw binary ----
        app.project.as_mut().unwrap().manifest.target = crate::project::Target::Bin;
        assert!(app.build_and_release_project());
        assert_eq!(
            std::fs::read(release.join("RelName.bin")).unwrap(),
            image,
            "the .bin is the flat image"
        );

        // The release folder never showed up among the sources.
        let paths: Vec<String> = crate::project::Project::open(&dir)
            .unwrap()
            .files
            .iter()
            .map(|f| f.path.clone())
            .collect();
        assert_eq!(paths, ["main.asm"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The unsaved-changes guard: closing a project with dirty buffers
    /// waits for the user; save-and-close writes them; a plain close
    /// discards them; cancel aborts everything; a clean project closes
    /// without any guard.
    #[test]
    fn close_guard_saves_or_discards_dirty_buffers() {
        let mut app = test_app();
        let mut project = disk_project("guard");
        let dir = project.dir.clone();
        project.files[0].text = "MVI A,9\nHLT\n".to_string();
        project.files[0].dirty = true;
        app.project = Some(project);

        // Close with dirty buffers: the guard goes up, nothing closes.
        app.close_project();
        assert!(app.close_guard.is_some(), "the guard is up");
        assert!(app.project.is_some(), "the project is still open");
        // Cancel: everything stays.
        app.cancel_close_guard();
        assert!(app.close_guard.is_none());
        assert!(app.project.is_some());

        // Save and close: the buffer is written, the project closed.
        app.close_project();
        assert!(app.resolve_close_guard(true), "the guard resolved");
        assert!(app.close_guard.is_none());
        assert!(app.project.is_none());
        assert_eq!(
            std::fs::read_to_string(dir.join("main.asm")).unwrap(),
            "MVI A,9\nHLT\n"
        );

        // A clean project closes without any guard.
        app.open_project(&dir);
        assert!(app.project.is_some());
        app.close_project();
        assert!(app.close_guard.is_none(), "nothing dirty, no guard");
        assert!(app.project.is_none());

        // Close without saving: the file on disk keeps the old text.
        app.open_project(&dir);
        app.project.as_mut().unwrap().files[0].text = "NOP\n".to_string();
        app.project.as_mut().unwrap().files[0].dirty = true;
        app.close_project();
        assert!(app.resolve_close_guard(false));
        assert!(app.project.is_none());
        assert_eq!(
            std::fs::read_to_string(dir.join("main.asm")).unwrap(),
            "MVI A,9\nHLT\n",
            "the discard kept the disk text"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The guard holds a project switch too: opening a project over
    /// dirty buffers waits, then switches (with or without saving).
    #[test]
    fn close_guard_holds_a_project_switch() {
        let mut app = test_app();
        let mut a = disk_project("switch-a");
        let dir_a = a.dir.clone();
        a.files[0].text = "MVI A,1\n".to_string();
        a.files[0].dirty = true;
        let b = disk_project("switch-b");
        let dir_b = b.dir.clone();
        app.project = Some(a);

        app.open_project(&dir_b);
        assert!(
            matches!(app.close_guard, Some(crate::app::AfterClose::Open(_))),
            "the switch waits for the guard"
        );
        assert_eq!(
            app.project.as_ref().unwrap().dir,
            dir_a,
            "the first project is still open"
        );

        // Discard and switch: no save, the new project opens.
        assert!(app.resolve_close_guard(false));
        assert_eq!(app.project.as_ref().unwrap().dir, dir_b);
        assert!(
            std::fs::read_to_string(dir_a.join("main.asm"))
                .unwrap()
                .starts_with("; Fills the PMD 85 screen"),
            "nothing was saved"
        );
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    /// The window close goes through the same guard: dirty buffers
    /// hold the exit, the dialog's answer confirms it.
    #[test]
    fn request_exit_guards_dirty_buffers() {
        let mut app = test_app();
        assert!(app.request_exit(), "a clean app exits at once");

        let mut project = disk_project("exit");
        let dir = project.dir.clone();
        project.files[0].text = "NOP\n".to_string();
        project.files[0].dirty = true;
        app.project = Some(project);
        assert!(!app.request_exit(), "dirty buffers hold the exit");
        assert_eq!(app.close_guard, Some(crate::app::AfterClose::Exit));

        // Cancel: still dirty, still holding.
        app.cancel_close_guard();
        assert!(!app.request_exit());

        // Save and exit: the save happened, the close is queued and
        // confirmed — the queued close passes request_exit.
        assert!(app.resolve_close_guard(true));
        assert!(app.exit_confirmed);
        assert_eq!(
            app.take_window_requests(),
            vec![crate::app::WindowRequest::Close]
        );
        assert!(app.request_exit(), "the confirmed exit passes");
        assert_eq!(
            std::fs::read_to_string(dir.join("main.asm")).unwrap(),
            "NOP\n",
            "the guard saved before exiting"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The IDE has no keyboard shortcuts: with the Project tab
    /// visible, F5 / Ctrl+S / Ctrl+B run, save and build nothing —
    /// the host keyboard belongs to the emulated machine (F5 is its
    /// K4 key, Ctrl its STOP key).
    #[test]
    fn no_host_shortcuts_in_the_project_tab() {
        let mut app = test_app();
        let mut project = test_project("no-shortcuts");
        project.files[0].text = "MVI A,1\nHLT\n".to_string();
        project.files[0].dirty = true;
        app.project = Some(project);
        crate::ui::dock::show_tab(&mut app.ui.dock, crate::ui::dock::Tab::Project);
        app.set_running(false);

        let frame = |app: &mut App, key: egui::Key, modifiers: egui::Modifiers| {
            let ctx = app.ctx.clone();
            ctx.run_ui(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key,
                        physical_key: Some(key),
                        pressed: true,
                        repeat: false,
                        modifiers,
                    }],
                    ..Default::default()
                },
                |ui| draw(app, ui),
            )
            .drop_without_applying_deltas();
        };
        frame(&mut app, egui::Key::F5, egui::Modifiers::NONE);
        frame(&mut app, egui::Key::S, egui::Modifiers::CTRL);
        frame(&mut app, egui::Key::B, egui::Modifiers::CTRL);

        assert!(!app.running, "F5 did not run the project");
        assert!(app.auto_type.is_none(), "no command was typed");
        assert!(
            app.project.as_ref().unwrap().files[0].dirty,
            "Ctrl+S did not save"
        );
        assert!(
            app.notifications()
                .iter()
                .all(|(m, _)| !m.contains("Build OK")),
            "Ctrl+B did not build"
        );
    }

    /// The Project tab's geometry: the file tree is a 150 px left
    /// column with the "sources" label on top and the file rows
    /// stacked below it, and the editor fills the rest of the tab.
    /// (Regression: the tree and editor sat in a `ui.horizontal` row,
    /// which egui sizes to one interact height — the tree's content
    /// capped the editor at a few lines, and the tree's rows inherited
    /// the row's horizontal layout and drew side by side.)
    #[test]
    fn project_tab_tree_is_a_column_and_the_editor_fills_the_tab() {
        let mut app = test_app();
        let mut project = test_project("layout");
        // A long file: the editor's viewport (not its content) is
        // what must fill the tab — a long file makes the two
        // distinguishable in the painted clip rects.
        project.files[0].text = "NOP\n".repeat(80);
        app.project = Some(project);
        crate::ui::dock::show_tab(&mut app.ui.dock, crate::ui::dock::Tab::Project);
        let ctx = app.ctx.clone();
        let screen = egui::vec2(1024.0, 768.0);
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
            ..Default::default()
        };
        // A few frames for the layout to settle.
        for _ in 0..2 {
            ctx.run_ui(raw.clone(), |ui| draw(&mut app, ui))
                .drop_without_applying_deltas();
        }
        let out = ctx.run_ui(raw.clone(), |ui| draw(&mut app, ui));

        // The editor's own TextEdit and the clips of its painted
        // content: the TextEdit is content-sized, so what proves the
        // editor fills the tab is the clip its painting gets — the
        // scroll area's viewport, exactly the thing the bug shrank
        // to the tree's content height.
        let editor = ctx
            .read_response(egui::Id::new(("project-editor", "main.asm")))
            .expect("the editor was drawn");
        // The tree column: the "sources" label's paint clip is the
        // tree's own viewport, the fixed 150 px column.
        let mut sources_clip = None;
        let mut sources = None;
        let mut main_row = None;
        let mut editor_clip = None;
        for clipped in &out.shapes {
            let egui::Shape::Text(text) = &clipped.shape else {
                continue;
            };
            let clip = clipped.clip_rect;
            let body = text.galley.text();
            let rect = clipped.shape.visual_bounding_rect();
            if body == "sources" {
                sources_clip = Some(clip);
                sources = Some(rect);
            } else if body == "main.asm" {
                main_row = Some(rect);
            } else if body.contains("NOP") && editor_clip.is_none() {
                // The editor's content (NOP repeated): its clip is
                // the editor's viewport.
                editor_clip = Some(clip);
            }
        }
        let sources_clip = sources_clip.expect("the sources label was painted");
        let sources = sources.expect("the sources label was painted");
        let main_row = main_row.expect("the main.asm row was painted");
        let editor_clip = editor_clip.expect("editor text was painted");

        // A fixed ~150 px tree column, as tall as the tab.
        let column = sources_clip.width();
        assert!(
            (column - 150.0).abs() < 10.0,
            "the tree column is 150 px wide ({column:.0} px)"
        );
        assert!(
            sources_clip.height() > screen.y * 0.5,
            "the tree column is as tall as the tab ({} px)",
            sources_clip.height()
        );

        // The file rows stack vertically below the "sources" label,
        // inside the column.
        assert!(
            main_row.top() > sources.bottom(),
            "the file row is below the sources label (the tree is vertical)"
        );
        assert!(
            (main_row.left() - sources.left()).abs() < 10.0,
            "the tree rows share one column (left edges {:.0} vs {:.0})",
            main_row.left(),
            sources.left()
        );
        assert!(
            main_row.right() < sources_clip.right(),
            "the tree stays in its column (row right edge {:.0})",
            main_row.right()
        );

        // The editor fills the rest: right of the column, wide, and
        // its viewport is nearly the whole tab tall.
        assert!(
            editor.rect.left() > sources_clip.right(),
            "the editor sits right of the tree column (left edge {:.0})",
            editor.rect.left()
        );
        assert!(
            editor.rect.width() > screen.x * 0.3,
            "the editor fills the tab width ({:.0} px)",
            editor.rect.width()
        );
        assert!(
            editor_clip.height() > screen.y * 0.5,
            "the editor's viewport fills the tab ({} px tall)",
            editor_clip.height()
        );
        out.drop_without_applying_deltas();
    }

    /// Dragging a selection past the editor's edge auto-scrolls it
    /// (egui does not do this on its own): while the drag is held,
    /// the view follows the pointer — even when the pointer then
    /// stops moving — and it stops when the drag is released.
    #[test]
    fn dragging_a_selection_past_the_edge_auto_scrolls() {
        let mut app = test_app();
        let mut project = test_project("autoscroll");
        project.files[0].text = "NOP\n".repeat(200);
        app.project = Some(project);
        crate::ui::dock::show_tab(&mut app.ui.dock, crate::ui::dock::Tab::Project);
        let ctx = app.ctx.clone();
        let screen = egui::vec2(1024.0, 768.0);
        let base = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
            ..Default::default()
        };
        // A frame with `events`; the clock advances a real 60 fps
        // between frames (stable_dt drives the scroll speed).
        let mut time = 0.0;
        let mut frame = |events: Vec<egui::Event>, app: &mut App| -> egui::FullOutput {
            time += 1.0 / 60.0;
            let mut raw = base.clone();
            raw.events = events;
            raw.time = Some(time);
            ctx.run_ui(raw, |ui| draw(app, ui))
        };
        // The top of the editor's painted content: it moves up as
        // the editor scrolls.
        let nop_top = |out: &egui::FullOutput| {
            out.shapes
                .iter()
                .find_map(|clipped| {
                    let egui::Shape::Text(text) = &clipped.shape else {
                        return None;
                    };
                    text.galley
                        .text()
                        .contains("NOP")
                        .then(|| clipped.shape.visual_bounding_rect().top())
                })
                .expect("editor text was painted")
        };

        // Let the layout settle; nothing scrolls on its own.
        let out = frame(vec![], &mut app);
        let settled = nop_top(&out);
        out.drop_without_applying_deltas();
        let out = frame(vec![], &mut app);
        assert_eq!(nop_top(&out), settled, "nothing scrolls on its own");
        out.drop_without_applying_deltas();

        // Press inside the editor (near its visible top: the TextEdit
        // is content-sized, so its center is far below the viewport),
        // then drag below the viewport and hold the pointer there.
        let editor = ctx
            .read_response(egui::Id::new(("project-editor", "main.asm")))
            .expect("the editor was drawn");
        let p = egui::pos2(editor.rect.left() + 40.0, editor.rect.top() + 40.0);
        frame(vec![egui::Event::PointerMoved(p)], &mut app).drop_without_applying_deltas();
        frame(
            vec![egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            }],
            &mut app,
        )
        .drop_without_applying_deltas();
        frame(
            vec![egui::Event::PointerMoved(egui::pos2(p.x, 760.0))],
            &mut app,
        )
        .drop_without_applying_deltas();

        // The pointer is now held still past the bottom edge: every
        // following frame (with no events at all) keeps scrolling.
        // The first of them still paints at the old offset — a
        // scroll delta written during frame N moves the content
        // from frame N+1 on.
        let mut tops = Vec::new();
        for _ in 0..4 {
            let out = frame(vec![], &mut app);
            tops.push(nop_top(&out));
            out.drop_without_applying_deltas();
        }
        assert!(
            tops.windows(2).all(|w| w[1] < w[0] - 0.5),
            "the held-still drag keeps scrolling ({tops:?})"
        );
        assert!(
            tops[3] < settled - 3.0,
            "the drag past the edge scrolls the editor ({settled:.0} → {:?})",
            tops[3]
        );

        // It stops when the drag is released (the release frame
        // itself may still apply the last pending delta).
        frame(
            vec![egui::Event::PointerButton {
                pos: egui::pos2(p.x, 760.0),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
            &mut app,
        )
        .drop_without_applying_deltas();
        let mut tops = Vec::new();
        for _ in 0..2 {
            let out = frame(vec![], &mut app);
            tops.push(nop_top(&out));
            out.drop_without_applying_deltas();
        }
        assert_eq!(
            tops[0], tops[1],
            "the scroll stops with the drag ({tops:?})"
        );
    }

    #[test]
    fn paused_advance_does_not_emulate() {
        let mut app = test_app();
        app.set_running(false);
        let before = app.machine.bus.total_cycles();
        app.advance();
        assert_eq!(app.machine.bus.total_cycles(), before);
        assert_eq!(app.frames_done, 0);
    }

    #[test]
    fn stop_resets_and_holds() {
        let mut app = test_app();
        app.advance();
        app.advance();
        let cycles = app.machine.bus.total_cycles();
        app.stop();
        assert!(!app.running);
        assert_eq!(app.frames_done, 0);
        // Held: no further emulation happens.
        app.advance();
        assert_eq!(app.machine.bus.total_cycles(), cycles);
    }

    #[test]
    fn reset_keeps_running() {
        let mut app = test_app();
        app.advance();
        let before = app.machine.bus.total_cycles();
        app.reset();
        assert!(app.running);
        // The machine is live again: cycles keep accumulating past the
        // (monotonic) counter value from before the reset.
        app.advance();
        assert!(app.machine.bus.total_cycles() > before);
    }

    #[test]
    fn rebuild_machine_switches_model_and_module() {
        let mut app = test_app();
        app.advance();
        let cfg = crate::app::MachineConfig {
            model: Model::Pmd852,
            monitor: None,
            rom_module: Some("basic2.rmm".into()),
        };
        app.apply_config(cfg);
        assert_eq!(app.machine.model(), Model::Pmd852);
        assert!(app.machine.bus.memory.rom_module.is_some());
        assert!(app.running);
        assert_eq!(app.frames_done, 0);
        // pending_config follows the applied configuration.
        assert_eq!(app.pending_config, app.config);
    }

    #[test]
    fn rebuild_with_missing_rom_is_not_fatal() {
        let mut app = test_app();
        let cfg = crate::app::MachineConfig {
            model: Model::Pmd851,
            monitor: Some("does-not-exist.rom".into()),
            rom_module: None,
        };
        app.apply_config(cfg);
        assert!(!app.notifications().is_empty());
        // Machine still exists and can run (blank monitor).
        app.advance();
    }

    #[test]
    fn multiplier_clamps_to_the_planned_range() {
        let mut app = test_app();
        app.set_multiplier(0.01);
        assert_eq!(app.speed.multiplier, crate::speed::SPEED_MIN);
        app.set_multiplier(1000.0);
        assert_eq!(app.speed.multiplier, crate::speed::SPEED_MAX);
        app.set_multiplier(2.0);
        assert_eq!(app.speed.multiplier, 2.0);
        app.set_multiplier(1.0);
        // Settings loaded from disk are clamped too.
        let settings = AppSettings {
            speed: 500.0,
            ..AppSettings::default()
        };
        let app = app_with_settings(settings);
        assert_eq!(app.speed.multiplier, crate::speed::SPEED_MAX);
    }

    #[test]
    fn paused_ui_keeps_painting() {
        // Regression: while paused/stopped the event loop must keep
        // requesting (lazy) redraws, otherwise the UI freezes and the
        // machine can never be unpaused.
        let mut app = test_app();
        app.set_running(false);
        assert!(app.wants_redraw(std::time::Instant::now()));
        // The lazy wake must also be scheduled in the future, so the
        // loop keeps ticking instead of parking forever.
        let now = std::time::Instant::now();
        assert!(app.next_wake(now) > now);
    }

    #[test]
    fn settings_window_stays_within_the_screen() {
        // Regression: the settings combo boxes used to be sized from
        // `available_width` while more widgets followed in the same
        // row. The window sizes itself to its content, so the row was
        // always a bit wider than the window: a feedback loop that
        // grew the window past the screen edge within a few frames.
        let mut app = test_app();
        app.ui.settings_open = true;
        // Show the "restart required" warning row too (pending config
        // differs from the running one).
        app.pending_config.model = Model::Pmd851;
        app.pending_config.rom_module = Some("basic1.rmm".into());
        let ctx = app.ctx.clone();
        let screen = egui::vec2(800.0, 600.0);
        let raw = |screen: egui::Vec2| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
            ..Default::default()
        };
        let mut right_edge: f32 = 0.0;
        for _ in 0..30 {
            let out = ctx.run_ui(raw(screen), |ui| {
                crate::ui::settings::draw(ui.ctx(), &mut app);
            });
            // Frames while the window is opening may paint nothing.
            if let Some(bbox) = out
                .shapes
                .iter()
                .map(|s| s.shape.visual_bounding_rect())
                .filter(|r| r.is_finite() && r.width() > 0.0 && r.height() > 0.0)
                .fold(None::<egui::Rect>, |acc, r| {
                    Some(acc.map_or(r, |a| a.union(r)))
                })
            {
                right_edge = right_edge.max(bbox.right());
            }
            out.drop_without_applying_deltas();
        }
        assert!(right_edge > 0.0, "settings window never painted");
        assert!(
            right_edge <= screen.x + 1.0,
            "settings window overflows the screen (right edge {:.0} > {:.0})",
            right_edge,
            screen.x
        );
    }

    #[test]
    fn keyboard_tab_paints_within_the_screen() {
        let mut app = test_app();
        crate::ui::dock::show_tab(&mut app.ui.dock, crate::ui::dock::Tab::Keyboard);
        let ctx = app.ctx.clone();
        let screen = egui::vec2(1024.0, 768.0);
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
            ..Default::default()
        };
        let mut right_edge: f32 = 0.0;
        for _ in 0..10 {
            let out = ctx.run_ui(raw.clone(), |ui| {
                crate::ui::dock::draw(ui, &mut app);
            });
            if let Some(bbox) = out
                .shapes
                .iter()
                .map(|s| s.shape.visual_bounding_rect())
                .filter(|r| r.is_finite() && r.width() > 0.0 && r.height() > 0.0)
                .fold(None::<egui::Rect>, |acc, r| {
                    Some(acc.map_or(r, |a| a.union(r)))
                })
            {
                right_edge = right_edge.max(bbox.right());
            }
            out.drop_without_applying_deltas();
        }
        // The dock (screen tab + the keyboard pane beside it) must
        // paint across the area and never overflow the window.
        assert!(right_edge > 100.0, "dock area never painted");
        assert!(
            right_edge <= screen.x + 1.0,
            "dock area overflows the screen (right edge {:.0} > {:.0})",
            right_edge,
            screen.x
        );
    }

    /// The window geometry persists through `layout.json` like the
    /// dock layout does: the last floating size and the maximized
    /// state come back on the next start.
    #[test]
    fn window_geometry_survives_a_restart() {
        let dir = state_scratch_dir("window-layout");
        let mut app = test_app();
        app.set_settings_dir(dir.clone());
        // A floating window of a distinctive size.
        app.window_size = egui::vec2(1234.0, 567.0);
        app.save_layout();

        let mut app2 = test_app();
        app2.set_settings_dir(dir.clone());
        app2.restore_layout();
        assert_eq!(app2.window_size, egui::vec2(1234.0, 567.0));
        assert!(!app2.window_start_maximized);

        // Closing while maximized remembers that — and the last
        // floating size, not the maximized one.
        app.window_maximized = true;
        app.save_layout();
        let mut app3 = test_app();
        app3.set_settings_dir(dir.clone());
        app3.restore_layout();
        assert!(app3.window_start_maximized);
        assert_eq!(app3.window_size, egui::vec2(1234.0, 567.0));

        // A corrupt geometry must not make an absurd window: the
        // size is clamped to something sane.
        std::fs::write(
            app.layout_path().unwrap(),
            r#"{"window": {"size": [0.0, -5.0], "maximized": false}}"#,
        )
        .unwrap();
        let mut app4 = test_app();
        app4.set_settings_dir(dir.clone());
        app4.restore_layout();
        assert!(
            app4.window_size.x >= 320.0 && app4.window_size.y >= 320.0,
            "clamped, got {}x{}",
            app4.window_size.x,
            app4.window_size.y
        );
        assert!(!app4.window_start_maximized);

        // A layout file from before this feature (no "window" key)
        // restores the dock and leaves the geometry at the default.
        let legacy = serde_json::to_string(&crate::ui::dock::default_dock()).unwrap();
        std::fs::write(app.layout_path().unwrap(), legacy).unwrap();
        let mut app5 = test_app();
        app5.set_settings_dir(dir.clone());
        app5.restore_layout();
        assert_eq!(app5.window_size, egui::vec2(980.0, 760.0));
        assert!(!app5.window_start_maximized);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The dock layout persists through `layout.json`: what was
    /// visible and docked when the app exited comes back, and a
    /// corrupt file falls back to the default instead of panicking.
    #[test]
    fn dock_layout_survives_a_restart() {
        let dir = state_scratch_dir("dock-layout");
        let mut app = test_app();
        app.set_settings_dir(dir.clone());
        crate::ui::dock::show_tab(&mut app.ui.dock, crate::ui::dock::Tab::Keyboard);
        crate::ui::dock::show_tab(&mut app.ui.dock, crate::ui::dock::Tab::Tape);
        app.save_layout();
        assert!(app.layout_path().unwrap().is_file());

        let mut app2 = test_app();
        app2.set_settings_dir(dir.clone());
        app2.restore_layout();
        assert!(crate::ui::dock::tab_visible(
            &app2.ui.dock,
            crate::ui::dock::Tab::Screen
        ));
        assert!(crate::ui::dock::tab_visible(
            &app2.ui.dock,
            crate::ui::dock::Tab::Keyboard
        ));
        assert!(crate::ui::dock::tab_visible(
            &app2.ui.dock,
            crate::ui::dock::Tab::Tape
        ));

        // A corrupt layout file must not take the app down.
        std::fs::write(app.layout_path().unwrap(), "{ not json").unwrap();
        let mut app3 = test_app();
        app3.set_settings_dir(dir.clone());
        app3.restore_layout();
        assert_eq!(
            app3.ui.dock.main_surface().num_tabs(),
            1,
            "falls back to the lone screen"
        );

        // A layout without the (non-closable) screen tab is refused
        // as well — it cannot happen through the UI, only by hand
        // editing the file.
        let screenless = serde_json::to_string(&crate::ui::dock::default_dock()).unwrap();
        let screenless = screenless.replace("\"Screen\"", "\"Keyboard\"");
        std::fs::write(app.layout_path().unwrap(), screenless).unwrap();
        let mut app4 = test_app();
        app4.set_settings_dir(dir.clone());
        app4.restore_layout();
        assert!(
            crate::ui::dock::tab_visible(&app4.ui.dock, crate::ui::dock::Tab::Screen),
            "screen-less layout replaced by the default"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A breakpoint mid-frame pauses the transport and the tape deck
    /// (the pump must stay in lockstep with the machine); continuing
    /// runs on without re-tripping the same address.
    #[test]
    fn breakpoint_pauses_the_run_mid_frame() {
        let mut app = test_app();
        // The very first boot instruction (0x0000 under the startup
        // shadow map) carries a breakpoint: the first emulated frame
        // must stop right there and pause the transport.
        app.machine.toggle_breakpoint(0x0000);
        app.run_frames(5);
        assert!(!app.running, "transport paused at the breakpoint");
        assert_eq!(app.machine.breakpoint_hit(), Some(0x0000));
        assert_eq!(app.machine.cpu.pc, 0x0000);
        assert!(
            app.machine.bus.total_cycles() < pmd85_core::machine::CYCLES_PER_FRAME,
            "stopped mid-frame, not after it"
        );

        // Continuing boots on without re-tripping the hit address.
        app.machine.resume();
        app.set_running(true);
        app.run_frames(5);
        assert!(app.running, "no re-trip at the hit address");
        assert_eq!(app.machine.breakpoint_hit(), None);
        assert!(
            app.machine.bus.total_cycles() >= 4 * pmd85_core::machine::CYCLES_PER_FRAME,
            "the frames really ran"
        );
    }

    /// An app whose machine is booted past the startup shadow map
    /// (the monitor's first I/O write clears it), so programs placed
    /// in RAM actually run.
    fn ram_app() -> App {
        let mut app = test_app();
        for _ in 0..100 {
            if !app.machine.bus.memory.startup_map() {
                return app;
            }
            app.run_frames(1);
        }
        panic!("the monitor never cleared the startup shadow map");
    }

    /// Stepping through a program lands exactly where the
    /// disassembler predicted, instruction by instruction.
    #[test]
    fn stepping_follows_the_disassembly() {
        let mut app = ram_app();
        // A small straight-line program in RAM: MVI A,42 ; MVI B,10 ;
        // ADD B ; HLT. (Control flow is exercised by the step-over
        // test below.)
        let prog: [u8; 6] = [0x3E, 0x42, 0x06, 0x10, 0x80, 0x76];
        app.machine.bus.memory.ram[0x0500..0x0506].copy_from_slice(&prog);
        app.machine.cpu.pc = 0x0500;
        app.set_running(false);

        // Predict the instruction chain with the disassembler.
        let read = |a: u16| app.machine.bus.memory.read(a);
        let mut chain = Vec::new();
        let mut addr = 0x0500u16;
        for _ in 0..10 {
            chain.push(addr);
            let bytes = [
                read(addr),
                read(addr.wrapping_add(1)),
                read(addr.wrapping_add(2)),
            ];
            let (_, len) = pmd85_core::disasm::disassemble(addr, &bytes);
            addr = addr.wrapping_add(len as u16);
            if addr > 0x0505 {
                break;
            }
        }
        assert_eq!(
            chain,
            vec![0x0500, 0x0502, 0x0504, 0x0505],
            "MVI, MVI, ADD, HLT"
        );

        // Each step lands on the next predicted address; the machine
        // stays paused for it.
        for want in chain {
            assert_eq!(app.machine.cpu.pc, want);
            app.debug_step();
            assert!(!app.running, "stepping keeps the transport paused");
        }
        assert!(app.machine.cpu.halted, "the final HLT executed");
        assert_eq!(app.machine.cpu.a, 0x52, "ADD B ran: 0x42 + 0x10");
    }

    /// Step-over crosses a CALL to its return address; over anything
    /// else it is a single step.
    #[test]
    fn step_over_crosses_a_call_in_the_app() {
        let mut app = ram_app();
        // CALL 0506 ; HLT ; MVI A,FF ; RET
        let prog: [u8; 9] = [0xCD, 0x06, 0x05, 0x76, 0x00, 0x00, 0x3E, 0xFF, 0xC9];
        app.machine.bus.memory.ram[0x0500..0x0509].copy_from_slice(&prog);
        app.machine.cpu.pc = 0x0500;
        app.set_running(false);

        app.debug_step_over();
        assert_eq!(app.machine.cpu.pc, 0x0503, "the call ran whole");
        assert!(!app.machine.cpu.halted, "the HLT past it did not run");
        assert_eq!(app.machine.cpu.a, 0xFF, "the routine really executed");

        // Over the HLT: one plain step.
        app.debug_step_over();
        assert!(app.machine.cpu.halted);
    }

    #[test]
    fn audio_gating() {
        let mut app = test_app();
        assert!(app.wants_audio());
        app.set_multiplier(2.0);
        assert!(!app.wants_audio());
        app.set_multiplier(1.0);
        app.set_turbo(true);
        assert!(!app.wants_audio());
        app.set_turbo(false);
        app.set_mute(true);
        assert!(!app.wants_audio());
        app.set_mute(false);
        app.set_running(false);
        assert!(!app.wants_audio());
    }

    // ----- tape -----

    use pmd85_core::keyboard::Key;
    use pmd85_core::tape::{self, Tape};

    /// Press and release an emulated key (16 frames, like a human
    /// typist the monitor can follow), keeping the tape deck pumped.
    fn type_key(app: &mut App, k: Key) {
        app.machine.bus.keyboard.set_key(k, true);
        for _ in 0..8 {
            app.machine.step_frame();
            let _ = app.tape.pump(&mut app.machine);
        }
        app.machine.bus.keyboard.set_key(k, false);
        for _ in 0..8 {
            app.machine.step_frame();
            let _ = app.tape.pump(&mut app.machine);
        }
    }

    /// Type a command on the machine keyboard and press Enter.
    fn type_command(app: &mut App, keys: &[Key]) {
        for &k in keys {
            type_key(app, k);
        }
    }

    /// Name of tape file `file` (for assertions).
    fn file_name(app: &App, file: usize) -> String {
        let files = app.tape.files();
        app.tape.tape.blocks[files[file].block]
            .header
            .as_ref()
            .unwrap()
            .name_str()
    }

    #[test]
    fn tape_editor_roundtrip() {
        let mut app = test_app();
        let dir = std::env::temp_dir().join(format!("pmd85-tape-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let c1 = vec![0x76u8, 1, 2, 3];
        let c2 = vec![9u8, 8, 7, 6, 5];
        let mut image = Tape::default();
        image
            .blocks
            .push(tape::make_file(0, b'?', "FIRST", 0x1000, &c1).unwrap());
        image
            .blocks
            .push(tape::make_file(1, b'?', "SECOND", 0x2000, &c2).unwrap());
        let path = dir.join("tape.ptp");
        tape::save(&path, &image).unwrap();

        app.open_tape(&path).unwrap();
        assert_eq!(app.tape.files().len(), 2);
        assert!(!app.tape.dirty);
        assert_eq!(app.tape.path.as_deref(), Some(path.as_path()));
        assert_eq!(app.tape.export_file(0).unwrap(), c1);
        assert_eq!(app.tape.export_file(1).unwrap(), c2);

        // A headerless block appended after SECOND groups beneath it.
        let cont = vec![0xAAu8, 0xBB];
        let mut stream = Vec::new();
        stream.extend_from_slice(&(cont.len() as u16).to_le_bytes());
        stream.extend_from_slice(&cont);
        app.tape.tape.append_ptp_stream(&stream);
        app.tape.dirty = true; // appending is an edit
        let files = app.tape.files();
        assert_eq!(files.len(), 2);
        assert_eq!(files[1].continuations.len(), 1, "continuation block");
        app.tape.dirty = false; // not under test below

        // Move SECOND (with its continuation) up.
        app.tape.move_file(1, -1);
        assert_eq!(file_name(&app, 0), "SECOND");
        assert_eq!(file_name(&app, 1), "FIRST");
        assert_eq!(
            app.tape.files()[0].continuations.len(),
            1,
            "continuation moved along"
        );
        assert!(app.tape.dirty);
        assert_eq!(app.tape.selected, Some(0));

        // Delete SECOND; FIRST remains (one header+body block).
        app.tape.delete_file(0);
        assert_eq!(app.tape.files().len(), 1);
        assert_eq!(file_name(&app, 0), "FIRST");
        assert_eq!(app.tape.tape.blocks.len(), 1, "the file's block");

        // Save under a new name and reopen.
        let path2 = dir.join("copy.ptp");
        app.tape.save_as(&path2).unwrap();
        assert!(!app.tape.dirty);
        assert_eq!(app.tape.path.as_deref(), Some(path2.as_path()));
        app.open_tape(&path2).unwrap();
        assert_eq!(app.tape.files().len(), 1);
        assert_eq!(app.tape.export_file(0).unwrap(), c1);

        // Import appends a new file and selects it.
        let index = app
            .tape
            .import_file(5, b'?', "IMPORTD", 0x0300, b"hello tape")
            .unwrap();
        assert_eq!(index, 1);
        assert_eq!(app.tape.selected, Some(1));
        assert_eq!(app.tape.files().len(), 2);
        assert_eq!(app.tape.export_file(1).unwrap(), b"hello tape");

        // Saving without a path fails (fresh tape).
        app.new_tape();
        assert!(app.tape.save().is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tape_play_file_loads_via_the_machine() {
        let mut app = test_app();
        app.tape.flash = false; // exercise the real tape interface
        let mut content: Vec<u8> = (1..17u16).map(|i| i as u8).collect();
        content.insert(0, 0x76); // halt, so the loaded file stops cleanly
        app.tape
            .tape
            .blocks
            .push(tape::make_file(0, b'?', "LOADTST", 0x1000, &content).unwrap());

        // Boot the monitor, then type MGLD 00.
        app.run_frames(500);
        use Key::*;
        type_command(&mut app, &[M, G, L, D, Space, Digit0, Digit0, Enter]);

        // Play: the header and body are fed, then the session ends
        // (auto-stop is on by default).
        app.play_tape_file(0);
        assert!(app.tape.is_playing());
        assert!(app.machine.bus.tape.is_playing());
        for _ in 0..1500 {
            if !app.tape.is_playing() {
                break;
            }
            app.run_frames(1);
        }
        assert!(!app.tape.is_playing(), "playback session did not end");
        assert!(!app.machine.bus.tape.is_playing());
        for (i, &expect) in content.iter().enumerate() {
            assert_eq!(app.machine.bus.memory.ram[0x1000 + i], expect, "+{i}");
        }
    }

    /// Play the file with flash loading off but the emulation sped up:
    /// the deck must be pumped per emulated frame (not per rendered
    /// frame) or the load corrupts — this pins that contract.
    #[test]
    fn tape_play_file_survives_a_speedup() {
        let mut app = test_app();
        app.tape.flash = false;
        let mut content: Vec<u8> = (1..33u16).map(|i| i as u8).collect();
        content.insert(0, 0x76);
        app.tape
            .tape
            .blocks
            .push(tape::make_file(0, b'?', "FASTTST", 0x1000, &content).unwrap());
        app.set_multiplier(10.0);

        app.run_frames(500);
        use Key::*;
        type_command(&mut app, &[M, G, L, D, Space, Digit0, Digit0, Enter]);
        app.play_tape_file(0);
        for _ in 0..3000 {
            if !app.tape.is_playing() {
                break;
            }
            app.run_frames(1);
        }
        assert!(!app.tape.is_playing(), "playback session did not end");
        for (i, &expect) in content.iter().enumerate() {
            assert_eq!(app.machine.bus.memory.ram[0x1000 + i], expect, "+{i}");
        }
    }

    #[test]
    fn tape_flash_loads_the_file() {
        let mut app = test_app();
        let mut content: Vec<u8> = (1..17u16).map(|i| i as u8).collect();
        content.insert(0, 0x76);
        app.tape
            .tape
            .blocks
            .push(tape::make_file(2, b'?', "FLASHTST", 0x1000, &content).unwrap());

        // Boot the monitor and enter the MGLD 02 wait.
        app.run_frames(500);
        use Key::*;
        type_command(&mut app, &[M, G, L, D, Space, Digit0, Digit2, Enter]);

        // Flash: the header plays for real (shortened leader), the
        // body is served through the intercepts — the whole load
        // takes barely over a second of emulated time.
        assert!(app.tape.flash, "flash load is the default");
        assert!(app.tape.request_play(0, &mut app.machine));
        assert!(app.machine.bus.tape.is_playing(), "deck is feeding");
        let mut frames = 0;
        while app.tape.is_playing() {
            frames += 1;
            assert!(frames < 300, "flash load did not finish");
            app.run_frames(1);
        }
        assert!(
            frames < 120,
            "flash load took {frames} frames — not much faster than real"
        );
        assert_eq!(
            app.machine.bus.memory.ram[0x1000..0x1000 + content.len()],
            content[..]
        );

        // A few frames later the machine is back at the monitor
        // prompt, not stuck in the tape wait.
        app.run_frames(60);
        assert!(app.machine.cpu.pc >= 0xE000, "left the monitor ROM");
        let mut wait_loop = 0;
        for _ in 0..50 {
            app.run_frames(1);
            if (0xE890..0xE8B0).contains(&app.machine.cpu.pc) {
                wait_loop += 1;
            }
        }
        assert_eq!(wait_loop, 0, "still stuck in the tape wait loop");
    }

    /// The full multi-block game flow in one: MGLD flash-loads a loader
    /// whose body covers the monitor stack (so the block read's RET
    /// jumps to the vector the body placed there — autorun), and the
    /// loader itself reads the continuation block through BLKLOAD.
    #[test]
    fn tape_flash_loads_multi_block_games_with_autorun() {
        let mut app = test_app();
        const HIJACK: usize = 0xBEFB; // the EDE2 return slot on the monitor-3 stack

        // A loader program at 0x1000: mark that it ran, then read the
        // continuation block (16 bytes) into 0x2000 through BLKLOAD
        // (EDC4), then report success and halt.
        let mut loader = vec![0u8; HIJACK + 2 - 0x1000];
        loader[0..2].copy_from_slice(&[0x3E, 0xA5]); // MVI A,A5
        loader[2..5].copy_from_slice(&[0x32, 0x00, 0x30]); // STA 3000
        loader[5..8].copy_from_slice(&[0x21, 0x00, 0x20]); // LXI HL,2000
        loader[8..11].copy_from_slice(&[0x11, 0x0F, 0x00]); // LXI DE,15
        loader[11..13].copy_from_slice(&[0x0E, 0x01]); // MVI C,1
        loader[13..16].copy_from_slice(&[0xCD, 0xC4, 0xED]); // CALL EDC4
        loader[16..19].copy_from_slice(&[0xD2, 0x40, 0x10]); // JNC 1040
        loader[19..21].copy_from_slice(&[0x3E, 0xEE]); // MVI A,EE (error)
        loader[21..24].copy_from_slice(&[0x32, 0x01, 0x30]); // STA 3001
        loader[24] = 0x76; // HLT
        loader[0x40..0x43].copy_from_slice(&[0x32, 0x01, 0x30]); // STA 3001
        loader[0x43] = 0x76; // HLT
                             // Autorun vector: the block read's RET at EDE1 pops this word.
        loader[HIJACK - 0x1000] = 0x00;
        loader[HIJACK - 0x1000 + 1] = 0x10;

        let payload: Vec<u8> = (0..16u16).map(|i| 0x60 ^ i as u8).collect();
        app.tape
            .tape
            .blocks
            .push(tape::make_file(0, b'?', "AUTORUN", 0x1000, &loader).unwrap());
        let mut body = payload.clone();
        body.push(tape::crc8(&payload));
        app.tape.tape.blocks.push(tape::TapeBlock {
            header: None,
            header_bytes: Vec::new(),
            body_bytes: body,
            header_crc_ok: true,
            body_crc_ok: true,
            body_length_error: None,
            old_format: false,
        });

        app.run_frames(500);
        use Key::*;
        type_command(&mut app, &[M, G, L, D, Space, Digit0, Digit0, Enter]);
        assert!(app.tape.request_play(0, &mut app.machine));
        let mut frames = 0;
        while app.tape.is_playing() {
            frames += 1;
            assert!(frames < 300, "flash load did not finish");
            app.run_frames(1);
        }
        // The loader ran (via autorun), read the continuation block
        // through BLKLOAD, and halted on the success path.
        assert!(app.machine.cpu.halted, "the loader never halted");
        assert_eq!(
            app.machine.bus.memory.ram[0x3000], 0xA5,
            "autorun did not run"
        );
        assert_eq!(
            app.machine.bus.memory.ram[0x3001], 0x00,
            "checksum mismatch"
        );
        for (i, &expect) in payload.iter().enumerate() {
            assert_eq!(app.machine.bus.memory.ram[0x2000 + i], expect, "+{i}");
        }
    }

    #[test]
    fn machine_reset_clears_tape_playback() {
        let mut app = test_app();
        app.tape.flash = false; // exercise the real tape interface
        app.tape
            .tape
            .blocks
            .push(tape::make_file(1, b'?', "ANYFILE", 0x1000, &[0x76, 0]).unwrap());
        app.play_tape_file(0);
        assert!(app.machine.bus.tape.is_playing());
        app.machine.reset();
        let _ = app.tape.pump(&mut app.machine);
        assert!(!app.tape.is_playing(), "session survived the reset");
        assert!(!app.machine.bus.tape.is_playing());
    }

    #[test]
    fn tape_recording_is_harvested_into_the_editor() {
        let mut app = test_app();
        for _ in 0..500 {
            app.machine.step_frame();
        }
        let content: Vec<u8> = (0..17u16).map(|i| 0xC0 ^ i as u8).collect();
        for (i, &b) in content.iter().enumerate() {
            app.machine.bus.memory.ram[0x2000 + i] = b;
        }
        use Key::*;
        type_command(
            &mut app,
            &[
                M, G, S, V, Space, Digit0, Digit0, Space, Digit2, Digit0, Digit0, Digit0, Space,
                Digit2, Digit0, Digit1, Digit0, Enter,
            ],
        );
        for _ in 0..1500 {
            app.machine.step_frame();
            let _ = app.tape.pump(&mut app.machine);
            if !app.machine.bus.tape.is_recording() && !app.tape.tape.blocks.is_empty() {
                break;
            }
        }
        assert!(
            !app.machine.bus.tape.is_recording(),
            "recorder still active"
        );
        assert_eq!(app.tape.files().len(), 1, "recorded file not harvested");
        assert!(app.tape.dirty);
        assert_eq!(app.tape.export_file(0).unwrap(), content);
    }

    // ----- save states -------------------------------------------------

    use std::path::PathBuf;

    /// A scratch settings dir of the test's own (tests run in
    /// parallel; the quick slot lives next to the settings).
    fn state_scratch_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("pmd85-app-state-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_and_load_state_round_trips() {
        let mut app = test_app();
        let dir = state_scratch_dir("roundtrip");
        let path = dir.join("state.pss");

        // Boot, leave a mark and take a state.
        app.run_frames(500);
        for i in 0..32u16 {
            app.machine.bus.memory.ram[0x3000 + i as usize] = (i ^ 0x99) as u8;
        }
        let pc_at_save = app.machine.cpu.pc;
        app.save_state_to(&path).expect("save while idle");

        // Move on, then come back.
        app.run_frames(400);
        assert_ne!(app.machine.cpu.pc, pc_at_save, "the machine never moved");
        app.load_state_from(&path).expect("load round-trips");

        assert_eq!(app.machine.cpu.pc, pc_at_save);
        for i in 0..32u16 {
            assert_eq!(
                app.machine.bus.memory.ram[0x3000 + i as usize],
                (i ^ 0x99) as u8
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quick_save_and_load_use_the_settings_dir() {
        let mut app = test_app();
        let dir = state_scratch_dir("quick");
        app.set_settings_dir(dir.clone());
        app.run_frames(500);

        // Quick load before anything was saved: a notification, no
        // panic, the machine untouched.
        let pc = app.machine.cpu.pc;
        app.quick_load_state();
        assert_eq!(
            app.notifications().back().map(|(m, _)| m.clone()),
            Some("State load failed: No such file or directory (os error 2)".into())
        );
        assert_eq!(app.machine.cpu.pc, pc);

        app.quick_save_state();
        assert!(dir.join("states/quick.pss").exists(), "quick slot missing");
        assert_eq!(
            app.notifications().back().map(|(m, _)| m.clone()),
            Some("State saved (quick slot)".into())
        );

        app.run_frames(300);
        app.quick_load_state();
        assert_eq!(app.machine.cpu.pc, pc, "quick load did not restore");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_state_is_refused_while_the_tape_runs() {
        let mut app = test_app();
        let dir = state_scratch_dir("tapebusy");
        let path = dir.join("state.pss");
        app.tape.flash = false;
        app.tape
            .tape
            .blocks
            .push(tape::make_file(0, b'?', "LOADTST", 0x1000, &[0x76, 1, 2, 3]).unwrap());

        app.run_frames(500);
        use Key::*;
        type_command(&mut app, &[M, G, L, D, Space, Digit0, Digit0, Enter]);
        app.play_tape_file(0);
        assert!(app.tape.is_playing());
        assert!(!app.can_save_state(), "save offered while the tape plays");

        // Both the file save and the quick slot are refused.
        assert!(app.save_state_to(&path).is_err());
        assert!(!path.exists());
        app.quick_save_state();
        assert!(
            !dir.join("states/quick.pss").exists(),
            "quick slot written while the tape plays"
        );
        assert!(
            app.notifications()
                .back()
                .map(|(m, _)| m.contains("tape"))
                .unwrap_or(false),
            "the refusal must explain itself"
        );

        // Once the session is over, saving works again.
        for _ in 0..2000 {
            if !app.tape.is_playing() {
                break;
            }
            app.run_frames(1);
        }
        assert!(app.can_save_state());
        app.save_state_to(&path)
            .expect("save after the session ends");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_state_reports_corrupt_files_and_repaints() {
        let mut app = test_app();
        let dir = state_scratch_dir("corrupt");
        let bad = dir.join("bad.pss");
        std::fs::write(&bad, b"definitely not a save state").unwrap();

        app.run_frames(500);
        let pc = app.machine.cpu.pc;
        let cycles = app.machine.bus.total_cycles();
        let err = app.load_state_from(&bad).unwrap_err();
        assert_eq!(err, "State: not a PMD 85 save state");
        assert_eq!(app.machine.cpu.pc, pc, "corrupt load mutated the machine");
        assert_eq!(app.machine.bus.total_cycles(), cycles);

        // Loading while paused repaints the screen immediately.
        app.set_running(false);
        assert!(app.decode_buf.is_empty(), "nothing decoded yet");
        let good = dir.join("good.pss");
        app.save_state_to(&good).unwrap();
        app.machine.bus.memory.ram[0xC000] ^= 0xFF; // scribble VRAM
        app.load_state_from(&good).unwrap();
        assert!(
            !app.decode_buf.is_empty(),
            "the screen was not decoded on load"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ----- session (remembered across restarts) ------------------------

    #[test]
    fn machine_config_changes_persist() {
        let mut app = test_app();
        let dir = state_scratch_dir("config");
        app.set_settings_dir(dir.clone());

        // Switch to a 85-2 with the default monitor: the choice must
        // become the boot configuration for the next start.
        let cfg = crate::app::MachineConfig {
            model: pmd85_core::Model::Pmd852,
            monitor: None,
            rom_module: None,
        };
        app.apply_config(cfg);
        assert_eq!(app.machine.model(), pmd85_core::Model::Pmd852);

        let loaded = crate::config::AppSettings::load(&dir);
        assert_eq!(
            pmd85_core::Model::from_str_loose(&loaded.model),
            Some(pmd85_core::Model::Pmd852)
        );
        assert_eq!(loaded.monitor, None);

        // A fresh app built from those settings boots the 85-2.
        let app2 = app_with_settings(loaded);
        assert_eq!(app2.config.model, pmd85_core::Model::Pmd852);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_round_trips_the_tape() {
        let mut app = test_app();
        let dir = state_scratch_dir("session");
        app.set_settings_dir(dir.clone());

        // A tape with unsaved edits, a path and a selection.
        let tape_path = dir.join("mygame.ptp");
        app.tape
            .tape
            .blocks
            .push(tape::make_file(0, b'?', "GAME", 0x1000, &[1, 2, 3, 4]).unwrap());
        app.tape.tape.blocks[0].body_bytes[1] ^= 0xFF; // an unsaved edit
        app.tape.path = Some(tape_path.clone());
        app.tape.dirty = true;
        app.tape.selected = Some(0);
        app.save_session();

        let mut app2 = test_app();
        app2.set_settings_dir(dir.clone());
        app2.restore_session();
        assert_eq!(app2.tape.tape.blocks.len(), 1);
        assert_eq!(
            app2.tape.tape.blocks[0].body_bytes,
            app.tape.tape.blocks[0].body_bytes
        );
        assert_eq!(app2.tape.path, Some(tape_path), "Save keeps writing there");
        assert!(app2.tape.dirty, "the dirty dot survived");
        assert_eq!(app2.tape.selected, Some(0));

        // An untitled tape (no path) round-trips too.
        app2.tape.clear();
        app2.tape
            .tape
            .blocks
            .push(tape::make_file(3, b'?', "SCRATCH", 0x2000, &[9]).unwrap());
        app2.tape.dirty = true;
        app2.save_session();
        let mut app3 = test_app();
        app3.set_settings_dir(dir.clone());
        app3.restore_session();
        assert_eq!(app3.tape.path, None);
        assert_eq!(app3.tape.files().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_or_missing_sessions_are_a_clean_start() {
        let mut app = test_app();
        let dir = state_scratch_dir("corrupt-session");
        app.set_settings_dir(dir.clone());

        // Nothing saved yet: clean, empty tape.
        app.restore_session();
        assert!(app.tape.tape.blocks.is_empty());
        assert_eq!(app.tape.path, None);

        // Garbage snapshot files must not panic or wedge the editor.
        std::fs::create_dir_all(dir.join("session")).unwrap();
        std::fs::write(dir.join("session/tape.ptp"), b"not a tape").unwrap();
        std::fs::write(dir.join("session/session.json"), "{not json").unwrap();
        app.restore_session();
        assert!(
            app.tape.tape.blocks.is_empty(),
            "corrupt tape snapshot ignored"
        );
        assert_eq!(app.tape.path, None, "corrupt sidecar ignored");
        assert!(!app.tape.dirty);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
