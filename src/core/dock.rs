//! Docking-Modell: Split-Baum mit gewichteten Kindern und Tab-Gruppen,
//! Default-Layouts je Workspace, Persistenz pro Workspace als JSON.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub type GroupId = u64;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum SplitDir {
    Row,
    Col,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DockNode {
    Split {
        dir: SplitDir,
        children: Vec<DockChild>,
    },
    Group(DockGroup),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DockChild {
    pub weight: f32,
    pub node: DockNode,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DockGroup {
    pub id: GroupId,
    pub tabs: Vec<String>,
    pub active: usize,
}

impl DockGroup {
    pub fn active_panel(&self) -> Option<&str> {
        self.tabs.get(self.active).map(|s| s.as_str())
    }
}

/// Andock-Kante beim Tab-Drop.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DropEdge {
    Center,
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Serialize, Deserialize)]
struct SavedLayout {
    root: DockNode,
    active_group: GroupId,
    next_group_id: GroupId,
}

pub struct DockManager {
    pub root: Option<DockNode>,
    pub active_group: GroupId,
    next_group_id: GroupId,
    /// Workspace, dessen Layout gerade geladen ist.
    pub loaded_workspace: Option<String>,
}

impl Default for DockManager {
    fn default() -> Self {
        DockManager {
            root: None,
            active_group: 0,
            next_group_id: 1,
            loaded_workspace: None,
        }
    }
}

fn layout_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("editron")
        .join("layouts")
}

fn layout_path(workspace: &str) -> PathBuf {
    layout_dir().join(format!("layout.v1.{workspace}.json"))
}

impl DockManager {
    fn next_id(&mut self) -> GroupId {
        let id = self.next_group_id;
        self.next_group_id += 1;
        id
    }

    fn group(&mut self, tabs: &[&str], active: usize) -> DockNode {
        DockNode::Group(DockGroup {
            id: self.next_id(),
            tabs: tabs.iter().map(|s| s.to_string()).collect(),
            active,
        })
    }

    // ------------------------------------------------------ Persistenz

    pub fn save_layout_for(&self, workspace: &str) {
        let Some(root) = &self.root else { return };
        let saved = SavedLayout {
            root: root.clone(),
            active_group: self.active_group,
            next_group_id: self.next_group_id,
        };
        let dir = layout_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(json) = serde_json::to_string(&saved) {
            let _ = std::fs::write(layout_path(workspace), json);
        }
    }

    /// Layout laden: gespeichertes JSON, bei Fehler Default bauen.
    pub fn load_workspace_layout(&mut self, workspace: &str) {
        self.loaded_workspace = Some(workspace.to_string());
        if let Ok(raw) = std::fs::read_to_string(layout_path(workspace)) {
            if let Ok(saved) = serde_json::from_str::<SavedLayout>(&raw) {
                self.root = Some(saved.root);
                self.active_group = saved.active_group;
                self.next_group_id = saved.next_group_id;
                return;
            }
            let _ = std::fs::remove_file(layout_path(workspace));
        }
        self.build_default_layout(workspace);
    }

    pub fn reset_current_layout(&mut self) {
        if let Some(ws) = self.loaded_workspace.clone() {
            let _ = std::fs::remove_file(layout_path(&ws));
            self.build_default_layout(&ws);
        }
    }

    // -------------------------------------------------- Default-Layouts
    // Premiere-artige Vorgaben: Panels, Flächenanteile und das initial
    // aktive Panel je Workspace.

    pub fn build_default_layout(&mut self, workspace: &str) {
        let root = match workspace {
            // Links groß der Medien-Browser, rechts oben Quelle,
            // rechts unten Info + Verlauf als Tab-Stapel.
            "media" => {
                let media = self.group(&["media"], 0);
                let source = self.group(&["source"], 0);
                let info = self.group(&["info", "history"], 0);
                DockNode::Split {
                    dir: SplitDir::Row,
                    children: vec![
                        DockChild { weight: 0.62, node: media },
                        DockChild {
                            weight: 0.38,
                            node: DockNode::Split {
                                dir: SplitDir::Col,
                                children: vec![
                                    DockChild { weight: 0.55, node: source },
                                    DockChild { weight: 0.45, node: info },
                                ],
                            },
                        },
                    ],
                }
            }
            // Oben Programm + Quelle als Tabs (Programm aktiv), unten links
            // Medien + Effekte als Tabs, unten rechts die Timeline.
            "edit" => {
                let preview = self.group(&["program", "source"], 0);
                let media = self.group(&["media", "effects", "subtitles", "markers"], 0);
                let timeline = self.group(&["timeline"], 0);
                DockNode::Split {
                    dir: SplitDir::Col,
                    children: vec![
                        DockChild { weight: 0.55, node: preview },
                        DockChild {
                            weight: 0.45,
                            node: DockNode::Split {
                                dir: SplitDir::Row,
                                children: vec![
                                    DockChild { weight: 0.30, node: media },
                                    DockChild { weight: 0.70, node: timeline },
                                ],
                            },
                        },
                    ],
                }
            }
            "color" => self.three_column_layout("scopes", 0.26, "color", 0.28),
            // Effekteinstellungen breit: rechts liegen die Keyframe-Spuren.
            "effects" => self.three_column_layout("effects", 0.18, "effectControls", 0.40),
            "audio" => self.three_column_layout("audioMixer", 0.24, "info", 0.22),
            "graphics" => self.three_column_layout("media", 0.20, "graphics", 0.28),
            _ => {
                let g = self.group(&["media"], 0);
                g
            }
        };
        self.root = Some(root);

        // Initial aktives Panel je Workspace.
        let initial = match workspace {
            "media" => "media",
            "edit" => "timeline",
            _ => "program",
        };
        if let Some(gid) = self.find_panel(initial).map(|(gid, _)| gid) {
            self.active_group = gid;
        }
    }

    /// Spalten-Layout der Workspaces Farbe/Effekte/Audio/Grafik:
    /// [links | Programm | rechts] oben, Timeline (30 %) unten.
    fn three_column_layout(
        &mut self,
        left: &str,
        left_w: f32,
        right: &str,
        right_w: f32,
    ) -> DockNode {
        let left_g = self.group(&[left], 0);
        let program = self.group(&["program"], 0);
        let right_g = self.group(&[right], 0);
        let timeline = self.group(&["timeline"], 0);
        DockNode::Split {
            dir: SplitDir::Col,
            children: vec![
                DockChild {
                    weight: 0.70,
                    node: DockNode::Split {
                        dir: SplitDir::Row,
                        children: vec![
                            DockChild { weight: left_w, node: left_g },
                            DockChild {
                                weight: 1.0 - left_w - right_w,
                                node: program,
                            },
                            DockChild { weight: right_w, node: right_g },
                        ],
                    },
                },
                DockChild { weight: 0.30, node: timeline },
            ],
        }
    }

    // ----------------------------------------------------- Abfragen

    pub fn find_panel(&self, panel: &str) -> Option<(GroupId, usize)> {
        fn walk(node: &DockNode, panel: &str) -> Option<(GroupId, usize)> {
            match node {
                DockNode::Group(g) => g
                    .tabs
                    .iter()
                    .position(|t| t == panel)
                    .map(|idx| (g.id, idx)),
                DockNode::Split { children, .. } => {
                    children.iter().find_map(|c| walk(&c.node, panel))
                }
            }
        }
        self.root.as_ref().and_then(|r| walk(r, panel))
    }

    pub fn group_mut(&mut self, id: GroupId) -> Option<&mut DockGroup> {
        fn walk(node: &mut DockNode, id: GroupId) -> Option<&mut DockGroup> {
            match node {
                DockNode::Group(g) => (g.id == id).then_some(g),
                DockNode::Split { children, .. } => {
                    children.iter_mut().find_map(|c| walk(&mut c.node, id))
                }
            }
        }
        self.root.as_mut().and_then(|r| walk(r, id))
    }

    pub fn first_group_id(&self) -> Option<GroupId> {
        fn walk(node: &DockNode) -> Option<GroupId> {
            match node {
                DockNode::Group(g) => Some(g.id),
                DockNode::Split { children, .. } => children.iter().find_map(|c| walk(&c.node)),
            }
        }
        self.root.as_ref().and_then(walk)
    }

    /// Aktives Panel der aktiven Gruppe (Kontext-Key `panel`-Quelle bei Fokuswechsel).
    pub fn active_panel(&self) -> Option<String> {
        fn walk(node: &DockNode, id: GroupId) -> Option<String> {
            match node {
                DockNode::Group(g) => {
                    (g.id == id).then(|| g.active_panel().unwrap_or("").to_string())
                }
                DockNode::Split { children, .. } => {
                    children.iter().find_map(|c| walk(&c.node, id))
                }
            }
        }
        self.root.as_ref().and_then(|r| walk(r, self.active_group))
    }

    // -------------------------------------------------- Mutationen

    /// Panel öffnen bzw. fokussieren: existiert es, wird es aktiviert,
    /// sonst in der aktiven Gruppe als Tab hinzugefügt.
    pub fn open_panel(&mut self, panel: &str) {
        if let Some((gid, idx)) = self.find_panel(panel) {
            self.active_group = gid;
            if let Some(g) = self.group_mut(gid) {
                g.active = idx;
            }
            return;
        }
        let target = if self.group_mut(self.active_group).is_some() {
            self.active_group
        } else if let Some(first) = self.first_group_id() {
            first
        } else {
            // Leeres Dock: Root-Gruppe anlegen.
            let g = DockGroup {
                id: self.next_id(),
                tabs: vec![panel.to_string()],
                active: 0,
            };
            self.active_group = g.id;
            self.root = Some(DockNode::Group(g));
            return;
        };
        if let Some(g) = self.group_mut(target) {
            g.tabs.push(panel.to_string());
            g.active = g.tabs.len() - 1;
            self.active_group = target;
        }
    }

    /// Tab schließen; leere Gruppen kollabieren.
    pub fn close_panel(&mut self, panel: &str) {
        let Some((gid, idx)) = self.find_panel(panel) else { return };
        if let Some(g) = self.group_mut(gid) {
            g.tabs.remove(idx);
            if g.active >= g.tabs.len() && !g.tabs.is_empty() {
                g.active = g.tabs.len() - 1;
            }
            if !g.tabs.is_empty() {
                return;
            }
        }
        self.remove_group(gid);
    }

    fn remove_group(&mut self, gid: GroupId) {
        fn prune(node: DockNode, gid: GroupId) -> Option<DockNode> {
            match node {
                DockNode::Group(g) => {
                    if g.id == gid {
                        None
                    } else {
                        Some(DockNode::Group(g))
                    }
                }
                DockNode::Split { dir, children } => {
                    let mut kept: Vec<DockChild> = children
                        .into_iter()
                        .filter_map(|c| {
                            prune(c.node, gid).map(|node| DockChild {
                                weight: c.weight,
                                node,
                            })
                        })
                        .collect();
                    match kept.len() {
                        0 => None,
                        1 => Some(kept.pop().unwrap().node),
                        _ => {
                            // Gewichte renormalisieren
                            let total: f32 = kept.iter().map(|c| c.weight).sum();
                            if total > 0.0 {
                                for c in &mut kept {
                                    c.weight /= total;
                                }
                            }
                            Some(DockNode::Split { dir, children: kept })
                        }
                    }
                }
            }
        }
        if let Some(root) = self.root.take() {
            self.root = prune(root, gid);
        }
        if self.active_group == gid {
            self.active_group = self.first_group_id().unwrap_or(0);
        }
    }

    /// Tab per Drag&Drop verschieben: `panel` aus seiner Gruppe lösen und an
    /// `target` andocken (center = als Tab, sonst Split an der Kante).
    pub fn move_panel(&mut self, panel: &str, target_group: GroupId, edge: DropEdge, tab_index: Option<usize>) {
        let Some((src_gid, _)) = self.find_panel(panel) else { return };
        // No-op: Tab auf die eigene Gruppe (center) ziehen, wenn er allein ist.
        if src_gid == target_group && edge == DropEdge::Center {
            if let Some(g) = self.group_mut(src_gid) {
                if g.tabs.len() == 1 {
                    return;
                }
                // Reorder innerhalb der Gruppe
                let from = g.tabs.iter().position(|t| t == panel).unwrap();
                let tab = g.tabs.remove(from);
                let mut to = tab_index.unwrap_or(g.tabs.len()).min(g.tabs.len());
                if to > from {
                    // remove hat Indizes verschoben — Zielindex bezieht sich
                    // auf die Liste inkl. des entfernten Tabs.
                    to -= 1;
                }
                g.tabs.insert(to, tab);
                g.active = to;
                self.active_group = src_gid;
                return;
            }
        }

        // Quelle entfernen (Gruppe ggf. kollabieren) — Ziel-Gruppe bleibt
        // bestehen, weil sie nicht leer wird (sie ist nicht die Quelle, oder
        // die Quelle hat >1 Tabs).
        let removed_src_group = {
            let g = self.group_mut(src_gid).unwrap();
            let idx = g.tabs.iter().position(|t| t == panel).unwrap();
            g.tabs.remove(idx);
            if g.active >= g.tabs.len() && !g.tabs.is_empty() {
                g.active = g.tabs.len() - 1;
            }
            g.tabs.is_empty()
        };
        if removed_src_group && src_gid != target_group {
            self.remove_group(src_gid);
        }

        match edge {
            DropEdge::Center => {
                if let Some(g) = self.group_mut(target_group) {
                    let to = tab_index.unwrap_or(g.tabs.len()).min(g.tabs.len());
                    g.tabs.insert(to, panel.to_string());
                    g.active = to;
                    self.active_group = target_group;
                }
            }
            _ => {
                let new_group = DockGroup {
                    id: self.next_id(),
                    tabs: vec![panel.to_string()],
                    active: 0,
                };
                let new_gid = new_group.id;
                self.split_at_group(target_group, edge, DockNode::Group(new_group));
                self.active_group = new_gid;
            }
        }
    }

    /// Teilt die Gruppe `target` an `edge` und setzt `node` daneben (50/50).
    fn split_at_group(&mut self, target: GroupId, edge: DropEdge, node: DockNode) {
        fn walk(current: &mut DockNode, target: GroupId, edge: DropEdge, node: &mut Option<DockNode>) {
            let is_target = matches!(current, DockNode::Group(g) if g.id == target);
            if is_target {
                let new_node = node.take().unwrap();
                let dir = match edge {
                    DropEdge::Left | DropEdge::Right => SplitDir::Row,
                    _ => SplitDir::Col,
                };
                let old = std::mem::replace(
                    current,
                    DockNode::Split {
                        dir,
                        children: Vec::new(),
                    },
                );
                let (first, second) = match edge {
                    DropEdge::Left | DropEdge::Top => (new_node, old),
                    _ => (old, new_node),
                };
                if let DockNode::Split { children, .. } = current {
                    children.push(DockChild { weight: 0.5, node: first });
                    children.push(DockChild { weight: 0.5, node: second });
                }
                return;
            }
            if let DockNode::Split { children, .. } = current {
                for c in children {
                    walk(&mut c.node, target, edge, node);
                    if node.is_none() {
                        return;
                    }
                }
            }
        }
        let mut payload = Some(node);
        if let Some(root) = &mut self.root {
            walk(root, target, edge, &mut payload);
        }
    }

    /// Alle Panels, die im aktuellen Layout offen sind.
    pub fn open_panels(&self) -> Vec<String> {
        fn walk(node: &DockNode, out: &mut Vec<String>) {
            match node {
                DockNode::Group(g) => out.extend(g.tabs.iter().cloned()),
                DockNode::Split { children, .. } => {
                    for c in children {
                        walk(&c.node, out);
                    }
                }
            }
        }
        let mut out = Vec::new();
        if let Some(r) = &self.root {
            walk(r, &mut out);
        }
        out
    }
}
