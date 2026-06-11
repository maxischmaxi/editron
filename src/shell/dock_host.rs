//! Docking-Fläche: rendert den Split-Baum mit Tab-Gruppen, Sash-Resize,
//! Tab-Drag&Drop mit Drop-Zonen und Panel-Inhalten
//! (Fuge 2 px, Radius 4 px, Tab-Höhe 32 px).

use crate::core::dock::{DockNode, DropEdge, GroupId, SplitDir};
use crate::panels::{panel_icon, panel_title, PanelHost};
use crate::services::Services;
use crate::state::AppState;
use crate::theme;
use crate::ui::geom::Rect;
use crate::ui::{DragPayload, FontKind, Ui};
use raylib::consts::MouseCursor;

const TAB_H: f32 = 32.0;
const GAP: f32 = 2.0; // theme.gap
const SASH_HIT: f32 = 8.0; // Hit-Zone um die Fuge
const MIN_PANE: f32 = 100.0;

struct GroupLayout {
    gid: GroupId,
    rect: Rect,
    tabs: Vec<String>,
    active: usize,
}

struct SashLayout {
    /// Pfad von der Wurzel zu diesem Split (Kind-Indizes).
    path: Vec<usize>,
    /// Fuge zwischen Kind `index` und `index + 1`.
    index: usize,
    rect: Rect,
    dir: SplitDir,
    /// Gesamtausdehnung des Splits entlang der Achse (für Weight-Deltas).
    total: f32,
}

pub struct DockHost {
    sash_drag: Option<SashDrag>,
    /// Drop-Vorschau des laufenden Tab-Drags.
    drop_preview: Option<(GroupId, DropEdge, Rect, Option<usize>)>,
}

struct SashDrag {
    path: Vec<usize>,
    index: usize,
}

impl Default for DockHost {
    fn default() -> Self {
        DockHost {
            sash_drag: None,
            drop_preview: None,
        }
    }
}

fn collect_layout(
    node: &DockNode,
    rect: Rect,
    path: &mut Vec<usize>,
    groups: &mut Vec<GroupLayout>,
    sashes: &mut Vec<SashLayout>,
) {
    match node {
        DockNode::Group(g) => groups.push(GroupLayout {
            gid: g.id,
            rect,
            tabs: g.tabs.clone(),
            active: g.active.min(g.tabs.len().saturating_sub(1)),
        }),
        DockNode::Split { dir, children } => {
            let n = children.len();
            if n == 0 {
                return;
            }
            let total = match dir {
                SplitDir::Row => rect.w,
                SplitDir::Col => rect.h,
            } - GAP * (n as f32 - 1.0);
            let weight_sum: f32 = children.iter().map(|c| c.weight).sum();
            let mut cursor = match dir {
                SplitDir::Row => rect.x,
                SplitDir::Col => rect.y,
            };
            for (i, child) in children.iter().enumerate() {
                let size = total * (child.weight / weight_sum.max(0.0001));
                let child_rect = match dir {
                    SplitDir::Row => Rect::new(cursor, rect.y, size, rect.h),
                    SplitDir::Col => Rect::new(rect.x, cursor, rect.w, size),
                };
                path.push(i);
                collect_layout(&child.node, child_rect, path, groups, sashes);
                path.pop();
                cursor += size;
                if i + 1 < n {
                    let sash_rect = match dir {
                        SplitDir::Row => {
                            Rect::new(cursor - (SASH_HIT - GAP) / 2.0, rect.y, SASH_HIT, rect.h)
                        }
                        SplitDir::Col => {
                            Rect::new(rect.x, cursor - (SASH_HIT - GAP) / 2.0, rect.w, SASH_HIT)
                        }
                    };
                    sashes.push(SashLayout {
                        path: path.clone(),
                        index: i,
                        rect: sash_rect,
                        dir: *dir,
                        total,
                    });
                    cursor += GAP;
                }
            }
        }
    }
}

/// Split-Knoten über seinen Pfad finden.
fn split_by_path<'a>(root: &'a mut DockNode, path: &[usize]) -> Option<&'a mut DockNode> {
    let mut node = root;
    for &i in path {
        match node {
            DockNode::Split { children, .. } => node = &mut children.get_mut(i)?.node,
            DockNode::Group(_) => return None,
        }
    }
    Some(node)
}

impl DockHost {
    pub fn render(
        &mut self,
        ui: &mut Ui,
        app: &mut AppState,
        services: &Services,
        panel_host: &mut PanelHost,
        rect: Rect,
    ) {
        ui.fill(rect, theme::SURFACE_0);

        let Some(root) = &app.dock.root else {
            // Watermark: leeres Dock
            let center = rect.center_box(220.0, 52.0);
            let mut area = center;
            let icon_rect = area.cut_top(24.0);
            ui.icon(
                "panels-top-left",
                icon_rect,
                24.0,
                theme::with_alpha(theme::TEXT_3, 153),
            );
            area.cut_top(8.0);
            ui.text_centered(
                "Panel über Fenster-Befehle öffnen",
                area,
                theme::TEXT_3,
                FontKind::Sans12,
            );
            return;
        };

        let mut groups = Vec::new();
        let mut sashes = Vec::new();
        collect_layout(root, rect, &mut Vec::new(), &mut groups, &mut sashes);

        // ---------- Sash-Interaktion (vor den Panels, mutiert nur weights) ----
        self.handle_sashes(ui, app, &sashes);

        // ---------- Gruppen: Tab-Leisten + Inhalt ----------
        let mut tab_actions: Vec<TabAction> = Vec::new();
        for group in &groups {
            self.render_group(ui, app, &mut tab_actions, group);
        }
        for action in tab_actions {
            match action {
                TabAction::Activate { gid, index } => {
                    app.dock.active_group = gid;
                    if let Some(g) = app.dock.group_mut(gid) {
                        g.active = index;
                        if let Some(panel) = g.active_panel() {
                            app.app.focused_panel = panel.to_string();
                        }
                    }
                }
                TabAction::Close { panel } => app.dock.close_panel(&panel),
            }
        }

        // ---------- Panel-Inhalte ----------
        for group in &groups {
            let content = Rect::new(
                group.rect.x,
                group.rect.y + TAB_H,
                group.rect.w,
                group.rect.h - TAB_H,
            );
            let Some(panel_id) = group.tabs.get(group.active) else { continue };
            // Fokus-Tracking: Klick in den Inhalt fokussiert das Panel.
            if ui.mouse_in(content) && (ui.input.left_pressed || ui.input.right_pressed) {
                app.app.focused_panel = panel_id.clone();
                app.dock.active_group = group.gid;
            }
            let panel_id = panel_id.clone();
            ui.push_clip(content);
            panel_host.update(&panel_id, ui, app, services, content);
            ui.pop_clip();
        }

        // ---------- Tab-Drag: Drop-Zonen ----------
        self.handle_tab_drop(ui, app, &groups);
    }

    fn handle_sashes(&mut self, ui: &mut Ui, app: &mut AppState, sashes: &[SashLayout]) {
        if let Some(drag) = &self.sash_drag {
            if !ui.input.left_down {
                self.sash_drag = None;
            } else {
                let drag_path = drag.path.clone();
                let drag_index = drag.index;
                // Aktiven Sash finden (Geometrie aus diesem Frame).
                let Some(sash) = sashes
                    .iter()
                    .find(|s| s.path == drag_path && s.index == drag_index)
                else {
                    self.sash_drag = None;
                    return;
                };
                let cursor = match sash.dir {
                    SplitDir::Row => MouseCursor::MOUSE_CURSOR_RESIZE_EW,
                    SplitDir::Col => MouseCursor::MOUSE_CURSOR_RESIZE_NS,
                };
                ui.want_cursor(cursor);
                // Aktiver Sash: Akzentlinie (2 px) auf der Fuge
                let line = match sash.dir {
                    SplitDir::Row => Rect::new(
                        sash.rect.x + (SASH_HIT - GAP) / 2.0,
                        sash.rect.y,
                        GAP,
                        sash.rect.h,
                    ),
                    SplitDir::Col => Rect::new(
                        sash.rect.x,
                        sash.rect.y + (SASH_HIT - GAP) / 2.0,
                        sash.rect.w,
                        GAP,
                    ),
                };
                ui.fill_rounded(line, theme::RADIUS_XS, theme::ACCENT);

                let delta_px = match sash.dir {
                    SplitDir::Row => ui.input.mouse_delta.x,
                    SplitDir::Col => ui.input.mouse_delta.y,
                };
                if delta_px != 0.0 {
                    let min_w = MIN_PANE / sash.total.max(1.0);
                    if let Some(root) = &mut app.dock.root {
                        if let Some(DockNode::Split { children, .. }) =
                            split_by_path(root, &drag_path)
                        {
                            let weight_sum: f32 = children.iter().map(|c| c.weight).sum();
                            let delta_w = delta_px / sash.total.max(1.0) * weight_sum;
                            let a = children[drag_index].weight;
                            let b = children[drag_index + 1].weight;
                            let min = min_w * weight_sum;
                            let new_a = (a + delta_w).clamp(min, a + b - min);
                            children[drag_index].weight = new_a;
                            children[drag_index + 1].weight = a + b - new_a;
                        }
                    }
                }
                return;
            }
        }

        for sash in sashes {
            if ui.mouse_in(sash.rect) && ui.nothing_active() && ui.active_drag().is_none() {
                let cursor = match sash.dir {
                    SplitDir::Row => MouseCursor::MOUSE_CURSOR_RESIZE_EW,
                    SplitDir::Col => MouseCursor::MOUSE_CURSOR_RESIZE_NS,
                };
                ui.want_cursor(cursor);
                if ui.input.left_pressed {
                    self.sash_drag = Some(SashDrag {
                        path: sash.path.clone(),
                        index: sash.index,
                    });
                }
            }
        }
    }

    fn render_group(
        &mut self,
        ui: &mut Ui,
        app: &AppState,
        actions: &mut Vec<TabAction>,
        group: &GroupLayout,
    ) {
        let rect = group.rect;
        let is_active_group = app.dock.active_group == group.gid;

        // Gruppe: Inhalt surface-1 (rounded 4), Tab-Leiste surface-0
        ui.fill_rounded(rect, theme::RADIUS_SM, theme::SURFACE_1);
        let tab_bar = Rect::new(rect.x, rect.y, rect.w, TAB_H);
        ui.fill_rounded(
            Rect::new(tab_bar.x, tab_bar.y, tab_bar.w, TAB_H),
            theme::RADIUS_SM,
            theme::SURFACE_0,
        );
        // untere Hälfte der Tab-Bar eckig (nur obere Ecken gerundet)
        ui.fill(
            Rect::new(tab_bar.x, tab_bar.y + TAB_H / 2.0, tab_bar.w, TAB_H / 2.0),
            theme::SURFACE_0,
        );
        // feine Linie unter der Tab-Leiste
        ui.hline(tab_bar.x, tab_bar.bottom() - 1.0, tab_bar.w, theme::LINE);

        let mut x = tab_bar.x;
        for (i, panel_id) in group.tabs.iter().enumerate() {
            let title = panel_title(panel_id);
            let icon = panel_icon(panel_id);
            let font = FontKind::Sans12;
            let title_w = ui.font(font).width(title).min(160.0);
            // pl-2 + Icon 14 + gap-1.5 + Titel + gap-1.5 + Close 16 + pr-1
            let tab_w = 8.0 + 14.0 + 6.0 + title_w + 6.0 + 16.0 + 4.0;
            let tab = Rect::new(x, tab_bar.y, tab_w, TAB_H);

            let visible = i == group.active;
            let id = ui.id(("dock.tab", group.gid, panel_id.as_str()));
            let it = ui.interact(id, tab);

            // Tab-Hintergrund: sichtbarer Tab opak (deckt die Linie ab)
            if visible {
                ui.fill(
                    Rect::new(tab.x, tab.y, tab.w, TAB_H),
                    theme::SURFACE_1,
                );
                if is_active_group {
                    // Akzentlinie oben (inset 0 2px 0 0 accent)
                    ui.fill(Rect::new(tab.x, tab.y, tab.w, 2.0), theme::ACCENT);
                }
            } else if it.hovered {
                ui.fill(
                    Rect::new(tab.x, tab.y, tab.w, TAB_H),
                    theme::with_alpha(theme::SURFACE_2, 153),
                );
            }

            let fg = match (is_active_group, visible) {
                (true, true) => theme::TEXT_1,
                (false, true) => theme::TEXT_2,
                _ => theme::TEXT_3,
            };
            let mut inner = tab;
            inner.cut_left(8.0);
            let icon_cell = inner.cut_left(14.0);
            ui.icon(icon, icon_cell, 14.0, theme::with_alpha(fg, 204));
            inner.cut_left(6.0);
            let label_cell = inner.cut_left(title_w);
            let display = ui.font(font).ellipsize(title, title_w);
            ui.text_left(&display, label_cell, fg, font);
            inner.cut_left(6.0);

            // Schließen-Knopf (size-4, Icon 12) bei Hover
            if it.hovered || visible && ui.mouse_in(tab) {
                let close = Rect::new(inner.x, tab.y + (TAB_H - 16.0) / 2.0, 16.0, 16.0);
                let close_id = ui.id(("dock.tab.close", group.gid, panel_id.as_str()));
                let close_it = ui.interact(close_id, close);
                if close_it.hovered {
                    ui.fill_rounded(close, theme::RADIUS_XS, theme::SURFACE_3);
                }
                ui.icon(
                    "x",
                    close,
                    12.0,
                    if close_it.hovered {
                        theme::TEXT_1
                    } else {
                        theme::TEXT_3
                    },
                );
                if close_it.clicked {
                    actions.push(TabAction::Close {
                        panel: panel_id.clone(),
                    });
                    x += tab_w;
                    continue;
                }
            }

            if it.hovered {
                ui.want_cursor(MouseCursor::MOUSE_CURSOR_POINTING_HAND);
            }
            if it.hovered && ui.input.left_pressed {
                actions.push(TabAction::Activate {
                    gid: group.gid,
                    index: i,
                });
                // Tab-Drag-Kandidat
                ui.start_drag(DragPayload::Tab {
                    panel: panel_id.clone(),
                });
            }
            x += tab_w;
        }
    }

    fn handle_tab_drop(&mut self, ui: &mut Ui, app: &mut AppState, groups: &[GroupLayout]) {
        self.drop_preview = None;
        let dragging_panel = match ui.active_drag() {
            Some(DragPayload::Tab { panel }) => panel.clone(),
            _ => return,
        };
        let mouse = ui.input.mouse;

        for group in groups {
            if !group.rect.contains(mouse) {
                continue;
            }
            let tab_bar = Rect::new(group.rect.x, group.rect.y, group.rect.w, TAB_H);
            let (edge, preview, tab_index) = if tab_bar.contains(mouse) {
                // In der Tab-Leiste: als Tab einfügen (Index nach X-Position)
                let mut index = group.tabs.len();
                let mut x = tab_bar.x;
                for (i, panel_id) in group.tabs.iter().enumerate() {
                    let title = panel_title(panel_id);
                    let title_w = ui.font(FontKind::Sans12).width(title).min(160.0);
                    let tab_w = 8.0 + 14.0 + 6.0 + title_w + 6.0 + 16.0 + 4.0;
                    if mouse.x < x + tab_w / 2.0 {
                        index = i;
                        break;
                    }
                    x += tab_w;
                }
                (DropEdge::Center, tab_bar, Some(index))
            } else {
                let rel_x = (mouse.x - group.rect.x) / group.rect.w;
                let rel_y = (mouse.y - group.rect.y) / group.rect.h;
                let r = group.rect;
                if rel_x < 0.25 {
                    (DropEdge::Left, Rect::new(r.x, r.y, r.w / 2.0, r.h), None)
                } else if rel_x > 0.75 {
                    (
                        DropEdge::Right,
                        Rect::new(r.x + r.w / 2.0, r.y, r.w / 2.0, r.h),
                        None,
                    )
                } else if rel_y < 0.25 {
                    (DropEdge::Top, Rect::new(r.x, r.y, r.w, r.h / 2.0), None)
                } else if rel_y > 0.75 {
                    (
                        DropEdge::Bottom,
                        Rect::new(r.x, r.y + r.h / 2.0, r.w, r.h / 2.0),
                        None,
                    )
                } else {
                    (DropEdge::Center, r, None)
                }
            };

            // Drop-Overlay: accent 16 % + Border accent
            ui.fill_rounded(preview, theme::RADIUS_SM, theme::with_alpha(theme::ACCENT, 41));
            ui.stroke_rounded(preview, theme::RADIUS_SM, 1.0, theme::ACCENT);
            self.drop_preview = Some((group.gid, edge, preview, tab_index));
            break;
        }

        if ui.input.left_released {
            if let Some((gid, edge, _, tab_index)) = self.drop_preview.take() {
                app.dock.move_panel(&dragging_panel, gid, edge, tab_index);
                app.app.focused_panel = dragging_panel;
            }
        }
    }
}

enum TabAction {
    Activate { gid: GroupId, index: usize },
    Close { panel: String },
}
