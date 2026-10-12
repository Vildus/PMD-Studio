//! The assembler IDE: the Project tab (toolbar, file tree, source
//! editor) and its shared state with the Problems tab.
//!
//! The editor is an `egui::TextEdit` laid out by the assembler's own
//! lexer — the highlighting is built from the very tokens the
//! assembler sees, so what looks right is what assembles right (the
//! same idea as the disassembly view). Line numbers sit in a
//! separate column (numbers inside the layouter would break the
//! cursor mapping), diagnostics get a red underline under their
//! token, and the editor scrolls inside its own scroll area — the
//! toolbar, find bar and file tree stay put (the tab body itself
//! does not scroll).
//!
//! There are no keyboard shortcuts to speak of: the host keyboard is
//! the emulated machine's (F5 is its K4 key, Ctrl its STOP key), so
//! the IDE is driven entirely by its buttons.

use std::collections::HashSet;
use std::sync::Arc;

use egui::{Id, TextEdit};
use egui_phosphor::regular as icon;
use pmd85_asm::{is_directive, lex_lines, lookup_mnemonic, LexLine, Tok};

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

    let mut jump = app.ui.project_jump.take();
    goto_popup(ui.ctx(), app, &mut jump);
    target_popup(ui.ctx(), app);
    file_popups(ui.ctx(), app);

    toolbar(ui, app, &theme);
    ui.add_space(4.0);
    find_bar(ui, app, &theme, &mut jump);
    ui.add_space(2.0);
    // The file tree is a fixed-width left panel, not a member of a
    // horizontal row: a row sizes its height to its tallest child, so
    // the tree's content height would cap the editor. A panel always
    // takes the full tab height; the editor then fills what is left.
    egui::Panel::left(Id::new("project-files"))
        .exact_size(150.0)
        .resizable(false)
        .frame(egui::Frame::NONE)
        .show_separator_line(false)
        .show(ui, |ui| file_tree(ui, app, &theme));
    editor(ui, app, &theme, jump);
}

/// The unsaved-changes guard, shown above everything (the app close
/// raises it too, not just the project close).
pub fn close_guard_modal(ctx: &egui::Context, app: &mut App) {
    let Some(after) = app.close_guard.clone() else {
        return;
    };
    let dirty: Vec<String> = app
        .project
        .iter()
        .flat_map(|p| p.files.iter().filter(|f| f.dirty))
        .map(|f| f.path.clone())
        .collect();
    let subject = match after {
        crate::app::AfterClose::Exit => "the app",
        _ => "the project",
    };
    let mut resolved = false;
    let mut save = false;
    let mut cancelled = false;
    egui::Window::new("Unsaved changes")
        .id(Id::new("project-close-guard"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(format!(
                "{} file(s) with unsaved edits:",
                dirty.len().max(1)
            ));
            ui.label(
                egui::RichText::new(dirty.join(", "))
                    .monospace()
                    .size(12.0)
                    .weak(),
            );
            ui.label(format!("Close {} without saving them?", subject));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Save and close").clicked() {
                    resolved = true;
                    save = true;
                }
                if ui.button("Close without saving").clicked() {
                    resolved = true;
                }
                if ui.button("Cancel").clicked() {
                    cancelled = true;
                }
            });
        });
    if resolved {
        app.resolve_close_guard(save);
    } else if cancelled {
        app.cancel_close_guard();
    }
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
                "Build assembles and writes the artifact to release/ \u{b7} Run\n\
                 loads through the monitor \u{b7} Run direct injects into RAM",
            )
            .small()
            .color(Theme::color(&theme.text_weak)),
        );
    });
}

/// The editor toolbar: build and run actions, then file actions, then
/// the build status line.
fn toolbar(ui: &mut egui::Ui, app: &mut App, theme: &Theme) {
    ui.horizontal_wrapped(|ui| {
        if ui
            .button(format!("{} Build", icon::STEPS))
            .on_hover_text(
                "Assemble the current buffers and write the artifact\n\
                 (a .ptp / .rmm / .bin, per the target) into the\n\
                 project's release/ folder",
            )
            .clicked()
        {
            app.build_and_release_project();
        }
        if ui
            .button(format!("{} Export\u{2026}", icon::UPLOAD_SIMPLE))
            .on_hover_text(
                "Assemble, then write the artifact wherever you pick\n\
                 (a .ptp tape, a .rmm ROM module or a raw .bin, per\n\
                 the target)",
            )
            .clicked()
        {
            app.export_project();
        }
        if ui
            .button(format!("{} Run", icon::PLAY))
            .on_hover_text(
                "Build, then load through the monitor: the file goes on the\n\
                 (flash) tape and the monitor's MGLD command is typed for\n\
                 you \u{2014} checksums and autorun run for real",
            )
            .clicked()
        {
            app.run_project();
        }
        if ui
            .button(format!("{} Run direct", icon::SKIP_FORWARD))
            .on_hover_text(
                "Build, then inject the image into RAM at its address and\n\
                 start the CPU at the entry point \u{2014} no monitor, no tape",
            )
            .clicked()
        {
            app.run_project_direct();
        }

        ui.separator();

        if ui
            .button(format!("{} New file\u{2026}", icon::FILE_PLUS))
            .on_hover_text("Add an empty source file to the project")
            .clicked()
        {
            if let Some(project) = app.project.as_mut() {
                project.new_open = true;
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
            .on_hover_text("Write the edited buffer to disk")
            .clicked()
        {
            app.save_project_file();
        }
        if ui
            .button(icon::MAGNIFYING_GLASS.to_string())
            .on_hover_text("Find and replace in the active buffer")
            .clicked()
        {
            if let Some(project) = app.project.as_mut() {
                project.find.open = !project.find.open;
                if project.find.open {
                    project.find.active = 0;
                    ui.ctx().memory_mut(|m| m.request_focus(find_query_id()));
                }
            }
        }
        if ui
            .button(icon::ARROW_BEND_UP_RIGHT.to_string())
            .on_hover_text("Go to a line in the active buffer")
            .clicked()
        {
            if let Some(project) = app.project.as_mut() {
                project.goto_open = true;
            }
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

/// The file list, with its per-file context menu. Laid out inside
/// the fixed-width left panel: the "sources" label on top, the file
/// rows stacked below it; the column scrolls on its own when the
/// files do not fit.
fn file_tree(ui: &mut egui::Ui, app: &mut App, theme: &Theme) {
    let Some(project) = app.project.as_mut() else {
        return;
    };
    egui::ScrollArea::vertical()
        .id_salt(Id::new("project-file-tree"))
        .auto_shrink([false, false])
        .show(ui, |ui| {
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
                let row = ui
                    .selectable_label(selected, egui::RichText::new(label).monospace().size(12.0))
                    .on_hover_text(tooltip);
                if row.clicked() {
                    project.active = Some(i);
                }
                row.context_menu(|ui| {
                    if ui
                        .button(format!("{} New file\u{2026}", icon::FILE_PLUS))
                        .clicked()
                    {
                        project.new_open = true;
                        project.new_name.clear();
                        ui.close();
                    }
                    if ui
                        .button(format!("{} Rename\u{2026}", icon::PENCIL_SIMPLE))
                        .clicked()
                    {
                        project.rename = Some(i);
                        project.rename_name = project.files[i].path.clone();
                        ui.close();
                    }
                    if ui
                        .button(format!("{} Delete\u{2026}", icon::TRASH))
                        .clicked()
                    {
                        project.remove = Some(i);
                        ui.close();
                    }
                });
            }
        });
}

/// The file popups: new file, rename, delete. Small dialogs driven
/// by the project state; the actions are staged and applied after
/// the project borrow ends (they may want to notify).
fn file_popups(ctx: &egui::Context, app: &mut App) {
    let danger = Theme::color(&app.active_theme_data().danger);

    // Staged dialog outcomes.
    let mut create: Option<String> = None;
    let mut rename: Option<(usize, String)> = None;
    let mut delete: Option<usize> = None;
    let mut close = [false; 3]; // [new, rename, delete]

    {
        let Some(project) = app.project.as_mut() else {
            return;
        };

        if project.new_open {
            let mut open = true;
            egui::Window::new("New file")
                .id(Id::new("project-new-file-dialog"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    let edit = ui.add(
                        TextEdit::singleline(&mut project.new_name)
                            .id(Id::new("project-new-file-input"))
                            .hint_text("name.asm")
                            .desired_width(140.0),
                    );
                    if ui.button("Create").clicked()
                        || (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    {
                        create = Some(project.new_name.trim().to_string());
                    }
                });
            close[0] = !open;
        }

        if let Some(index) = project.rename {
            let mut open = true;
            egui::Window::new("Rename file")
                .id(Id::new("project-rename-dialog"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    let path = project
                        .files
                        .get(index)
                        .map(|f| f.path.clone())
                        .unwrap_or_default();
                    ui.label(
                        egui::RichText::new(format!("{}/{}", project.dir.display(), path))
                            .weak()
                            .small(),
                    );
                    let edit = ui.add(
                        TextEdit::singleline(&mut project.rename_name)
                            .id(Id::new("project-rename-input"))
                            .desired_width(140.0),
                    );
                    if ui.button("Rename").clicked()
                        || (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    {
                        rename = Some((index, project.rename_name.trim().to_string()));
                    }
                });
            close[1] = !open;
        }

        if let Some(index) = project.remove {
            let mut open = true;
            let path = project
                .files
                .get(index)
                .map(|f| f.path.clone())
                .unwrap_or_default();
            egui::Window::new("Delete file")
                .id(Id::new("project-remove-dialog"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Delete {}/{} from disk?",
                        project.dir.display(),
                        path
                    ));
                    ui.label(
                        egui::RichText::new("The file will be gone; this is not undoable.")
                            .weak()
                            .small(),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui
                            .button(egui::RichText::new("Delete").color(danger))
                            .clicked()
                        {
                            delete = Some(index);
                        }
                        if ui.button("Cancel").clicked() {
                            close[2] = true;
                        }
                    });
                });
            close[2] |= !open;
        }
    }

    if let Some(name) = create {
        let outcome = app.project.as_mut().map(|p| p.new_file(&name));
        match outcome {
            Some(Ok(())) => {
                if let Some(p) = app.project.as_mut() {
                    p.new_open = false;
                }
            }
            Some(Err(e)) => app.notify(e),
            None => {}
        }
    } else if close[0] {
        if let Some(p) = app.project.as_mut() {
            p.new_open = false;
        }
    }

    if let Some((index, name)) = rename {
        let outcome = app.project.as_mut().map(|p| p.rename_file(index, &name));
        match outcome {
            Some(Ok(path)) => {
                if let Some(p) = app.project.as_mut() {
                    p.rename = None;
                }
                app.notify(format!("Renamed to {path}"));
            }
            Some(Err(e)) => app.notify(e),
            None => {}
        }
    } else if close[1] {
        if let Some(p) = app.project.as_mut() {
            p.rename = None;
        }
    }

    if let Some(index) = delete {
        let outcome = app.project.as_mut().map(|p| p.remove_file(index));
        match outcome {
            Some(Ok(path)) => {
                if let Some(p) = app.project.as_mut() {
                    p.remove = None;
                }
                app.notify(format!("Deleted {path}"));
            }
            Some(Err(e)) => app.notify(e),
            None => {}
        }
    } else if close[2] {
        if let Some(p) = app.project.as_mut() {
            p.remove = None;
        }
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

    // The editor scrolls inside its own area (the tab body does not
    // scroll): gutter and text scroll together, the toolbar, find
    // bar and file tree stay put.
    let response = egui::ScrollArea::both()
        .id_salt(("project-editor-scroll", &path))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // The gutter and the editor form one row: no spacing between
            // them, the same font, and the TextEdit's own top frame
            // padding (its default margin is 2 px) added above the line
            // numbers so the rows line up.
            let response = ui.horizontal(|ui| {
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

            // A pending jump: cursor onto the spot, view scrolled to
            // it (this ui sits inside the scroll area, so the scroll
            // it asks for is the editor's own).
            if let Some(jump) = jump.filter(|j| j.file == path) {
                let char_pos = char_offset(&text, jump.line, jump.col);
                set_cursor(ui.ctx(), id, char_pos);
                let row = ui.ctx().fonts_mut(|f| f.row_height(&font_id));
                let rect = response
                    .inner
                    .rect
                    .translate(egui::vec2(0.0, (jump.line as f32 - 0.5) * row));
                ui.scroll_to_rect(rect, Some(egui::Align::Center));
            }
            response.inner
        })
        .inner;

    let changed = response.changed();
    let file = &mut project.files[idx];
    file.text = text;
    if changed {
        file.dirty = true;
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
            .on_hover_text("Close the find bar")
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
    // The lexer drops blank lines, so lines are looked up by their
    // number, not by their position in the lexed list — indexing
    // positionally would color every line after a blank one with the
    // wrong line's tokens.
    let mut diags = Vec::new();
    let lines = lex_lines(0, text, &mut diags);
    let lexed_of: std::collections::HashMap<usize, &LexLine> =
        lines.iter().map(|l| (l.line, l)).collect();
    let raw: Vec<&str> = text.split_inclusive('\n').collect();

    let mut base = 0usize; // byte offset of the current line start
    for (i, line) in raw.iter().enumerate() {
        let lineno = i + 1;
        let nchars = line.chars().count();
        let lexed = lexed_of.get(&lineno).copied();

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

#[cfg(test)]
mod tests {
    use super::*;

    /// The highlighting is laid out from the assembler's own tokens,
    /// which the lexer emits without blank lines: the highlighter must
    /// map them back by line number, or every line after a blank one
    /// is colored with a neighbouring line's tokens (mnemonics on
    /// labels, literals on registers, at a constant offset).
    #[test]
    fn highlighting_survives_blank_lines() {
        let theme = Theme::default();
        let palette = Palette::from(&theme);
        let text = "START: MOV A,H\n\n INR B\n HLT ; done\n";
        let job = highlight_job(
            text,
            &egui::FontId::monospace(12.0),
            &palette,
            &HashSet::new(),
            &FindState::default(),
        );
        let runs = |want: egui::Color32| -> Vec<String> {
            job.sections
                .iter()
                .filter(|s| s.format.color == want)
                .map(|s| job.text[s.byte_range.start.0..s.byte_range.end.0].to_string())
                .collect()
        };
        assert_eq!(
            runs(Theme::color(&theme.accent)),
            ["MOV", "INR", "HLT"],
            "mnemonics (and only they) glow"
        );
        assert_eq!(
            runs(Theme::color(&theme.warn)),
            Vec::<String>::new(),
            "no literals in this source"
        );
        assert_eq!(
            runs(Theme::color(&theme.text_weak)),
            ["; done\n"],
            "the comment dims"
        );
        assert_eq!(job.text, text, "the job covers the text exactly");
    }
}
