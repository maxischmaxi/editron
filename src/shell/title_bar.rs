//! Kopfleiste: Logo + Datei-Menü links, Workspace-Tabs in der Mitte,
//! globale Aktionen + FFmpeg-Status rechts.

use crate::overlays::context_menu::{MenuEntry, MenuItem};
use crate::state::AppState;
use crate::stores::{workspace_name, WORKSPACE_IDS};
use crate::theme;
use crate::ui::geom::{v2, Rect};
use crate::ui::widgets::IconButton;
use crate::ui::{FontKind, Ui};
use raylib::consts::MouseCursor;
use raylib::prelude::RaylibDraw;

/// Einträge des Datei-Menüs (Projektverwaltung, Import/Export, Relink).
fn file_menu_items(app: &AppState) -> Vec<MenuEntry> {
    let recent: Vec<MenuEntry> = if app.project.recent.is_empty() {
        vec![MenuEntry::Item(
            MenuItem::command("project.openRecent")
                .with_label("Keine zuletzt verwendeten Projekte")
                .with_disabled(true),
        )]
    } else {
        app.project
            .recent
            .iter()
            .map(|path| {
                let name = std::path::Path::new(path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.clone());
                MenuEntry::Item(
                    MenuItem::command("project.openRecent")
                        .with_label(&name)
                        .with_args(serde_json::json!({ "path": path })),
                )
            })
            .collect()
    };
    vec![
        MenuEntry::Item(MenuItem::command("project.new").with_icon("clapperboard")),
        MenuEntry::Item(MenuItem::command("project.open").with_icon("folder-open")),
        MenuEntry::Submenu {
            label: "Zuletzt geöffnet".into(),
            icon: Some("history"),
            items: recent,
        },
        MenuEntry::Separator,
        MenuEntry::Item(MenuItem::command("project.save")),
        MenuEntry::Item(MenuItem::command("project.saveAs")),
        MenuEntry::Separator,
        MenuEntry::Item(MenuItem::command("sequence.settings").with_icon("sliders-horizontal")),
        MenuEntry::Separator,
        MenuEntry::Item(MenuItem::command("media.import").with_icon("import")),
        MenuEntry::Item(MenuItem::command("app.export").with_icon("file-output")),
        MenuEntry::Separator,
        MenuEntry::Item(MenuItem::command("subtitle.importSrt").with_icon("captions")),
        MenuEntry::Item(MenuItem::command("subtitle.exportSrt").with_icon("captions")),
        MenuEntry::Separator,
        MenuEntry::Item(MenuItem::command("project.relink").with_icon("link-2")),
        MenuEntry::Item(MenuItem::command("project.restoreAutosave")),
    ]
}

pub fn render(ui: &mut Ui, app: &mut AppState, rect: Rect) {
    ui.fill(rect, theme::SURFACE_0);
    ui.hline(rect.x, rect.bottom() - 1.0, rect.w, theme::LINE);

    // ---- Links: Logo + Projektname + Datei-Menü (pl-3, gap-2) ----
    let mut left = rect;
    left.cut_left(12.0);
    let logo = Rect::new(left.x, rect.y + (rect.h - 20.0) / 2.0, 20.0, 20.0);
    ui.fill_rounded(logo, theme::RADIUS_SM, theme::ACCENT);
    ui.text_centered("E", logo, theme::WHITE, FontKind::Sans12Bold);
    left.cut_left(20.0 + 8.0);
    let title = format!(
        "{}{}",
        app.project.display_name(),
        if app.project.dirty { " •" } else { "" }
    );
    let title_w = ui.font(FontKind::Sans14Semibold).width(&title);
    let title_rect = left.cut_left(title_w);
    ui.text_left(&title, title_rect, theme::TEXT_1, FontKind::Sans14Semibold);
    left.cut_left(12.0);

    // Datei-Menü als Dropdown über das Kontextmenü-Overlay
    let menu_label = "Datei";
    let menu_w = ui.font(FontKind::Sans12).width(menu_label) + 20.0;
    let menu_btn = Rect::new(left.x, rect.y + (rect.h - 24.0) / 2.0, menu_w, 24.0);
    let menu_id = ui.id("titlebar.fileMenu");
    let it = ui.interact(menu_id, menu_btn);
    if it.hovered {
        ui.fill_rounded(menu_btn, theme::RADIUS_SM, theme::SURFACE_2);
        ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
    }
    ui.text_centered(
        menu_label,
        menu_btn,
        if it.hovered { theme::TEXT_1 } else { theme::TEXT_2 },
        FontKind::Sans12,
    );
    if it.clicked {
        let items = file_menu_items(app);
        app.context_menu.show(menu_btn.x, rect.bottom(), items);
    }

    // ---- Mitte: Workspace-Tabs (px-3, text-xs) ----
    let font = FontKind::Sans12;
    let total_w: f32 = WORKSPACE_IDS
        .iter()
        .map(|id| ui.font(font).width(workspace_name(id)) + 24.0)
        .sum();
    let mut x = rect.x + (rect.w - total_w) / 2.0;
    for id in WORKSPACE_IDS {
        let name = workspace_name(id);
        let w = ui.font(font).width(name) + 24.0;
        let tab = Rect::new(x, rect.y, w, rect.h - 1.0);
        let active = app.app.active_workspace == id;
        let wid = ui.id(("titlebar.ws", id));
        let it = ui.interact(wid, tab);
        let color = if active {
            theme::TEXT_1
        } else if it.hovered {
            theme::TEXT_2
        } else {
            theme::TEXT_3
        };
        let kind = if active {
            FontKind::Sans12Medium
        } else {
            FontKind::Sans12
        };
        ui.text_centered(name, tab, color, kind);
        if active {
            // after:inset-x-2 after:bottom-0 after:h-0.5 after:rounded-full after:bg-accent
            ui.fill_rounded(
                Rect::new(tab.x + 8.0, tab.bottom() - 2.0, tab.w - 16.0, 2.0),
                1.0,
                theme::ACCENT,
            );
        }
        if it.hovered {
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
        }
        if it.clicked {
            ui.run_command(format!("workspace.switch.{id}"));
        }
        x += w;
    }

    // ---- Rechts: Action-Buttons + Divider + FFmpeg-Status (pr-3, gap-1) ----
    let actions: [(&str, &str, &str); 4] = [
        ("media.import", "Medien importieren", "import"),
        ("app.export", "Exportieren", "file-output"),
        ("app.shortcutEditor", "Tastaturbefehle", "keyboard"),
        ("app.commandPalette", "Befehlspalette", "square-terminal"),
    ];

    // FFmpeg-Status zuerst vermessen (rechtsbündig layouten)
    let ffmpeg_label_w = ui.font(FontKind::Sans12).width("FFmpeg");
    let status_w = 8.0 + 6.0 + ffmpeg_label_w + 16.0; // dot + gap + Label + px-2
    let mut right = rect;
    right.cut_right(12.0);

    let status_rect = right.cut_right(status_w);
    let (dot_color, tooltip) = match &app.app.ffmpeg {
        None => (theme::SURFACE_4, "FFmpeg wird gesucht …".to_string()),
        Some(info) if info.available => (
            theme::SUCCESS,
            format!(
                "FFmpeg {}{}",
                info.version.as_deref().unwrap_or("(Version unbekannt)"),
                info.ffmpeg_path
                    .as_deref()
                    .map(|p| format!(" — {p}"))
                    .unwrap_or_default()
            ),
        ),
        Some(_) => (
            theme::DANGER,
            "FFmpeg wurde nicht gefunden — Medienfunktionen sind deaktiviert.".to_string(),
        ),
    };
    let dot = v2(status_rect.x + 12.0, status_rect.center().y);
    ui.d.draw_circle_v(dot, 4.0, dot_color);
    ui.text_left(
        "FFmpeg",
        Rect::new(status_rect.x + 22.0, status_rect.y, ffmpeg_label_w, status_rect.h),
        theme::TEXT_3,
        FontKind::Sans12,
    );
    let status_id = ui.id("titlebar.ffmpeg");
    if ui.mouse_in(status_rect) {
        ui.set_hot(status_id);
    }
    ui.tooltip(status_id, status_rect, &tooltip);

    // Divider (mx-1 h-4 w-px)
    right.cut_right(4.0);
    let divider = right.cut_right(1.0);
    ui.fill(
        Rect::new(divider.x, rect.y + (rect.h - 16.0) / 2.0, 1.0, 16.0),
        theme::LINE,
    );
    right.cut_right(4.0);

    // Buttons (size-7) von rechts nach links
    for (command, label, icon) in actions.iter().rev() {
        let slot = right.cut_right(28.0);
        let btn = Rect::new(slot.x, rect.y + (rect.h - 28.0) / 2.0, 28.0, 28.0);
        let shortcut = app.keymap.shortcut_for(command);
        let tip = match shortcut {
            Some(sc) => format!("{label} ({sc})"),
            None => label.to_string(),
        };
        if IconButton::new(icon)
            .tooltip(&tip)
            .show(ui, ("titlebar.action", command), btn)
            .clicked
        {
            ui.run_command(*command);
        }
        right.cut_right(4.0); // gap-1
    }
}
