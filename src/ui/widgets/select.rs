//! <select>-Nachbau: kompaktes Feld (h-6, bg-surface-2, border-line, px-1,
//! text-xs text-text-2) mit Dropdown-Popup im Overlay-Pass.

use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::widgets::drop_shadow;
use crate::ui::{FontKind, Ui, WidgetId};
use raylib::consts::{KeyboardKey, MouseCursor};

pub struct SelectPopup {
    pub owner: WidgetId,
    pub anchor: Rect,
    pub options: Vec<String>,
    pub selected: usize,
    pub result: Option<usize>,
    /// Frame, in dem das Popup geöffnet wurde (Klick nicht sofort als „außerhalb“ werten).
    pub just_opened: bool,
    /// Scroll-Offset für lange Listen (z. B. Schriftfamilien).
    pub scroll: f32,
}

/// Pro Frame offenes Select-Popup (höchstens eines).
#[derive(Default)]
pub struct SelectHost {
    pub popup: Option<SelectPopup>,
}

/// Zeichnet das Select-Feld; liefert `Some(neuer Index)` im Frame nach der
/// Auswahl. Das Popup lebt zentral in `ui.persist.select` und wird im
/// Overlay-Pass über [`render_select_popup`] gezeichnet.
pub fn select(
    ui: &mut Ui,
    id_src: impl std::hash::Hash,
    rect: Rect,
    options: &[&str],
    selected: usize,
) -> Option<usize> {
    let mut host = std::mem::take(&mut ui.persist.select);
    let result = select_with_host(ui, &mut host, id_src, rect, options, selected);
    ui.persist.select = host;
    result
}

fn select_with_host(
    ui: &mut Ui,
    host: &mut SelectHost,
    id_src: impl std::hash::Hash,
    rect: Rect,
    options: &[&str],
    selected: usize,
) -> Option<usize> {
    let id = ui.id(id_src);

    // Ergebnis eines im Overlay-Pass abgeschlossenen Popups abholen
    if let Some(popup) = &host.popup {
        if popup.owner == id {
            if let Some(result) = popup.result {
                host.popup = None;
                if result != selected {
                    return Some(result);
                }
                return None;
            }
        }
    }

    let it = ui.interact(id, rect);
    ui.fill_rounded(rect, theme::RADIUS_SM, theme::SURFACE_2);
    let border = if matches!(&host.popup, Some(p) if p.owner == id) {
        theme::ACCENT
    } else {
        theme::LINE
    };
    ui.stroke_rounded(rect, theme::RADIUS_SM, 1.0, border);

    let mut inner = rect.inset_xy(4.0, 0.0);
    let arrow = inner.cut_right(14.0);
    let label = options.get(selected).copied().unwrap_or("");
    let text = ui.font(FontKind::Sans12).ellipsize(label, inner.w);
    ui.text_left(&text, inner, theme::TEXT_2, FontKind::Sans12);
    ui.icon("chevron-down", arrow, 12.0, theme::TEXT_3);

    if it.hovered {
        ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
    }
    if it.clicked {
        host.popup = Some(SelectPopup {
            owner: id,
            anchor: rect,
            options: options.iter().map(|s| s.to_string()).collect(),
            selected,
            result: None,
            just_opened: true,
            scroll: -1.0, // < 0: beim Öffnen zur Auswahl scrollen
        });
    }
    None
}

/// Im Overlay-Pass aufrufen: zeichnet das offene Popup und füllt `result`.
pub fn render_select_popup(ui: &mut Ui) {
    let mut host = std::mem::take(&mut ui.persist.select);
    render_popup_with_host(ui, &mut host);
    ui.persist.select = host;
}

fn render_popup_with_host(ui: &mut Ui, host: &mut SelectHost) {
    let Some(popup) = &mut host.popup else { return };
    if popup.result.is_some() {
        return;
    }

    let row_h = 28.0;
    let w = popup.anchor.w.max(120.0);
    let content_h = popup.options.len() as f32 * row_h + 8.0;
    // Lange Listen (z. B. Schriftfamilien): Höhe begrenzen + Mausrad-Scroll.
    let max_h = (ui.screen.h - 16.0).max(row_h + 8.0);
    let h = content_h.min(max_h);
    let mut x = popup.anchor.x;
    let mut y = popup.anchor.bottom() + 2.0;
    if y + h > ui.screen.bottom() - 4.0 {
        y = (popup.anchor.y - 2.0 - h).max(8.0);
    }
    x = x.min(ui.screen.right() - w - 4.0).max(4.0);
    let rect = Rect::new(x, y, w, h);

    let max_scroll = (content_h - h).max(0.0);
    if popup.scroll < 0.0 {
        // Beim Öffnen die aktuelle Auswahl ins Sichtfeld rücken.
        popup.scroll = (popup.selected as f32 * row_h - h / 2.0 + row_h).clamp(0.0, max_scroll);
    }
    if rect.contains(ui.input.mouse) {
        popup.scroll = (popup.scroll - ui.input.wheel.y * row_h * 2.0).clamp(0.0, max_scroll);
    }

    drop_shadow(ui, rect, theme::RADIUS_MD);
    ui.fill_rounded(rect, theme::RADIUS_MD, theme::SURFACE_2);
    ui.stroke_rounded(rect, theme::RADIUS_MD, 1.0, theme::LINE);

    ui.push_clip(rect.inset_xy(0.0, 2.0));
    let mut row = Rect::new(rect.x, rect.y + 4.0 - popup.scroll, rect.w, row_h);
    let mut clicked: Option<usize> = None;
    for (i, option) in popup.options.iter().enumerate() {
        if row.bottom() < rect.y || row.y > rect.bottom() {
            row = row.offset(0.0, row_h);
            continue;
        }
        let hovered = ui.mouse_in(row);
        if hovered {
            ui.fill(row, theme::SURFACE_3);
            ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
            if ui.input.left_pressed {
                clicked = Some(i);
            }
        }
        let mut inner = row.inset_xy(10.0, 0.0);
        let check = inner.cut_left(16.0);
        if i == popup.selected {
            ui.icon("check", check, 14.0, theme::TEXT_1);
        }
        inner.cut_left(4.0);
        ui.text_left(option, inner, theme::TEXT_1, FontKind::Sans12);
        row = row.offset(0.0, row_h);
    }
    ui.pop_clip();
    // Scroll-Indikator (schmaler Thumb am rechten Rand).
    if max_scroll > 0.0 {
        let track_h = rect.h - 8.0;
        let thumb_h = (track_h * (h / content_h)).max(24.0);
        let thumb_y = rect.y + 4.0 + (track_h - thumb_h) * (popup.scroll / max_scroll);
        ui.fill_rounded(
            Rect::new(rect.right() - 5.0, thumb_y, 3.0, thumb_h),
            theme::RADIUS_XS,
            theme::SURFACE_4,
        );
    }

    if let Some(i) = clicked {
        popup.result = Some(i);
        return;
    }

    // Schließen: Klick außerhalb, Escape
    let outside_click = (ui.input.left_pressed || ui.input.right_pressed)
        && !rect.contains(ui.input.mouse)
        && !popup.just_opened;
    let escape = ui
        .input
        .keys
        .iter()
        .any(|k| k.key == KeyboardKey::KEY_ESCAPE);
    if outside_click || escape {
        host.popup = None;
        return;
    }
    popup.just_opened = false;
}
