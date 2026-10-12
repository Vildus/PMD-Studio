//! The assembler IDE: the Project tab (toolbar, file tree, source
//! editor) and its shared state with the Problems tab.
//!
//! The editor is an `egui::TextEdit` laid out by the assembler's own
//! lexer — the highlighting is built from the very tokens the
//! assembler sees, so what looks right is what assembles right (the
//! same idea as the disassembly view). Line numbers sit in a
//! separate column (numbers inside the layouter would break the
//! cursor mapping), diagnostics get a red underline under their
//! token, and the whole buffer is content-sized so the tab's own
//! scroll area scrolls gutter and text together.
//!
//! Shortcuts (Ctrl+B/S/F/G, F5, Ctrl+F5) are consumed here, inside
//! the tab's draw — they only exist while the Project tab is visible,
//! so they never leak into the emulated keyboard.

use std::collections::HashSet;
use std::sync::Arc;

use egui::{Id, TextEdit};
use egui_phosphor::regular as icon;
use pmd85_asm::{is_directive, lex_lines, lookup_mnemonic, Tok};

use crate::app::App;
use crate::project::{FindState, Target};
use crate::ui::theme::Theme;

/// A pending jump to a source position, applied by the editor on its
/// next draw: (file path, 1-based line, 1-based column).
#[derive(Clone, Debug, PartialEq)]
pub struct Jump {
    pub file: String,
    pub line: usize,
    pub col: usize,
}

/// The Project tab body.
pub fn ui(ui: &mut egui::Ui, app: &mut App) {
    let theme = app.active_theme_data();
    if app.project.is_none() {
        welcome(ui, app, &theme);
        return;
    }

    // Shortcuts first, before any child widget could eat the keys:
    // they belong to the tab, not to whatever has focus.
    shortcuts(ui.ctx(), app);

    let mut jump = app.ui.project_jump.take();
    goto_popup(ui.ctx(), app, &mut jump);
    target_popup(ui.ctx(), app);

    toolbar(ui, app, &theme);
    ui.add_space(4.0);
    find_bar(ui, app, &theme, &mut jump);
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        file_tree(ui, app, &theme);
        editor(ui, app, &theme, jump);
    });
}

/// The panel shown in place of the IDE when no project is open.
fn welcome(ui: &mut egui::Ui, app: &mut App, theme: &Theme) {
    ui.vertical_centered(|ui| {
        ui.add_space(30.0);
        ui.label(
            egui::RichText::new("PMD 85 assembler IDE")
                .size(18.0)
                .color(Theme::color(&theme.accent)),
        );
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(
                "A project is a folder with a project.json manifest and\n\
                 .asm sources. Open one, or create a fresh project.",
            )
            .color(Theme::color(&theme.text_weak)),
        );
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui
                .button(format!("{} Open project\u{2026}", icon::FOLDER_OPEN))
                .clicked()
            {
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    app.open_project(&dir);
                }
            }
            if ui
                .button(format!("{} New project\u{2026}", icon::FILE_PLUS))
                .clicked()
            {
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    app.create_project(&dir);
                }
            }
        });
        ui.add_space(12.0);
        ui.label(
            egui::RichText::new(
                "Build with Ctrl+B \u{b7} run authentic (monitor tape load) with F5 \u{b7} \n\
                 run direct (RAM inject) with Ctrl+F5",
            )
            .small()
            .color(Theme::color(&theme.text_weak)),
        );
    });
}

/// Tab-global shortcuts, consumed so no widget (or the emulated
/// keyboard, when nothing is focused) sees them.
fn shortcuts(ctx: &egui::Context, app: &mut App) {
    use egui::{Key, KeyboardShortcut, Modifiers};
    let ctrl = |key: Key| KeyboardShortcut::new(Modifiers::CTRL, key);
    let consume = |s: &KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(s));
    if consume(&ctrl(Key::B)) {
        app.build_project();
    }
    if consume(&ctrl(Key::S)) {
        app.save_project_file();
    }
    if consume(&ctrl(Key::F)) {
        if let Some(project) = app.project.as_mut() {
            project.find.open = !project.find.open;
            if project.find.open {
                project.find.active = 0;
                ctx.memory_mut(|m| m.request_focus(find_query_id()));
            }
        }
    }
    if consume(&ctrl(Key::G)) {
        if let Some(project) = app.project.as_mut() {
            project.goto_open = true;
        }
    }
    if consume(&KeyboardShortcut::new(Modifiers::NONE, Key::F5)) {
        app.run_project();
    }
    if consume(&KeyboardShortcut::new(Modifiers::CTRL, Key::F5)) {
        app.run_project_direct();
    }
}

/// The editor toolbar: build and run actions, then file actions, then
/// the build status line.
fn toolbar(ui: &mut egui::Ui, app: &mut App, theme: &Theme) {
    ui.horizontal_wrapped(|ui| {
        if ui
            .button(format!("{} Build", icon::STEPS))
            .on_hover_text("Assemble the current buffers (Ctrl+B)")
            .clicked()
        {
            app.build_project();
        }
        if ui
            .button(format!("{} Run", icon::PLAY))
            .on_hover_text(
                "Build, then load through the monitor: the file goes on the\n\
                 (flash) tape and the monitor's MGLD command is typed for\n\
                 you \u{2014} checksums and autorun run for real (F5)",
            )
            .clicked()
        {
            app.run_project();
        }
        if ui
            .button(format!("{} Run direct", icon::SKIP_FORWARD))
            .on_hover_text(
                "Build, then inject the image into RAM at its address and\n\
                 start the CPU at the entry point \u{2014} no monitor, no tape\n\
                 (Ctrl+F5)",
            )
            .clicked()
        {
            app.run_project_direct();
        }

        ui.separator();

        if ui
            .button(format!("{} New file", icon::FILE_PLUS))
            .on_hover_text("Add an empty source file to the project")
            .clicked()
        {
            if let Some(project) = app.project.as_mut() {
                project.new_name.clear();
            }
        }
        if ui
            .button(format!("{} Open\u{2026}", icon::FOLDER_OPEN))
            .on_hover_text("Add an existing file from the project directory")
            .clicked()
        {
            let dir = app
                .project
                .as_ref()
                .map(|p| p.dir.clone())
                .unwrap_or_default();
            if let Some(path) = rfd::FileDialog::new()
                .set_directory(&dir)
                .pick_file()
                .and_then(|p| {
                    p.strip_prefix(&dir)
                        .ok()
                        .and_then(|r| r.to_str().map(str::to_string))
                })
            {
                if let Some(project) = app.project.as_mut() {
                    if let Err(e) = project.open_file(&path) {
                        app.notify(e);
                    }
                }
            }
        }
        let save_label = app
            .project
            .as_ref()
            .and_then(|p| {
                p.active
                    .and_then(|a| p.files.get(a))
                    .filter(|f| f.dirty)
                    .map(|f| f.path.clone())
            })
            .map(|path| format!("{} Save {path} \u{2022}", icon::FLOPPY_DISK))
            .unwrap_or_else(|| format!("{} Save", icon::FLOPPY_DISK));
        if ui
            .button(save_label)
            .on_hover_text("Write the edited buffer to disk (Ctrl+S)")
            .clicked()
        {
            app.save_project_file();
        }
        if ui.button(format!("{} Close project", icon::X)).clicked() {
            app.close_project();
            return;
        }

        ui.separator();

        // The target chip opens the target editor popup.
        let target = app
            .project
            .as_ref()
            .map(|p| match &p.manifest.target {
                Target::Tape { name, autostart } => format!(
                    "target: tape {name}{}",
                    if *autostart { " (autostart)" } else { "" }
                ),
                t => format!("target: {}", t.kind()),
            })
            .unwrap_or_default();
        if ui
            .button(egui::RichText::new(target).color(Theme::color(&theme.text_weak)))
            .on_hover_text("What the project builds into \u{2014} click to change")
            .clicked()
        {
            if let Some(project) = app.project.as_mut() {
                project.target_open = true;
            }
        }

        // Build status on the right.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let status = app
                .project
                .as_ref()
                .and_then(|p| p.build.as_ref())
                .map(|b| {
                    if b.has_errors() {
                        (
                            format!("{} errors", b.asm.diagnostics.len()),
                            Theme::color(&theme.danger),
                        )
                    } else {
                        (
                            format!("OK \u{b7} entry {:04X}", b.asm.entry.unwrap_or_default()),
                            Theme::color(&theme.accent),
                        )
                    }
                });
            if let Some((text, color)) = status {
                ui.label(egui::RichText::new(text).size(12.0).color(color));
            }
        });
    });
}

/// The file list and the new-file row.
fn file_tree(ui: &mut egui::Ui, app: &mut App, theme: &Theme) {
    let mut notify: Option<String> = None;
    {
        let Some(project) = app.project.as_mut() else {
            return;
        };
        ui.vertical(|ui| {
            ui.set_width(150.0);
            ui.label(
                egui::RichText::new("sources")
                    .small()
                    .color(Theme::color(&theme.text_weak)),
            );
            for i in 0..project.files.len() {
                let file = &project.files[i];
                let label = if file.dirty {
                    format!("{} \u{2022}", file.path)
                } else {
                    file.path.clone()
                };
                let selected = project.active == Some(i);
                let tooltip = format!("{}/{}", project.dir.display(), file.path);
                if ui
                    .selectable_label(selected, egui::RichText::new(label).monospace().size(12.0))
                    .on_hover_text(tooltip)
                    .clicked()
                {
                    project.active = Some(i);
                }
            }
            // New file: name + add.
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let edit = ui.add(
                    TextEdit::singleline(&mut project.new_name)
                        .id(Id::new("project-new-file"))
                        .hint_text("name.asm")
                        .desired_width(80.0),
                );
                let field_focused = ui
                    .ctx()
                    .memory(|m| m.has_focus(Id::new("project-new-file")));
                let wants = ui
                    .button(format!("{} add", icon::FILE_CODE))
                    .on_hover_text("Create the file in the project directory")
                    .clicked()
                    || (field_focused
                        && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        && project.new_name.trim().ends_with(".asm"));
                let _ = edit;
                if wants {
                    let name = project.new_name.trim().to_string();
                    match project.new_file(&name) {
                        Ok(()) => project.new_name.clear(),
                        Err(e) => notify = Some(e),
                    }
                }
            });
        });
    }
    if let Some(e) = notify {
        app.notify(e);
    }
}

/// The source editor: gutter + highlighted TextEdit.
fn editor(ui: &mut egui::Ui, app: &mut App, theme: &Theme, jump: Option<Jump>) {
    let Some(project) = app.project.as_mut() else {
        return;
    };
    let Some(idx) =
        project
            .active
            .filter(|&i| i < project.files.len())
            .or(if project.files.is_empty() {
                None
            } else {
                Some(0)
            })
    else {
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("no source file \u{2014} create one on the left").weak())
        });
        return;
    };
    let path = project.files[idx].path.clone();

    // The assembler's file index for this buffer (INCLUDEd files get
    // their own); its diagnostics underline their tokens.
    let underlines: HashSet<(usize, usize)> = match &project.build {
        Some(build) => match build.asm.files.iter().position(|f| *f == path) {
            Some(file) => build
                .asm
                .diagnostics
                .iter()
                .filter(|d| d.file == file)
                .map(|d| (d.line, d.col))
                .collect(),
            None => HashSet::new(),
        },
        None => HashSet::new(),
    };
    let find = project.find.clone();
    let palette = Palette::from(theme);
    let mut text = std::mem::take(&mut project.files[idx].text);

    let id = Id::new(("project-editor", &path));
    let font_id = egui::TextStyle::Monospace.resolve(ui.style());
    let lines = text.lines().count().max(1);

    let mut layouter =
        |ui: &egui::Ui, buf: &dyn egui::TextBuffer, _wrap: f32| -> Arc<egui::Galley> {
            let job = highlight_job(buf.as_str(), &font_id, &palette, &underlines, &find);
            ui.ctx().fonts_mut(|f| f.layout_job(job))
        };

    let response = ui.horizontal(|ui| {
        // The gutter and the editor form one row: no spacing between
        // them, the same font, and the TextEdit's own top frame
        // padding (its default margin is 2 px) added above the line
        // numbers so the rows line up.
        ui.spacing_mut().item_spacing.x = 0.0;
        let width = gutter_width(ui, &font_id, lines);
        ui.vertical(|ui| {
            ui.set_width(width);
            ui.add_space(2.0);
            gutter(ui, &font_id, lines, theme);
        });
        ui.add(
            TextEdit::multiline(&mut text)
                .id(id)
                .font(egui::TextStyle::Monospace)
                .desired_rows(lines)
                .desired_width(f32::INFINITY)
                .lock_focus(true)
                .layouter(&mut layouter),
        )
    });

    let changed = response.inner.changed();
    let file = &mut project.files[idx];
    file.text = text;
    if changed {
        file.dirty = true;
    }

    // A pending jump: cursor onto the spot, view scrolled to it.
    if let Some(jump) = jump.filter(|j| j.file == path) {
        let char_pos = char_offset(&file.text, jump.line, jump.col);
        set_cursor(ui.ctx(), id, char_pos);
        let row = ui.ctx().fonts_mut(|f| f.row_height(&font_id));
        let rect = response
            .inner
            .rect
            .translate(egui::vec2(0.0, (jump.line as f32 - 0.5) * row));
        ui.scroll_to_rect(rect, Some(egui::Align::Center));
    }
}

/// The line-number column: one weak monospace layout job.
fn gutter(ui: &mut egui::Ui, font_id: &egui::FontId, lines: usize, theme: &Theme) {
    let weak = Theme::color(&theme.text_weak);
    let mut job = egui::epaint::text::LayoutJob::default();
    let fmt = egui::TextFormat::simple(font_id.clone(), weak);
    let width = lines.to_string().len();
    for line in 1..=lines {
        let label = format!("{line:>width$}\n");
        job.append(&label, 0.0, fmt.clone());
    }
    ui.add(egui::Label::new(egui::WidgetText::LayoutJob(Arc::new(job))))
        .on_hover_cursor(egui::CursorIcon::Default);
}

/// Gutter width for `lines` lines (digits plus breathing room).
fn gutter_width(ui: &mut egui::Ui, font_id: &egui::FontId, lines: usize) -> f32 {
    let digits = lines.to_string().len().max(3);
    ui.ctx()
        .fonts_mut(|f| digits as f32 * f.glyph_width(font_id, '0') + 8.0)
}

/// The find/replace bar. Returns nothing; communicates through the
/// project's find state and the jump it hands to the editor.
fn find_bar(ui: &mut egui::Ui, app: &mut App, theme: &Theme, jump: &mut Option<Jump>) {
    let Some(project) = app.project.as_mut() else {
        return;
    };
    if !project.find.open || project.active.is_none() {
        return;
    }
    let Some(idx) = project.active else { return };

    // Match offsets in the active buffer, recomputed every frame: the
    // count feeds the label, the offsets feed the highlighter and
    // next/prev.
    let query = project.find.query.clone();
    let text_len = project.files[idx].text.len();
    let matches: Vec<usize> = if query.is_empty() {
        Vec::new()
    } else {
        project.files[idx]
            .text
            .match_indices(&query)
            .map(|(byte, _)| byte)
            .collect()
    };
    project.find.matches = matches.len();

    let active = if matches.is_empty() {
        None
    } else {
        Some(matches[project.find.active % matches.len()])
    };
    project.find.active_match = active;

    let weak = Theme::color(&theme.text_weak);
    ui.horizontal_wrapped(|ui| {
        let query_edit = ui.add(
            TextEdit::singleline(&mut project.find.query)
                .id(find_query_id())
                .hint_text("Find")
                .desired_width(110.0),
        );
        // Enter cycles forward, Shift+Enter back, Escape closes.
        if query_edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            if ui.input(|i| i.modifiers.shift) {
                cycle_find(&mut project.find, &matches, false);
            } else {
                cycle_find(&mut project.find, &matches, true);
            }
            query_edit.request_focus();
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            project.find.open = false;
            return;
        }
        let count = if matches.is_empty() {
            "no matches".to_string()
        } else {
            format!(
                "{}/{}",
                project.find.active % matches.len() + 1,
                matches.len()
            )
        };
        ui.label(egui::RichText::new(count).small().color(weak));
        if ui
            .button(egui::RichText::new(icon::ARROW_UP).size(12.0))
            .on_hover_text("Previous match (Shift+Enter)")
            .clicked()
        {
            cycle_find(&mut project.find, &matches, false);
        }
        if ui
            .button(egui::RichText::new(icon::ARROW_DOWN).size(12.0))
            .on_hover_text("Next match (Enter)")
            .clicked()
        {
            cycle_find(&mut project.find, &matches, true);
        }

        ui.separator();

        ui.add(
            TextEdit::singleline(&mut project.find.replace)
                .id(Id::new("project-replace"))
                .hint_text("Replace with")
                .desired_width(110.0),
        );
        if ui
            .button("Replace")
            .on_hover_text("Replace the active match")
            .clicked()
        {
            if let Some(byte) = active {
                let file = &mut project.files[idx];
                let end = (byte + query.len()).min(file.text.len());
                file.text.replace_range(byte..end, &project.find.replace);
                file.dirty = true;
            }
        }
        if ui.button("Replace all").clicked() && !query.is_empty() {
            let replace_with = project.find.replace.clone();
            let file = &mut project.files[idx];
            let replaced = file.text.replace(&query, &replace_with);
            file.text = replaced;
            file.dirty = true;
        }
        if ui
            .button(egui::RichText::new(icon::X).size(12.0))
            .on_hover_text("Close the find bar (Ctrl+F)")
            .clicked()
        {
            project.find.open = false;
        }
    });

    // The active match becomes this frame's jump so the editor
    // scrolls to it and puts the cursor there.
    if let Some(byte) = active {
        let file = &project.files[idx];
        if byte <= text_len {
            let before = &file.text[..byte];
            let line = before.matches('\n').count() + 1;
            let col = before
                .rsplit('\n')
                .next()
                .map(|l| l.chars().count() + 1)
                .unwrap_or(1);
            *jump = Some(Jump {
                file: file.path.clone(),
                line,
                col,
            });
        }
    }
}

/// Step the active find match, wrapping around.
fn cycle_find(find: &mut FindState, matches: &[usize], forward: bool) {
    if matches.is_empty() {
        return;
    }
    find.active = if forward {
        (find.active + 1) % matches.len()
    } else {
        (find.active + matches.len() - 1) % matches.len()
    };
}

/// The id of the find query field (for focus requests).
fn find_query_id() -> Id {
    Id::new("project-find-query")
}

/// The go-to-line popup.
fn goto_popup(ctx: &egui::Context, app: &mut App, jump: &mut Option<Jump>) {
    let mut go = false;
    let mut bad_input: Option<String> = None;
    let keep;
    {
        let Some(project) = app.project.as_mut() else {
            return;
        };
        if !project.goto_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Go to line")
            .id(Id::new("project-goto"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let edit = ui.add(
                        TextEdit::singleline(&mut project.goto_line)
                            .id(Id::new("project-goto-input"))
                            .hint_text("line")
                            .desired_width(60.0),
                    );
                    if ui.button("Go").clicked()
                        || (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    {
                        go = true;
                    }
                });
                ui.label(
                    egui::RichText::new("Enter jumps; the popup stays for the next jump.")
                        .small()
                        .weak(),
                );
            });
        keep = open;
        if go {
            if let Some(idx) = project.active {
                match project.goto_line.trim().parse::<usize>() {
                    Ok(line) if line >= 1 => {
                        *jump = Some(Jump {
                            file: project.files[idx].path.clone(),
                            line,
                            col: 1,
                        });
                    }
                    _ => bad_input = Some(project.goto_line.clone()),
                }
            }
        }
    }
    if let Some(input) = bad_input {
        app.notify(format!("{input:?} is not a line number"));
        go = false;
    }
    if let Some(project) = app.project.as_mut() {
        project.goto_open = keep && !go;
    }
}

/// The build-target editor popup.
fn target_popup(ctx: &egui::Context, app: &mut App) {
    let mut saved = false;
    {
        let Some(project) = app.project.as_mut() else {
            return;
        };
        if !project.target_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Build target")
            .id(Id::new("project-target"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                let selected = project.manifest.target.kind();
                egui::ComboBox::from_id_salt("project-target-kind")
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        let mut pick = |t: Target, ui: &mut egui::Ui| {
                            ui.selectable_value(&mut project.manifest.target, t.clone(), t.kind());
                        };
                        pick(
                            Target::Tape {
                                name: "PROGRAM".into(),
                                autostart: true,
                            },
                            ui,
                        );
                        pick(Target::Bin, ui);
                        pick(Target::RomModule, ui);
                    });
                match &mut project.manifest.target {
                    Target::Tape { name, autostart } => {
                        ui.horizontal(|ui| {
                            ui.label("Tape file name:");
                            ui.add(
                                TextEdit::singleline(name)
                                    .id(Id::new("project-target-name"))
                                    .desired_width(90.0)
                                    .hint_text("PROGRAM"),
                            );
                        });
                        ui.checkbox(autostart, "Autostart (autorun loader)");
                        ui.label(
                            egui::RichText::new(
                                "The name is what the monitor shows; one to eight\n\
                                 letters and digits.\n\
                                 Autostart saves the wiki's autorun loader at 7FD7h,\n\
                                 which takes over the block reader's return slot\n\
                                 and jumps into the program after the load.",
                            )
                            .small()
                            .weak(),
                        );
                    }
                    Target::Bin => {
                        ui.label(
                            "A raw binary: the image lands in RAM at its\n\
                                  address, the CPU starts at the entry point.",
                        );
                    }
                    Target::RomModule => {
                        ui.label(
                            egui::RichText::new(
                                "The image is plugged into the module connector and\n\
                                  the machine rebooted; the monitor detects it and\n\
                                  transfers the program to RAM by itself.\n\
                                  On the 85-1 the app types the BASIC G transfer\n\
                                  command (Monitor 1 has no boot detection).",
                            )
                            .small()
                            .weak(),
                        );
                    }
                }
                ui.separator();
                if ui.button("Save").clicked() {
                    if let Target::Tape { name, .. } = &mut project.manifest.target {
                        *name = normalize_tape_name(name);
                    }
                    saved = true;
                }
            });
        if !open {
            project.target_open = false;
        }
    }
    if saved {
        if let Some(project) = app.project.as_mut() {
            match project.save_manifest() {
                Ok(()) => {
                    project.target_open = false;
                    app.notify("Target saved");
                }
                Err(e) => app.notify(e),
            }
        }
    }
}

/// A monitor-plausible tape file name: uppercase letters and digits,
/// at most 8 characters.
fn normalize_tape_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>()
        .to_uppercase()
}

/// Park the text cursor at a character offset in the editor `id`.
fn set_cursor(ctx: &egui::Context, id: Id, char_pos: usize) {
    use egui::text::{CCursor, CCursorRange};
    let mut state = TextEdit::load_state(ctx, id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(CCursorRange::one(CCursor::new(char_pos))));
    state.store(ctx, id);
}

/// The character offset of (1-based) `line` and `col` in `text`.
fn char_offset(text: &str, line: usize, col: usize) -> usize {
    let mut off = 0;
    for (i, l) in text.lines().enumerate() {
        if i + 1 == line {
            return off + (col.saturating_sub(1)).min(l.chars().count());
        }
        off += l.chars().count() + 1;
    }
    off
}

/// The highlighting palette.
struct Palette {
    text: egui::Color32,
    weak: egui::Color32,
    accent: egui::Color32,
    accent_dim: egui::Color32,
    warn: egui::Color32,
    danger: egui::Color32,
}

impl Palette {
    fn from(theme: &Theme) -> Self {
        Palette {
            text: Theme::color(&theme.text),
            weak: Theme::color(&theme.text_weak),
            accent: Theme::color(&theme.accent),
            accent_dim: Theme::color(&theme.accent_dim),
            warn: Theme::color(&theme.warn),
            danger: Theme::color(&theme.danger),
        }
    }
}

/// One colored run of source.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Plain,
    Mnemonic,
    Directive,
    Label,
    Ident,
    Literal,
    Comment,
}

/// Lay out one source buffer: mnemonics glow, directives italicize,
/// literals are warm, comments dim, find matches are backlit, and
/// the tokens carrying a diagnostic get a red underline.
fn highlight_job(
    text: &str,
    font_id: &egui::FontId,
    palette: &Palette,
    underlines: &HashSet<(usize, usize)>,
    find: &FindState,
) -> egui::epaint::text::LayoutJob {
    let mut job = egui::epaint::text::LayoutJob::default();
    let base_fmt = egui::TextFormat::simple(font_id.clone(), palette.text);

    // The assembler's view of the text: the same tokens the build
    // will see (a scratch file index of 0; positions are per line).
    let mut diags = Vec::new();
    let lines = lex_lines(0, text, &mut diags);
    let raw: Vec<&str> = text.split_inclusive('\n').collect();

    let mut base = 0usize; // byte offset of the current line start
    for (i, line) in raw.iter().enumerate() {
        let lineno = i + 1;
        let nchars = line.chars().count();
        let lexed = lines.get(i);

        // Byte offset of each char boundary (chars -> bytes).
        let mut at_byte = Vec::with_capacity(nchars + 1);
        for (byte, _) in line.char_indices() {
            at_byte.push(byte);
        }
        at_byte.push(line.len());
        let to_byte = |c: usize| at_byte[c.min(nchars)];

        // Segments in order: tokens, comment, gaps between.
        let mut segments: Vec<(usize, usize, Kind, bool)> = Vec::new();
        let mut cursor = 0usize;
        if let Some(lexed) = lexed {
            let tokens = &lexed.tokens;
            for i in 0..tokens.len() {
                let t = &tokens[i];
                let next = tokens.get(i + 1);
                let start = t.span.col - 1;
                let end = start + t.len;
                if start > cursor {
                    segments.push((cursor, start, Kind::Plain, false));
                }
                if end > start {
                    let kind = match &t.tok {
                        Tok::Number(_) | Tok::Str(_) | Tok::Dollar => Kind::Literal,
                        Tok::Ident(word) => {
                            if next.is_some_and(|n| matches!(n.tok, Tok::Colon)) {
                                Kind::Label
                            } else if lookup_mnemonic(word).is_some() {
                                Kind::Mnemonic
                            } else if is_directive(word) {
                                Kind::Directive
                            } else if i == 0
                                && next.is_some_and(|n| {
                                    matches!(&n.tok, Tok::Ident(w) if lookup_mnemonic(w).is_some())
                                })
                            {
                                // A label without its colon.
                                Kind::Label
                            } else {
                                Kind::Ident
                            }
                        }
                        _ => Kind::Plain,
                    };
                    let underline = underlines.contains(&(lineno, t.span.col));
                    segments.push((start, end, kind, underline));
                }
                cursor = end.max(cursor);
            }
            if let Some(comment_col) = lexed.comment {
                let start = comment_col - 1;
                if start > cursor {
                    segments.push((cursor, start, Kind::Plain, false));
                }
                segments.push((start, nchars, Kind::Comment, false));
                cursor = nchars;
            }
        }
        if nchars > cursor {
            segments.push((cursor, nchars, Kind::Plain, false));
        }

        // Find matches on this line: (start char, end char, active).
        let mut matches: Vec<(usize, usize, bool)> = Vec::new();
        if find.open && !find.query.is_empty() {
            let query = find.query.as_str();
            let mut from = 0;
            while let Some(rel) = line[from..].find(query) {
                let byte = from + rel;
                let start = line[..byte].chars().count();
                let end = start + query.chars().count();
                matches.push((start, end, find.active_match == Some(base + byte)));
                from = byte + query.len().max(1);
            }
        }

        // Emit, splitting runs on match edges so matches get their
        // background fill.
        for (start, end, kind, underline) in segments {
            let color = match kind {
                Kind::Plain | Kind::Label | Kind::Ident => palette.text,
                Kind::Mnemonic => palette.accent,
                Kind::Directive | Kind::Comment => palette.weak,
                Kind::Literal => palette.warn,
            };
            let mut piece = start;
            while piece < end {
                let mut stop = end;
                let mut active = false;
                let mut in_match = false;
                for &(ms, me, act) in &matches {
                    if ms > piece && ms < stop {
                        stop = ms;
                    }
                    if me > piece && me < stop {
                        stop = me;
                    }
                    if ms <= piece && piece < me {
                        in_match = true;
                        active = act;
                        stop = stop.min(me);
                    }
                }
                let format = egui::TextFormat {
                    color,
                    italics: kind == Kind::Directive,
                    background: if active {
                        palette.accent
                    } else if in_match {
                        palette.accent_dim
                    } else {
                        egui::Color32::TRANSPARENT
                    },
                    underline: if underline {
                        egui::Stroke::new(1.0, palette.danger)
                    } else {
                        egui::Stroke::NONE
                    },
                    ..base_fmt.clone()
                };
                job.append(&line[to_byte(piece)..to_byte(stop)], 0.0, format);
                piece = stop;
            }
        }

        base += line.len();
    }
    job
}
