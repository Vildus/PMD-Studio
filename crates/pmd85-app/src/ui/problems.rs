//! The Problems tab: the build's diagnostics, clickable through to
//! the offending spot in the source.

use crate::app::App;
use crate::ui::project::Jump;
use crate::ui::theme::Theme;
use egui_phosphor::regular as icon;

/// The Problems tab body.
pub fn ui(ui: &mut egui::Ui, app: &mut App) {
    let theme = app.active_theme_data();
    let Some(project) = app.project.as_ref() else {
        ui.label(
            egui::RichText::new("No project open \u{2014} open one in the Project tab.").weak(),
        );
        return;
    };
    let Some(build) = project.build.as_ref() else {
        ui.label(
            egui::RichText::new("Not built yet \u{2014} Build in the Project tab (Ctrl+B).").weak(),
        );
        return;
    };
    if build.asm.diagnostics.is_empty() {
        ui.label(egui::RichText::new("\u{2713} Build OK").color(Theme::color(&theme.accent)));
        if let Some(entry) = build.asm.entry {
            ui.label(
                egui::RichText::new(format!("entry point {:04X}H", entry))
                    .small()
                    .color(Theme::color(&theme.text_weak)),
            );
        }
        let size: usize = build.asm.segments.iter().map(|s| s.data.len()).sum();
        ui.label(
            egui::RichText::new(format!(
                "{} segment(s), {} byte{} of code",
                build.asm.segments.len(),
                size,
                if size == 1 { "" } else { "s" }
            ))
            .small()
            .color(Theme::color(&theme.text_weak)),
        );
        return;
    }

    ui.label(format!(
        "{} problem{}",
        build.asm.diagnostics.len(),
        if build.asm.diagnostics.len() == 1 {
            ""
        } else {
            "s"
        }
    ));
    ui.add_space(4.0);

    // Collect first, then draw: a click mutates the project (opening
    // the file) while the build data stays borrowed otherwise.
    let rows: Vec<(String, usize, usize, String)> = build
        .asm
        .diagnostics
        .iter()
        .map(|d| {
            (
                build
                    .asm
                    .files
                    .get(d.file)
                    .cloned()
                    .unwrap_or_else(|| "?".to_string()),
                d.line,
                d.col,
                d.message.clone(),
            )
        })
        .collect();
    for (file, line, col, message) in rows {
        let text = egui::RichText::new(format!(
            "{} {}:{}:{}: {}",
            icon::WARNING_CIRCLE,
            file,
            line,
            col,
            message
        ))
        .monospace()
        .size(12.0)
        .color(Theme::color(&theme.danger));
        if ui
            .selectable_label(false, text)
            .on_hover_text("Open the source at this problem")
            .clicked()
        {
            // The jump lands in the editor on its next draw; make
            // sure the file is open and the Project tab is visible.
            if let Some(project) = app.project.as_mut() {
                if let Err(e) = project.open_file(&file) {
                    app.notify(e);
                }
            }
            app.ui.project_jump = Some(Jump { file, line, col });
            crate::ui::dock::show_tab(&mut app.ui.dock, crate::ui::dock::Tab::Project);
        }
    }
}
