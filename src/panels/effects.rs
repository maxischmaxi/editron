//! Effekte-Panel: durchsuchbarer Katalog aller Effekte (`core/effects.rs`)
//! und Übergänge (`core/transitions.rs`), nach Kategorien gruppiert.
//! Anwenden per Drag&Drop auf einen Timeline-Clip bzw. eine Schnittkante
//! (oder ins Panel Effekteinstellungen), per Doppelklick auf die
//! ausgewählten Clips oder über das Kontextmenü. Die Fußzeile zeigt die
//! Beschreibung des Eintrags unter der Maus.

use crate::core::effects::{EffectCategory, EffectKind};
use crate::core::transitions::TransitionKind;
use crate::overlays::context_menu::{CustomAction, MenuEntry, MenuItem};
use crate::panels::Panel;
use crate::services::Services;
use crate::state::AppState;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::scroll::ScrollState;
use crate::ui::widgets::text_input::TextInputState;
use crate::ui::{DragPayload, FontKind, Ui};
use raylib::consts::MouseCursor;
use std::collections::HashSet;

#[derive(Default)]
pub struct EffectsPanel {
    search: TextInputState,
    collapsed: HashSet<&'static str>,
    scroll: ScrollState,
}

/// Kategorien-ID für den Collapse-State.
fn category_id(cat: EffectCategory) -> &'static str {
    match cat {
        EffectCategory::BlurSharpen => "blur-sharpen",
        EffectCategory::Color => "color",
        EffectCategory::Keying => "keying",
        EffectCategory::Stylize => "stylize",
        EffectCategory::Transform => "transform",
        EffectCategory::Audio => "audio",
    }
}

/// Ein Eintrag des Katalogs: Effekt oder Übergang.
#[derive(Clone, Copy, PartialEq)]
enum CatalogEntry {
    Effect(EffectKind),
    Transition(TransitionKind),
}

impl CatalogEntry {
    fn label(&self) -> &'static str {
        match self {
            CatalogEntry::Effect(k) => k.label(),
            CatalogEntry::Transition(k) => k.label(),
        }
    }

    fn key(&self) -> &'static str {
        match self {
            CatalogEntry::Effect(k) => k.key(),
            CatalogEntry::Transition(k) => k.key(),
        }
    }

    fn icon(&self) -> &'static str {
        match self {
            CatalogEntry::Effect(k) => k.icon(),
            CatalogEntry::Transition(k) => k.icon(),
        }
    }

    fn description(&self) -> &'static str {
        match self {
            CatalogEntry::Effect(k) => k.description(),
            CatalogEntry::Transition(k) => k.description(),
        }
    }
}

impl Panel for EffectsPanel {
    fn update(&mut self, ui: &mut Ui, app: &mut AppState, _services: &Services, rect: Rect) {
        ui.fill(rect, theme::SURFACE_1);
        if ui.mouse_in(rect) && (ui.input.left_pressed || ui.input.right_pressed) {
            app.app.focused_panel = "effects".into();
        }
        let mut area = rect;

        // Suchleiste (h-9, border-b): Lupe + transparentes Eingabefeld
        let bar = area.cut_top(36.0);
        ui.hline(bar.x, bar.bottom() - 1.0, bar.w, theme::LINE);
        let mut bar_inner = bar.inset_xy(8.0, 0.0);
        let icon_cell = bar_inner.cut_left(14.0);
        ui.icon("search", icon_cell, 14.0, theme::TEXT_3);
        bar_inner.cut_left(8.0);
        let field = Rect::new(bar_inner.x, bar.y + 6.0, bar_inner.w, 24.0);
        self.search
            .show(ui, "effects.search", field, "Effekte durchsuchen …");

        // Fußzeile: Beschreibung des Eintrags unter der Maus, sonst Hinweis.
        let footer = area.cut_bottom(33.0);

        // Inhalt filtern
        let query = self.search.text.trim().to_lowercase();
        let searching = !query.is_empty();
        let matches = |label: &str, key: &str, cat: &str| {
            !searching
                || label.to_lowercase().contains(&query)
                || key.to_lowercase().contains(&query)
                || cat.to_lowercase().contains(&query)
        };
        // Effekt-Kategorien + Übergangs-Kategorien (Video/Audio).
        let mut groups: Vec<(&'static str, &'static str, Vec<CatalogEntry>)> = EffectCategory::ALL
            .iter()
            .map(|cat| {
                (
                    category_id(*cat),
                    cat.label(),
                    EffectKind::ALL
                        .iter()
                        .filter(|k| {
                            k.category() == *cat && matches(k.label(), k.key(), cat.label())
                        })
                        .map(|k| CatalogEntry::Effect(*k))
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        for (id, label, audio) in [
            ("transitions-video", "Videoübergänge", false),
            ("transitions-audio", "Audioübergänge", true),
        ] {
            groups.push((
                id,
                label,
                TransitionKind::ALL
                    .iter()
                    .filter(|k| k.is_audio() == audio && matches(k.label(), k.key(), label))
                    .map(|k| CatalogEntry::Transition(*k))
                    .collect(),
            ));
        }
        let groups: Vec<(&'static str, &'static str, Vec<CatalogEntry>)> = groups
            .into_iter()
            .filter(|(_, _, entries)| !entries.is_empty())
            .collect();

        let mut content_h = 8.0;
        for (id, _, entries) in &groups {
            content_h += 28.0;
            if searching || !self.collapsed.contains(id) {
                content_h += entries.len() as f32 * 28.0;
            }
        }

        let mut hovered: Option<CatalogEntry> = None;
        let has_selection = !app.timeline.selected_clip_ids.is_empty();

        if groups.is_empty() {
            ui.text_centered(
                "Keine Effekte gefunden.",
                area,
                theme::TEXT_3,
                FontKind::Sans12,
            );
        } else {
            let view = self.scroll.begin(ui, area, 0.0, content_h);
            let mut y = view.origin_y + 4.0;
            let w = view.viewport.w;
            for (cat_id, cat_label, entries) in &groups {
                let header = Rect::new(view.viewport.x, y, w, 28.0);
                let is_collapsed = !searching && self.collapsed.contains(cat_id);
                let hid = ui.id(("effects.cat", cat_id));
                let it = ui.interact(hid, header);
                if it.hovered {
                    ui.fill(header, theme::SURFACE_2);
                    ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                }
                let mut hi = header.inset_xy(8.0, 0.0);
                let chev = hi.cut_left(14.0);
                ui.icon(
                    if is_collapsed { "chevron-right" } else { "chevron-down" },
                    chev,
                    14.0,
                    theme::TEXT_3,
                );
                hi.cut_left(6.0);
                let count = entries.len().to_string();
                let count_w = ui.font(FontKind::Mono12).width(&count);
                let count_cell = hi.cut_right(count_w);
                ui.text_left(&count, count_cell, theme::TEXT_3, FontKind::Mono12);
                ui.text_left(
                    cat_label,
                    hi,
                    if it.hovered { theme::TEXT_1 } else { theme::TEXT_2 },
                    FontKind::Sans12Medium,
                );
                if it.clicked && !searching {
                    if is_collapsed {
                        self.collapsed.remove(cat_id);
                    } else {
                        self.collapsed.insert(cat_id);
                    }
                }
                y += 28.0;
                if is_collapsed {
                    continue;
                }
                for entry in entries {
                    let row = Rect::new(view.viewport.x, y, w, 28.0);
                    let rid = ui.id(("effects.row", entry.key()));
                    let rit = ui.interact(rid, row);
                    if rit.hovered {
                        ui.fill(row, theme::SURFACE_2);
                        ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
                        hovered = Some(*entry);
                    }
                    let mut ri = row.inset_xy(8.0, 0.0);
                    ri.cut_left(20.0); // Einrückung unter dem Chevron
                    let ic = ri.cut_left(14.0);
                    ui.icon(entry.icon(), ic, 14.0, theme::TEXT_2);
                    ri.cut_left(8.0);
                    let display = ui.font(FontKind::Sans12).ellipsize(entry.label(), ri.w);
                    ui.text_left(&display, ri, theme::TEXT_1, FontKind::Sans12);

                    // Drag-Quelle: Effekt auf Clip ziehen, Übergang auf eine
                    // Schnittkante.
                    if rit.hovered && ui.input.left_pressed && ui.nothing_active() {
                        match entry {
                            CatalogEntry::Effect(kind) => {
                                ui.start_drag(DragPayload::Effect(*kind))
                            }
                            CatalogEntry::Transition(kind) => {
                                ui.start_drag(DragPayload::Transition(*kind))
                            }
                        }
                    }
                    // Doppelklick: auf die ausgewählten Clips anwenden.
                    if rit.double_clicked && has_selection {
                        match entry {
                            CatalogEntry::Effect(kind) => ui.run_command_with(
                                "clip.addEffect",
                                serde_json::json!({ "kind": kind.key() }),
                            ),
                            CatalogEntry::Transition(kind) => ui.run_command_with(
                                "clip.applyTransition",
                                serde_json::json!({ "kind": kind.key() }),
                            ),
                        }
                    }
                    // Kontextmenü
                    if rit.right_clicked {
                        let item = match entry {
                            CatalogEntry::Effect(kind) => {
                                let ids = app.timeline.selected_clip_ids.clone();
                                MenuItem::custom(
                                    "Auf ausgewählte Clips anwenden",
                                    CustomAction::EffectsApplyToClips {
                                        kind: *kind,
                                        clip_ids: ids,
                                    },
                                )
                            }
                            CatalogEntry::Transition(kind) => {
                                MenuItem::command("clip.applyTransition")
                                    .with_label("Auf Kanten der Auswahl anwenden")
                                    .with_args(serde_json::json!({ "kind": kind.key() }))
                            }
                        };
                        app.context_menu.show(
                            ui.input.mouse.x,
                            ui.input.mouse.y,
                            vec![MenuEntry::Item(
                                item.with_icon("plus").with_disabled(!has_selection),
                            )],
                        );
                    }
                    y += 28.0;
                }
            }
            self.scroll.end(ui, area, 0.0, content_h);
        }

        // Fußzeile zeichnen (nach dem Inhalt — kennt den Hover-Zustand).
        ui.hline(footer.x, footer.y, footer.w, theme::LINE);
        let footer_text = match hovered {
            Some(entry) => entry.description().to_string(),
            None => {
                "Effekt auf einen Clip ziehen, Übergang auf eine Schnittkante — oder per Doppelklick anwenden."
                    .to_string()
            }
        };
        let display = ui
            .font(FontKind::Sans12)
            .ellipsize(&footer_text, footer.w - 24.0);
        ui.text_left(
            &display,
            footer.inset_xy(12.0, 0.0),
            theme::TEXT_3,
            FontKind::Sans12,
        );
    }
}
