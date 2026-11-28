#[cfg(windows)]
use crate::folder_app;
use crate::fsx::{self, Bg, Msg};
use crate::pane::{Action, Pane, Rename, SearchState};
use crate::preview::{self, Preview};
use crate::text::{fold, fuzzy_score};
use crate::theme::{self, Glyph};
use eframe::egui::{self, Key};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

enum Dialog {
    NewFolder { pane: usize, name: String, focus: bool },
    ConfirmDelete { paths: Vec<PathBuf> },
    Search { pane: usize, query: String, focus: bool },
    Palette { text: String, sel: usize, focus: bool },
    Errors(Vec<String>),
    Preview(Preview),
    Help,
    #[cfg(windows)]
    FolderApp { status: std::io::Result<folder_app::Status>, error: Option<String> },
}

struct Tab {
    /// Stable identity, so a tab can be dragged while its position changes.
    id: u64,
    pane: Pane,
}

struct PaneTabs {
    tabs: Vec<Tab>,
    selected: usize,
    scroll_to_selected: bool,
}

impl PaneTabs {
    fn new(id: u64, path: PathBuf) -> Self {
        Self {
            tabs: vec![Tab { id, pane: Pane::new(path) }],
            selected: 0,
            scroll_to_selected: true,
        }
    }

    fn pane(&self) -> &Pane {
        &self.tabs[self.selected].pane
    }

    fn pane_mut(&mut self) -> &mut Pane {
        &mut self.tabs[self.selected].pane
    }
}

/// Where the tab strips and panes were drawn this frame, for dragging tabs between panes.
#[derive(Default)]
struct TabLayout {
    /// The area of each pane's tab strip (drop zone).
    zones: [Option<egui::Rect>; 2],
    /// Each tab's rectangle, per pane, in order.
    rects: [Vec<egui::Rect>; 2],
    /// Each pane's area (dropping a tab there appends it to that pane's tabs).
    panes: [Option<egui::Rect>; 2],
    /// A tab being dragged this frame: (pane, index).
    dragging: Option<(usize, usize)>,
    /// A tab released this frame: (pane, index).
    dropped: Option<(usize, usize)>,
}

/// Where a dragged tab would land in the other pane.
#[derive(Clone, Copy)]
enum TabTarget {
    /// In the pane's tab strip, before tab `at`.
    Strip(usize, usize),
    /// On the pane itself: after its last tab.
    Pane(usize),
}

pub struct App {
    panes: [PaneTabs; 2],
    tab_layout: TabLayout,
    active: usize,
    dual: bool,
    next_tab_id: u64,
    /// Tab strip on the left (stacked vertically) instead of along the top.
    tabs_vertical: bool,
    /// Dual pane arranged top/bottom instead of left/right.
    split_stacked: bool,
    thumbs: crate::thumbs::Thumbs,
    ui_zoom: f32,
    type_to_filter: bool,
    show_hidden: bool,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    ctx: egui::Context,
    next_req: u64,
    clipboard: Option<(Vec<PathBuf>, bool)>,
    status: String,
    busy: bool,
    dialog: Option<Dialog>,
    preview_running: bool,
    recent: Vec<PathBuf>,
    /// Sidebar favourites as (label, folder), in display order.
    quick: Vec<(String, PathBuf)>,
    /// Favourites were reordered by a drag that is still going; save when it ends.
    fav_dirty: bool,
    drives: Vec<PathBuf>,
    font: Option<String>,
    /// Frame-rate cap on Windows, where vsync is off (see `pace.rs`).
    pacer: crate::pace::Pacer,
    /// The last drag of files out of the window (see `dragout.rs`).
    drag_out: Option<DragOut>,
}

struct DragOut {
    paths: Vec<PathBuf>,
    /// When egui saw the mouse button released after the drag; None while it is still going.
    released: Option<std::time::Instant>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let font = crate::fonts::install(&cc.egui_ctx);
        theme::install(&cc.egui_ctx);
        let ui_zoom = crate::config::load_ui_zoom();
        cc.egui_ctx.set_zoom_factor(ui_zoom);
        let (tx, rx) = channel();
        let mut args = std::env::args_os().skip(1);
        let start = args.next().and_then(|first| {
            #[cfg(windows)]
            if first == folder_app::OPEN_FLAG {
                return args.next().and_then(folder_app::shell_path);
            }
            let path = PathBuf::from(first);
            path.is_dir().then_some(path)
        }).unwrap_or_else(fsx::home_dir);
        let mut app = App {
            panes: [PaneTabs::new(0, start.clone()), PaneTabs::new(1, start)],
            tab_layout: TabLayout::default(),
            active: 0,
            dual: false,
            next_tab_id: 2,
            tabs_vertical: false,
            split_stacked: false,
            thumbs: crate::thumbs::Thumbs::new(),
            ui_zoom,
            type_to_filter: true,
            show_hidden: false,
            tx,
            rx,
            ctx: cc.egui_ctx.clone(),
            next_req: 1,
            clipboard: None,
            status: String::new(),
            busy: false,
            dialog: None,
            preview_running: false,
            recent: Vec::new(),
            quick: initial_favorites(),
            fav_dirty: false,
            drives: fsx::drives(),
            font,
            pacer: crate::pace::Pacer::new(),
            drag_out: None,
        };
        if app.font.is_none() {
            app.status = "No Japanese font found; set FONT_8823 to a .ttf/.ttc path".into();
        }
        app.publish_favorites();
        app.load(0);
        app
    }

    /// Let the panes see the favourites, so their menus can offer Add or Remove.
    fn publish_favorites(&self) {
        let paths: Vec<PathBuf> = self.quick.iter().map(|(_, p)| p.clone()).collect();
        self.ctx.data_mut(|d| d.insert_temp(crate::pane::favorites_id(), Arc::new(paths)));
    }

    fn favorites_changed(&mut self) {
        self.publish_favorites();
        let paths: Vec<PathBuf> = self.quick.iter().map(|(_, p)| p.clone()).collect();
        if let Err(e) = crate::config::save_favorites(&paths) {
            self.status = format!("Could not save Favorites: {e}");
        }
    }

    fn is_favorite(&self, path: &Path) -> bool {
        self.quick.iter().any(|(_, p)| p == path)
    }

    fn add_favorite(&mut self, path: PathBuf) {
        if self.is_favorite(&path) {
            self.status = "Already in Favorites".into();
            return;
        }
        let name = favorite_name(&path, &fsx::quick_access(&fsx::standard_folders()));
        self.status = format!("Added to Favorites: {name}");
        self.quick.push((name, path));
        self.favorites_changed();
    }

    fn remove_favorite(&mut self, path: &Path) {
        if let Some(i) = self.quick.iter().position(|(_, p)| p == path) {
            let (name, _) = self.quick.remove(i);
            self.status = format!("Removed from Favorites: {name}");
            self.favorites_changed();
        }
    }

    fn bg(&self) -> Bg {
        Bg { tx: self.tx.clone(), ctx: self.ctx.clone() }
    }

    fn load(&mut self, p: usize) {
        let req = self.next_req;
        self.next_req += 1;
        let pane = self.panes[p].pane_mut();
        pane.req = req;
        pane.loading = true;
        let path = pane.path.clone();
        let (sort, asc) = (pane.sort, pane.asc);
        fsx::spawn_listing(self.bg(), p, req, path, sort, asc);
    }

    fn navigate(&mut self, p: usize, path: PathBuf, record: bool) {
        if !path.is_dir() {
            self.panes[p].pane_mut().error = Some(format!("Not a folder: {}", path.display()));
            return;
        }
        let pane = self.panes[p].pane_mut();
        if let Some(s) = pane.search.take() {
            s.cancel.store(true, Ordering::Relaxed);
        }
        if record && pane.path != path {
            pane.back.push(pane.path.clone());
            pane.fwd.clear();
        }
        pane.path = path.clone();
        pane.filter.clear();
        pane.error = None;
        pane.rename = None;
        pane.scroll_y = 0.0;
        self.recent.retain(|r| r != &path);
        self.recent.insert(0, path);
        self.recent.truncate(200);
        self.load(p);
    }

    fn switch_tab(&mut self, p: usize, to: usize) {
        let group = &mut self.panes[p];
        if to >= group.tabs.len() {
            return;
        }
        self.active = p;
        group.scroll_to_selected = true;
        if to != group.selected {
            group.selected = to;
            // The folder may have changed while the tab was in the background.
            if group.pane().search.is_none() {
                self.load(p);
            }
        }
    }

    fn new_tab(&mut self, p: usize, path: PathBuf) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let group = &mut self.panes[p];
        let mut pane = Pane::new(path);
        pane.gallery = group.pane().gallery;
        let to = group.selected + 1;
        group.tabs.insert(to, Tab { id, pane });
        self.switch_tab(p, to);
    }

    fn close_tab(&mut self, p: usize, i: usize) {
        let group = &self.panes[p];
        if group.tabs.len() <= 1 {
            return;
        }
        if i == group.selected {
            let to = if i + 1 < group.tabs.len() { i + 1 } else { i - 1 };
            self.switch_tab(p, to);
        }
        let group = &mut self.panes[p];
        let tab = group.tabs.remove(i);
        if let Some(s) = tab.pane.search {
            s.cancel.store(true, Ordering::Relaxed);
        }
        if i < group.selected {
            group.selected -= 1;
        }
    }

    /// Find the tab that started the listing or search. Tabs can move between panes, so
    /// look in both; request ids are unique.
    fn pane_for(&mut self, _pane: usize, req: u64) -> Option<&mut Pane> {
        self.panes
            .iter_mut()
            .flat_map(|group| group.tabs.iter_mut())
            .map(|tab| &mut tab.pane)
            .find(|p| p.req == req)
    }

    fn move_tab(&mut self, p: usize, from: usize, to: usize) {
        let group = &mut self.panes[p];
        let selected_id = group.tabs[group.selected].id;
        let tab = group.tabs.remove(from);
        group.tabs.insert(to, tab);
        group.selected = group.tabs.iter().position(|tab| tab.id == selected_id).unwrap();
    }

    /// The tab strip: along the top (`vertical == false`) or stacked in a left column.
    /// Click to switch, drag to reorder or onto the other pane, middle-click or × to close.
    fn tab_bar(&mut self, ui: &mut egui::Ui, p: usize, vertical: bool) {
        let mut switch = None;
        let mut close = None;
        let mut rects: Vec<egui::Rect> = Vec::new();
        let mut dragging: Option<usize> = None;
        let mut dropped: Option<usize> = None;
        let group = &mut self.panes[p];
        let scroll_to_selected = std::mem::take(&mut group.scroll_to_selected);
        let n = group.tabs.len();
        let font = egui::FontId::proportional(13.0);
        let mut body = |ui: &mut egui::Ui| {
            for (i, tab) in group.tabs.iter().enumerate() {
                let path = &tab.pane.path;
                let title = path.file_name().map(crate::text::display_name).unwrap_or_else(|| path.display().to_string());
                let selected = i == group.selected;
                let pal = theme::pal(ui);
                let color = if selected { pal.strong } else { pal.weak };
                let chrome = if n > 1 { 42.0 } else { 22.0 };
                let max_w = if vertical {
                    (ui.available_width() - chrome).max(0.0)
                } else {
                    (ui.clip_rect().width() - chrome).clamp(0.0, 180.0)
                };
                let galley = theme::elided(ui, &title, font.clone(), color, max_w);
                let text_w = galley.size().x.min(max_w);
                let w = if vertical { ui.available_width() } else { text_w + chrome };
                let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 28.0), egui::Sense::hover());
                let id = egui::Id::unique(("tab", tab.id));
                let r = ui.interact(rect, id, egui::Sense::click_and_drag());
                if selected && scroll_to_selected {
                    r.scroll_to_me(None);
                }
                let fill = if selected {
                    pal.surface
                } else if r.hovered() || r.dragged() {
                    pal.hover
                } else {
                    egui::Color32::TRANSPARENT
                };
                ui.painter().rect_filled(rect, 6.0, fill);
                if selected {
                    // Accent marker: under the tab when tabs are on top, beside it when on the left.
                    let mark = if vertical {
                        egui::Rect::from_min_max(egui::pos2(rect.left(), rect.top() + 6.0), egui::pos2(rect.left() + 3.0, rect.bottom() - 6.0))
                    } else {
                        egui::Rect::from_min_max(egui::pos2(rect.left() + 8.0, rect.bottom() - 2.5), egui::pos2(rect.right() - 8.0, rect.bottom() - 0.5))
                    };
                    ui.painter().rect_filled(mark, 1.5, pal.accent);
                }
                let tx = if vertical { 12.0 } else { 10.0 };
                let text_clip = egui::Rect::from_min_max(rect.min, egui::pos2(rect.left() + tx + text_w, rect.bottom()));
                ui.painter().with_clip_rect(text_clip).galley(
                    egui::pos2(rect.left() + tx, rect.center().y - galley.size().y / 2.0),
                    galley,
                    color,
                );
                let r = r.on_hover_text(path.display().to_string());
                if r.clicked() {
                    switch = Some(i);
                }
                if r.middle_clicked() {
                    close = Some(i);
                }
                if r.dragged() {
                    dragging = Some(i);
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                }
                if r.drag_stopped() {
                    dropped = Some(i);
                }
                if n > 1 {
                    let x_rect = egui::Rect::from_center_size(egui::pos2(rect.right() - 13.0, rect.center().y), egui::vec2(16.0, 16.0));
                    let xr = ui.interact(x_rect, id.with("close"), egui::Sense::click());
                    if xr.hovered() {
                        ui.painter().rect_filled(x_rect, 4.0, pal.border);
                    }
                    let show_x = selected || r.hovered() || xr.hovered();
                    let c = if xr.hovered() { pal.strong } else if show_x { pal.weak } else { egui::Color32::TRANSPARENT };
                    let d = 3.5;
                    let m = x_rect.center();
                    let st = egui::Stroke::new(1.3, c);
                    ui.painter().line_segment([m + egui::vec2(-d, -d), m + egui::vec2(d, d)], st);
                    ui.painter().line_segment([m + egui::vec2(-d, d), m + egui::vec2(d, -d)], st);
                    if xr.on_hover_text(format!("Close tab ({})", crate::plat::keys("Ctrl+W"))).clicked() {
                        close = Some(i);
                    }
                }
                rects.push(rect);
            }
        };
        if vertical {
            ui.spacing_mut().item_spacing.y = 2.0;
            body(ui);
        } else {
            ui.spacing_mut().item_spacing.x = 2.0;
            body(ui);
        }

        // Reordering and moving to the other pane happen in `tab_drag`, once every strip is drawn.
        self.tab_layout.rects[p] = rects;
        if let Some(i) = dragging {
            self.tab_layout.dragging = Some((p, i));
        }
        if let Some(i) = dropped {
            self.tab_layout.dropped = Some((p, i));
        }
        if let Some(i) = switch {
            self.switch_tab(p, i);
        }
        if let Some(i) = close {
            self.close_tab(p, i);
        }
    }

    /// Move tab `i` of pane `from` into pane `to` before position `at` (clamped), select it
    /// there and focus that pane. A pane left without tabs closes: the other pane is shown
    /// alone, and the closed side starts over with one tab the next time dual is turned on.
    fn move_tab_across(&mut self, from: usize, i: usize, to: usize, at: usize) {
        if from == to || i >= self.panes[from].tabs.len() {
            return;
        }
        let last = self.panes[from].tabs.len() == 1;
        if !last && i == self.panes[from].selected {
            let next = if i + 1 < self.panes[from].tabs.len() { i + 1 } else { i - 1 };
            self.switch_tab(from, next);
        }
        let group = &mut self.panes[from];
        let mut tab = group.tabs.remove(i);
        if !last && i < group.selected {
            group.selected -= 1;
        }
        tab.pane.scroll_to_cursor = true;
        let group = &mut self.panes[to];
        let at = at.min(group.tabs.len());
        group.tabs.insert(at, tab);
        group.selected = at;
        group.scroll_to_selected = true;
        self.active = to;
        if last {
            let mut keep = to;
            if from == 0 {
                self.panes.swap(0, 1);
                keep = 0;
            }
            let id = self.next_tab_id;
            self.next_tab_id += 1;
            let path = self.panes[keep].pane().path.clone();
            let gallery = self.panes[keep].pane().gallery;
            self.panes[1] = PaneTabs::new(id, path);
            self.panes[1].pane_mut().gallery = gallery;
            self.dual = false;
            self.active = 0;
        }
    }

    /// Where a tab dragged from pane `from` would land if released at `pos`.
    fn tab_target(&self, from: usize, pos: egui::Pos2) -> Option<TabTarget> {
        if !self.dual {
            return None;
        }
        let o = 1 - from;
        let l = &self.tab_layout;
        if l.zones[o].is_some_and(|z| z.expand(4.0).contains(pos)) {
            let vertical = self.tabs_vertical;
            let along = |p: egui::Pos2| if vertical { p.y } else { p.x };
            let at = l.rects[o].iter().position(|r| along(pos) < along(r.center())).unwrap_or(l.rects[o].len());
            return Some(TabTarget::Strip(o, at));
        }
        if l.panes[o].is_some_and(|r| r.contains(pos)) {
            return Some(TabTarget::Pane(o));
        }
        None
    }

    /// After every strip and pane is drawn: reorder a dragged tab within its strip, or show
    /// where it would go in the other pane and move it there when released.
    fn tab_drag(&mut self, ui: &egui::Ui) {
        let layout = std::mem::take(&mut self.tab_layout);
        let ctx = ui.ctx();
        let Some(pos) = ctx.pointer_latest_pos() else { return };
        if let Some((side, i)) = layout.dropped {
            self.tab_layout = layout;
            match self.tab_target(side, pos) {
                Some(TabTarget::Strip(to, at)) => self.move_tab_across(side, i, to, at),
                Some(TabTarget::Pane(to)) => self.move_tab_across(side, i, to, usize::MAX),
                None => {}
            }
            self.tab_layout = TabLayout::default();
            return;
        }
        let Some((side, i)) = layout.dragging else { return };
        self.tab_layout = layout;
        let vertical = self.tabs_vertical;
        let along = |p: egui::Pos2| if vertical { p.y } else { p.x };
        let Some(target) = self.tab_target(side, pos) else {
            // Within its own strip: move the tab once the pointer passes a neighbour's middle.
            if self.tab_layout.zones[side].is_some_and(|z| z.expand(4.0).contains(pos)) {
                let mut to = i;
                for (j, r) in self.tab_layout.rects[side].iter().enumerate() {
                    if (j > i && along(pos) > along(r.center())) || (j < i && along(pos) < along(r.center()) && to == i) {
                        to = j;
                    }
                }
                if to != i && i.max(to) < self.panes[side].tabs.len() {
                    self.move_tab(side, i, to);
                }
            }
            self.tab_layout = TabLayout::default();
            return;
        };
        let pal = theme::pal(ui);
        let fg = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("tab-drag")));
        match target {
            TabTarget::Strip(to, at) => {
                // Insertion marker between tabs.
                let rs = &self.tab_layout.rects[to];
                let zone = self.tab_layout.zones[to].unwrap_or(egui::Rect::NOTHING);
                let (v, span) = match (rs.get(at), at.checked_sub(1).and_then(|k| rs.get(k))) {
                    (Some(r), _) => (if vertical { r.top() - 1.0 } else { r.left() - 1.0 }, *r),
                    (None, Some(r)) => (if vertical { r.bottom() + 1.0 } else { r.right() + 1.0 }, *r),
                    (None, None) => (if vertical { zone.top() + 2.0 } else { zone.left() + 2.0 }, zone),
                };
                let line = if vertical {
                    egui::Rect::from_min_max(egui::pos2(span.left(), v - 1.0), egui::pos2(span.right(), v + 1.0))
                } else {
                    egui::Rect::from_min_max(egui::pos2(v - 1.0, span.top() + 4.0), egui::pos2(v + 1.0, span.bottom() - 4.0))
                };
                fg.rect_filled(line, 1.0, pal.accent);
            }
            TabTarget::Pane(to) => {
                if let Some(r) = self.tab_layout.panes[to] {
                    fg.rect(r.shrink(1.0), 8.0, pal.accent.gamma_multiply(0.08), egui::Stroke::new(2.0, pal.accent), egui::StrokeKind::Inside);
                }
            }
        }
        // A small copy of the tab follows the pointer.
        if let Some(tab) = self.panes[side].tabs.get(i) {
            let path = &tab.pane.path;
            let title = path.file_name().map(crate::text::display_name).unwrap_or_else(|| path.display().to_string());
            let galley = ctx.fonts_mut(|f| f.layout_no_wrap(title, egui::FontId::proportional(13.0), pal.strong));
            let size = egui::vec2(galley.size().x.min(220.0) + 20.0, 26.0);
            let r = egui::Rect::from_min_size(pos + egui::vec2(12.0, 10.0), size);
            fg.rect(r, 6.0, pal.surface, egui::Stroke::new(1.0, pal.accent), egui::StrokeKind::Inside);
            fg.with_clip_rect(r.shrink(4.0)).galley(egui::pos2(r.left() + 10.0, r.center().y - galley.size().y / 2.0), galley, pal.strong);
        }
        self.tab_layout = TabLayout::default();
    }

    fn new_tab_button(&mut self, ui: &mut egui::Ui, p: usize) {
        let pal = theme::pal(ui);
        let (rect, response) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::click());
        if response.hovered() {
            ui.painter().rect_filled(rect, 6.0, pal.surface);
        }
        let stroke = egui::Stroke::new(1.4, if response.hovered() { pal.strong } else { pal.weak });
        let center = rect.center();
        let x = egui::vec2(5.0, 0.0);
        let y = egui::vec2(0.0, 5.0);
        ui.painter().line_segment([center - x, center + x], stroke);
        ui.painter().line_segment([center - y, center + y], stroke);
        if response.on_hover_text(format!("New tab ({})", crate::plat::keys("Ctrl+T"))).clicked() {
            let path = self.panes[p].pane().path.clone();
            self.new_tab(p, path);
        }
    }

    fn horizontal_tabs(&mut self, ui: &mut egui::Ui, p: usize, width: f32) {
        let row = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            egui::ScrollArea::horizontal()
                .id_salt(("tabs-scroll", p))
                .max_width((width - 30.0).max(1.0))
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| ui.horizontal(|ui| self.tab_bar(ui, p, false)));
            self.new_tab_button(ui, p);
        });
        self.tab_layout.zones[p] = Some(row.response.rect);
    }

    fn vertical_tabs(&mut self, ui: &mut egui::Ui, p: usize) {
        self.tab_layout.zones[p] = Some(ui.available_rect_before_wrap());
        self.new_tab_button(ui, p);
        egui::ScrollArea::vertical()
            .id_salt(("vtabs-scroll", p))
            .show(ui, |ui| self.tab_bar(ui, p, true));
    }

    fn pane_ui(&mut self, ui: &mut egui::Ui, p: usize, actions: &mut Vec<Action>) {
        let group = &mut self.panes[p];
        let tab = &mut group.tabs[group.selected];
        let scope = egui::Id::unique(("pane-tab", tab.id));
        ui.scope_builder(egui::UiBuilder::new().scope_id(scope), |ui| {
            tab.pane.ui(ui, p, self.active == p, self.show_hidden, &mut self.thumbs, actions);
        });
    }

    fn split_pane_ui(&mut self, ui: &mut egui::Ui, p: usize, tabs_vertical: bool, actions: &mut Vec<Action>) {
        self.tab_layout.panes[p] = Some(ui.available_rect_before_wrap());
        // The second pane has no listing until it is first displayed or navigated to.
        if self.panes[p].pane().req == 0 {
            self.load(p);
        }
        ui.shrink_clip_rect(ui.available_rect_before_wrap().expand(1.0));
        pane_frame(ui, self.active == p, |ui| {
            let width = ui.available_width();
            if tabs_vertical {
                let pal = theme::pal(ui);
                egui::Panel::left(("pane-tabs", p))
                    .exact_size((width / 3.0).min(160.0))
                    .resizable(false)
                    .frame(egui::Frame::new().fill(pal.panel).inner_margin(4.0))
                    .show(ui, |ui| self.vertical_tabs(ui, p));
            } else {
                self.horizontal_tabs(ui, p, width);
            }
            self.pane_ui(ui, p, actions);
        });
    }

    /// View switches, right-aligned: list/gallery, single/dual pane, tab strip position.
    fn view_icons(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.x = 2.0;
        if icon_button(ui, Icon::TabsSide(self.tabs_vertical), false, if self.tabs_vertical { "Tabs on the left (click for top)" } else { "Tabs on top (click for left)" }) {
            self.tabs_vertical = !self.tabs_vertical;
        }
        ui.add_space(8.0);
        let dual_tip = format!("Dual pane, side by side ({})", crate::plat::keys("Ctrl+\\"));
        if icon_button(ui, Icon::DualStacked, self.dual && self.split_stacked, "Dual pane, top and bottom") {
            self.dual = true;
            self.split_stacked = true;
        }
        if icon_button(ui, Icon::Dual, self.dual && !self.split_stacked, &dual_tip) {
            self.dual = true;
            self.split_stacked = false;
        }
        if icon_button(ui, Icon::Single, !self.dual, "Single pane") {
            self.dual = false;
            self.active = 0;
        }
        ui.add_space(8.0);
        let a = self.active;
        let g = self.panes[a].pane().gallery;
        let gal_tip = format!("Gallery view ({})", crate::plat::keys("Ctrl+G"));
        let list_tip = format!("List view ({})", crate::plat::keys("Ctrl+G"));
        if icon_button(ui, Icon::Gallery, g, &gal_tip) {
            self.set_gallery(true);
        }
        if icon_button(ui, Icon::List, !g, &list_tip) {
            self.set_gallery(false);
        }
    }

    fn set_gallery(&mut self, on: bool) {
        let p = self.panes[self.active].pane_mut();
        p.gallery = on;
        p.scroll_to_cursor = true;
    }

    fn refresh_all(&mut self) {
        for p in 0..2 {
            let pane = self.panes[p].pane();
            if pane.req != 0 && pane.search.is_none() {
                self.load(p);
            }
        }
    }

    fn poll(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Listing { pane, req, path, sorted, result } => {
                    let sh = self.show_hidden;
                    let Some(p) = self.pane_for(pane, req) else { continue };
                    if p.path != path {
                        continue;
                    }
                    p.loading = false;
                    match result {
                        Ok(entries) => {
                            p.error = None;
                            p.set_entries(entries, sorted, sh);
                        }
                        Err(e) => {
                            p.error = Some(e);
                            p.set_entries(Vec::new(), sorted, sh);
                        }
                    }
                }
                Msg::SearchBatch { pane, req, items, scanned } => {
                    let sh = self.show_hidden;
                    let Some(p) = self.pane_for(pane, req) else { continue };
                    if let Some(s) = &mut p.search {
                        s.scanned = scanned;
                    }
                    p.entries.extend(items);
                    p.rebuild_view(sh);
                }
                Msg::SearchDone { pane, req, scanned } => {
                    let sh = self.show_hidden;
                    let Some(p) = self.pane_for(pane, req) else { continue };
                    if let Some(s) = &mut p.search {
                        s.running = false;
                        s.scanned = scanned;
                    }
                    p.resort(sh);
                }
                Msg::PreviewReady { req, result } => {
                    self.preview_running = false;
                    if let Some(Dialog::Preview(preview)) = &mut self.dialog
                        && preview.req == req
                    {
                        preview.finish(&self.ctx, result);
                    }
                    self.start_preview();
                }
                Msg::OpProgress(s) => self.status = s,
                Msg::OpDone { summary, errors } => {
                    self.busy = false;
                    self.status = if errors.is_empty() { summary } else { format!("{summary}, {} error(s)", errors.len()) };
                    if !errors.is_empty() {
                        self.dialog = Some(Dialog::Errors(errors));
                    }
                    self.refresh_all();
                }
            }
        }
    }

    fn start_preview(&mut self) {
        if self.preview_running {
            return;
        }
        let bg = self.bg();
        if let Some(Dialog::Preview(preview)) = &mut self.dialog {
            self.preview_running = preview.start(bg);
        }
    }

    fn start_search(&mut self, p: usize, query: String) {
        let req = self.next_req;
        self.next_req += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        let pane = self.panes[p].pane_mut();
        if let Some(s) = pane.search.take() {
            s.cancel.store(true, Ordering::Relaxed);
        }
        pane.req = req;
        pane.entries.clear();
        pane.view.clear();
        pane.selected.clear();
        pane.cursor = 0;
        pane.filter.clear();
        pane.search = Some(SearchState { query: query.clone(), running: true, scanned: 0, cancel: cancel.clone() });
        let root = pane.path.clone();
        fsx::spawn_search(self.bg(), p, req, root, query, cancel);
    }

    /// Hand the pane's selection to the OS as a file drag (see `dragout.rs`). Once per press:
    /// egui keeps reporting the drag until it sees the button released.
    fn start_drag_out(&mut self, frame: &eframe::Frame, p: usize) {
        if self.drag_out.as_ref().is_some_and(|d| d.released.is_none()) {
            return;
        }
        let paths = self.panes[p].pane().targets();
        if paths.is_empty() {
            return;
        }
        crate::dragout::start(frame, &paths);
        self.drag_out = Some(DragOut { paths, released: None });
    }

    /// After a drag out: when the button is up, reload if the target moved files away, and
    /// forget the drag a little later (it is kept briefly to ignore a drop back onto us).
    fn drag_out_tick(&mut self, ctx: &egui::Context) {
        let Some(d) = &mut self.drag_out else { return };
        match d.released {
            None if !ctx.input(|i| i.pointer.any_down()) => {
                d.released = Some(std::time::Instant::now());
                if d.paths.iter().any(|p| !p.exists()) {
                    for p in 0..if self.dual { 2 } else { 1 } {
                        self.load(p);
                    }
                }
                ctx.request_repaint_after(std::time::Duration::from_secs(3));
            }
            Some(t) if t.elapsed().as_secs() >= 3 => self.drag_out = None,
            _ => {}
        }
    }

    fn apply(&mut self, a: Action) {
        let sh = self.show_hidden;
        match a {
            Action::Activate(p) => self.active = p,
            Action::Navigate(p, path) => self.navigate(p, path, true),
            Action::OpenCursor(p) => {
                let Some(e) = self.panes[p].pane().cursor_entry().cloned() else { return };
                if e.is_dir {
                    self.navigate(p, e.path, true);
                } else if let Err(err) = open::that_detached(&e.path) {
                    self.status = format!("Could not open {}: {err}", e.name);
                }
            }
            Action::Preview(path) => {
                if self.dialog.is_some() {
                    return;
                }
                let req = self.next_req;
                self.next_req += 1;
                self.dialog = Some(Dialog::Preview(Preview::new(req, path)));
                self.start_preview();
                self.ctx.request_repaint();
            }
            Action::Up(p) => {
                let pane = self.panes[p].pane_mut();
                if pane.search.is_some() {
                    self.apply(Action::CloseSearch(p));
                    return;
                }
                if let Some(parent) = pane.path.parent().map(|x| x.to_path_buf()) {
                    pane.pending_select = Some(pane.path.clone());
                    self.navigate(p, parent, true);
                }
            }
            Action::Back(p) => {
                let pane = self.panes[p].pane_mut();
                if let Some(prev) = pane.back.pop() {
                    pane.fwd.push(pane.path.clone());
                    pane.pending_select = Some(pane.path.clone());
                    self.navigate(p, prev, false);
                }
            }
            Action::Forward(p) => {
                let pane = self.panes[p].pane_mut();
                if let Some(next) = pane.fwd.pop() {
                    pane.back.push(pane.path.clone());
                    self.navigate(p, next, false);
                }
            }
            Action::Refresh(p) => {
                self.drives = fsx::drives();
                self.thumbs.invalidate();
                self.load(p)
            }
            Action::Copy(p) | Action::Cut(p) => {
                let cut = matches!(a, Action::Cut(_));
                let t = self.panes[p].pane().targets();
                if !t.is_empty() {
                    self.status = format!("{} {} item(s)", if cut { "Cut" } else { "Copied" }, t.len());
                    // Also put the paths on the system clipboard: handy in other apps, and it
                    // guarantees the next ⌘/Ctrl+V reaches us (egui only reports a paste when
                    // the system clipboard has content).
                    let text: Vec<String> = t.iter().map(|x| x.display().to_string()).collect();
                    self.ctx.copy_text(text.join("\n"));
                    self.clipboard = Some((t, cut));
                }
            }
            Action::Paste(p) => {
                if self.panes[p].pane().search.is_some() {
                    return;
                }
                if let Some((src, cut)) = self.clipboard.clone() {
                    if cut {
                        self.clipboard = None;
                    }
                    self.busy = true;
                    self.status = "Working…".into();
                    fsx::spawn_paste(self.bg(), src, self.panes[p].pane().path.clone(), cut);
                }
            }
            Action::Delete(p, permanent) => {
                let paths = self.panes[p].pane().targets();
                if paths.is_empty() {
                    return;
                }
                if permanent {
                    self.dialog = Some(Dialog::ConfirmDelete { paths });
                } else {
                    self.busy = true;
                    fsx::spawn_delete(self.bg(), paths, false);
                }
            }
            Action::StartRename(p) => {
                let pane = self.panes[p].pane_mut();
                if let Some(e) = pane.cursor_entry() {
                    pane.rename = Some(Rename { path: e.path.clone(), text: e.name.clone(), focus: true });
                }
            }
            Action::CommitRename(p, from, name) => {
                let name = name.trim().to_string();
                let Some(dir) = from.parent() else { return };
                if name.is_empty() || from.file_name().is_some_and(|f| f.to_string_lossy() == name) {
                    return;
                }
                if name.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|']) {
                    self.status = "A name can't contain \\ / : * ? \" < > |".into();
                    return;
                }
                let to = dir.join(&name);
                if to.exists() && !to.to_string_lossy().eq_ignore_ascii_case(&from.to_string_lossy()) {
                    self.status = format!("“{name}” already exists");
                    return;
                }
                match std::fs::rename(&from, &to) {
                    Ok(()) => {
                        self.status = format!("Renamed to {name}");
                        self.panes[p].pane_mut().pending_select = Some(to);
                        self.refresh_all();
                    }
                    Err(e) => self.status = format!("Rename failed: {e}"),
                }
            }
            Action::NewFolder(p) => {
                self.dialog = Some(Dialog::NewFolder { pane: p, name: "新しいフォルダー".into(), focus: true });
            }
            Action::OpenInNewTab(p, path) => self.new_tab(p, path),
            // Needs the window; handled in `ui` (start_drag_out).
            Action::DragOut(_) => {}
            Action::ToggleFavorite(path) => {
                if self.is_favorite(&path) {
                    self.remove_favorite(&path);
                } else {
                    self.add_favorite(path);
                }
            }
            Action::OpenInOther(p) => {
                if let Some(e) = self.panes[p].pane().cursor_entry().cloned() {
                    self.dual = true;
                    let o = 1 - p;
                    self.navigate(o, e.path, true);
                }
            }
            Action::CopyPath(p) => {
                let t = self.panes[p].pane().targets();
                let s: Vec<String> = t.iter().map(|x| x.display().to_string()).collect();
                if !s.is_empty() {
                    self.ctx.copy_text(s.join("\r\n"));
                    self.status = format!("Copied {} path(s)", s.len());
                }
            }
            Action::Reveal(path) => crate::plat::reveal(&path),
            Action::Terminal(path) => crate::plat::terminal(&path),
            Action::CloseSearch(p) => {
                if let Some(s) = self.panes[p].pane_mut().search.take() {
                    s.cancel.store(true, Ordering::Relaxed);
                }
                self.load(p);
            }
        }
        let _ = sh;
    }

    fn global_keys(&mut self, ctx: &egui::Context, out: &mut Vec<Action>) {
        if self.dialog.is_some() {
            if matches!(self.dialog, Some(Dialog::Preview(_))) && ctx.input_mut(|i| {
                preview::consume_shortcut(i) || i.consume_key(egui::Modifiers::NONE, Key::Escape)
            }) {
                self.dialog = None;
            }
            return;
        }
        let typing = ctx.egui_wants_keyboard_input();
        // Tabs: ⌘/Ctrl+T new, ⌘/Ctrl+W close, Ctrl+Tab / Ctrl+Shift+Tab cycle, ⌘/Ctrl+1…9 jump.
        let (t_new, t_close, t_next, t_prev, t_jump) = ctx.input_mut(|i| {
            let ctrl_tab = egui::Modifiers { ctrl: true, ..Default::default() };
            let ctrl_shift_tab = egui::Modifiers { ctrl: true, shift: true, ..Default::default() };
            let mut next = i.consume_key(ctrl_tab, Key::Tab);
            let mut prev = i.consume_key(ctrl_shift_tab, Key::Tab);
            if cfg!(target_os = "macos") {
                // Safari/Finder style: ⇧⌘] / ⇧⌘[
                next |= i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, Key::CloseBracket);
                prev |= i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, Key::OpenBracket);
            }
            let digits = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
            let jump = digits.iter().position(|k| i.consume_key(egui::Modifiers::COMMAND, *k));
            (
                i.consume_key(egui::Modifiers::COMMAND, Key::T),
                i.consume_key(egui::Modifiers::COMMAND, Key::W),
                next,
                prev,
                jump,
            )
        });
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::G)) {
            let g = self.panes[self.active].pane().gallery;
            self.set_gallery(!g);
        }
        // ⌘/Ctrl+D: add the selected folders to Favorites, or this folder when none is selected.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::D)) {
            let pane = self.panes[self.active].pane();
            let mut picks: Vec<PathBuf> =
                pane.selected.iter().filter_map(|&ei| pane.entries.get(ei)).filter(|e| e.is_dir).map(|e| e.path.clone()).collect();
            picks.sort();
            if picks.is_empty() {
                picks.push(pane.path.clone());
            }
            for p in picks {
                self.add_favorite(p);
            }
        }
        let p = self.active;
        if t_new {
            let path = self.panes[p].pane().path.clone();
            self.new_tab(p, path);
        }
        if t_close {
            self.close_tab(p, self.panes[p].selected);
        }
        let n = self.panes[p].tabs.len();
        if t_next {
            self.switch_tab(p, (self.panes[p].selected + 1) % n);
        }
        if t_prev {
            self.switch_tab(p, (self.panes[p].selected + n - 1) % n);
        }
        if let Some(j) = t_jump {
            // ⌘9 is always the last tab, as in browsers.
            self.switch_tab(p, if j == 8 { n - 1 } else { j });
        }

        let (tab, toggle_dual, hidden, palette, search, help) = ctx.input_mut(|i| {
            (
                !typing && i.consume_key(egui::Modifiers::NONE, Key::Tab),
                i.consume_key(egui::Modifiers::COMMAND, Key::Backslash),
                i.consume_key(egui::Modifiers::COMMAND, Key::H),
                i.consume_key(egui::Modifiers::COMMAND, Key::P),
                i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, Key::F),
                i.consume_key(egui::Modifiers::NONE, Key::F1),
            )
        });
        // Finder-style aliases: ⌘⇧. toggles hidden files, ⌘⇧G opens "Go to folder".
        // (⌘H is the system "Hide app" shortcut on macOS, so it can't be ours there.)
        let (hidden, palette) = if cfg!(target_os = "macos") {
            ctx.input(|i| {
                let ms = i.modifiers.command && i.modifiers.shift;
                let hit = |want: Key| {
                    i.events.iter().any(|e| {
                        matches!(e, egui::Event::Key { key, physical_key, pressed: true, .. }
                            if ms && (*key == want || *physical_key == Some(want)))
                    })
                };
                (hidden || hit(Key::Period), palette || hit(Key::G))
            })
        } else {
            (hidden, palette)
        };
        if tab && self.dual {
            self.active = 1 - self.active;
        }
        if toggle_dual {
            self.dual = !self.dual;
            if !self.dual {
                self.active = 0;
            }
        }
        if hidden {
            self.show_hidden = !self.show_hidden;
            let sh = self.show_hidden;
            for group in &mut self.panes {
                group.pane_mut().rebuild_view(sh);
            }
            self.status = if sh { "Showing hidden files".into() } else { "Hiding hidden files".into() };
        }
        if palette {
            self.dialog = Some(Dialog::Palette { text: String::new(), sel: 0, focus: true });
        }
        if search {
            self.dialog = Some(Dialog::Search { pane: self.active, query: String::new(), focus: true });
        }
        if help {
            self.dialog = Some(Dialog::Help);
        }
        if !typing && self.dialog.is_none() {
            let a = self.active;
            let sh = self.show_hidden;
            let pane = self.panes[a].pane_mut();
            let had_filter = !pane.filter.is_empty();
            pane.keys(ctx, a, self.type_to_filter, out);
            if had_filter && pane.filter.is_empty() {
                pane.rebuild_view(sh);
            }
        }

        // Files dropped from Explorer/Finder are copied into the active folder. Our own drag
        // dropped back onto the window is ignored (it would duplicate the files in place).
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        let own = self.drag_out.as_ref().is_some_and(|d| dropped.iter().all(|p| d.paths.contains(p)));
        if !dropped.is_empty() && !own {
            self.busy = true;
            fsx::spawn_paste(self.bg(), dropped, self.panes[self.active].pane().path.clone(), false);
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui, out: &mut Vec<Action>) {
        let mut add_current = false;
        let mut remove: Option<PathBuf> = None;
        let mut reorder: Option<(usize, usize)> = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            let a = self.active;
            let cur = self.panes[a].pane().path.clone();
            let add_tip = format!("Add this folder to Favorites ({})", crate::plat::keys("Ctrl+D"));
            add_current = section_with_add(ui, "FAVORITES", &add_tip);
            // Click to go, drag to reorder, right-click to remove.
            let mut rects: Vec<egui::Rect> = Vec::new();
            let mut dragging: Option<usize> = None;
            for (i, (label, path)) in self.quick.iter().enumerate() {
                let id = egui::Id::unique(("favorite", path));
                let r = side_row(ui, place_glyph(label, path), label, &cur == path, Some(id));
                rects.push(r.rect);
                if r.clicked() {
                    out.push(Action::Navigate(a, path.clone()));
                }
                if r.middle_clicked() {
                    out.push(Action::OpenInNewTab(a, path.clone()));
                }
                if r.dragged() {
                    dragging = Some(i);
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                }
                r.context_menu(|ui| {
                    if ui.button("Open in new tab").clicked() {
                        out.push(Action::OpenInNewTab(a, path.clone()));
                    }
                    if ui.button("Remove from Favorites").clicked() {
                        remove = Some(path.clone());
                    }
                });
                if dragging.is_none() {
                    r.on_hover_text(path.display().to_string());
                }
            }
            // Move the dragged favourite once the pointer passes a neighbour's middle.
            if let (Some(i), Some(pos)) = (dragging, ui.ctx().pointer_interact_pos()) {
                let mut to = i;
                for (j, r) in rects.iter().enumerate() {
                    if (j > i && pos.y > r.center().y) || (j < i && pos.y < r.center().y && to == i) {
                        to = j;
                    }
                }
                if to != i {
                    reorder = Some((i, to));
                }
            }
            section(ui, if cfg!(windows) { "DRIVES" } else { "VOLUMES" });
            for d in &self.drives {
                let label = match d.file_name() {
                    Some(n) => crate::text::display_name(n),
                    None => d.display().to_string(),
                };
                if side_item(ui, Glyph::Drive, &label, &cur == d, &d.display().to_string()) {
                    out.push(Action::Navigate(a, d.clone()));
                }
            }
            if !self.recent.is_empty() {
                section(ui, "RECENT");
                for r in self.recent.iter().take(8) {
                    let name = r.file_name().map(crate::text::display_name).unwrap_or_else(|| r.display().to_string());
                    if side_item(ui, Glyph::Clock, &name, false, &r.display().to_string()) {
                        out.push(Action::Navigate(a, r.clone()));
                    }
                }
            }
        });
        if add_current {
            let p = self.panes[self.active].pane().path.clone();
            self.add_favorite(p);
        }
        if let Some(p) = remove {
            self.remove_favorite(&p);
        }
        if let Some((from, to)) = reorder {
            let f = self.quick.remove(from);
            self.quick.insert(to, f);
            self.fav_dirty = true;
        }
        if self.fav_dirty && !ui.input(|i| i.pointer.any_down()) {
            self.fav_dirty = false;
            self.favorites_changed();
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.dialog.take() else { return };
        let preview = matches!(d, Dialog::Preview(_));
        let mut keep = true;
        let mut after: Option<Box<dyn FnOnce(&mut App)>> = None;
        let modal = egui::Modal::new(egui::Id::unique("dialog")).show(ctx, |ui| {
            if !preview {
                ui.set_width(460.0);
            }
            match &mut d {
                Dialog::NewFolder { pane, name, focus } => {
                    ui.heading("New folder");
                    let r = ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY));
                    if *focus {
                        r.request_focus();
                        *focus = false;
                    }
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    ui.horizontal(|ui| {
                        if ui.button("Create").clicked() || enter {
                            let (p, n) = (*pane, name.trim().to_string());
                            after = Some(Box::new(move |app: &mut App| app.create_folder(p, n)));
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::ConfirmDelete { paths, .. } => {
                    ui.heading("Delete permanently?");
                    ui.label(format!("{} item(s) will be deleted without going to the {}.", paths.len(), crate::plat::TRASH));
                    for p in paths.iter().take(8) {
                        ui.monospace(p.display().to_string());
                    }
                    if paths.len() > 8 {
                        ui.weak(format!("…and {} more", paths.len() - 8));
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Delete").clicked() {
                            let ps = paths.clone();
                            after = Some(Box::new(move |app: &mut App| {
                                app.busy = true;
                                fsx::spawn_delete(app.bg(), ps, true);
                            }));
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::Search { pane, query, focus } => {
                    ui.heading("Search in subfolders");
                    ui.weak(self.panes[*pane].pane().path.display().to_string());
                    ui.weak("Matches ignore case, full/half-width and hiragana/katakana. Spaces separate terms.");
                    let r = ui.add(egui::TextEdit::singleline(query).desired_width(f32::INFINITY).hint_text("例: 請求書 2024"));
                    if *focus {
                        r.request_focus();
                        *focus = false;
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && !query.trim().is_empty() {
                        let (p, q) = (*pane, query.trim().to_string());
                        after = Some(Box::new(move |app: &mut App| app.start_search(p, q)));
                        keep = false;
                    }
                }
                Dialog::Palette { text, sel, focus } => {
                    ui.heading("Go to folder");
                    let r = ui.add(egui::TextEdit::singleline(text).desired_width(f32::INFINITY).hint_text("Type a path, or part of a recent/quick-access folder name"));
                    if *focus {
                        r.request_focus();
                        *focus = false;
                    }
                    let q = fold(text);
                    let mut items: Vec<(i32, PathBuf)> = self
                        .quick
                        .iter()
                        .map(|(_, p)| p.clone())
                        .chain(self.recent.iter().cloned())
                        .chain(self.drives.iter().cloned())
                        .filter_map(|p| fuzzy_score(&fold(&p.to_string_lossy()), &q).map(|s| (s, p)))
                        .collect();
                    items.sort_by(|a, b| b.0.cmp(&a.0));
                    items.dedup_by(|a, b| a.1 == b.1);
                    let typed = PathBuf::from(text.trim().trim_matches('"'));
                    if !text.trim().is_empty() && typed.is_dir() {
                        items.insert(0, (i32::MAX, typed));
                    }
                    items.truncate(12);
                    let (down, up) = ui.input_mut(|i| (i.consume_key(egui::Modifiers::NONE, Key::ArrowDown), i.consume_key(egui::Modifiers::NONE, Key::ArrowUp)));
                    if down {
                        *sel = (*sel + 1).min(items.len().saturating_sub(1));
                    }
                    if up {
                        *sel = sel.saturating_sub(1);
                    }
                    if r.changed() {
                        *sel = 0;
                    }
                    let mut pick = None;
                    for (k, (_, p)) in items.iter().enumerate() {
                        if ui.selectable_label(k == *sel, p.display().to_string()).clicked() {
                            pick = Some(p.clone());
                        }
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        pick = items.get(*sel).map(|x| x.1.clone());
                        if pick.is_none() {
                            keep = false;
                        }
                    }
                    if let Some(p) = pick {
                        let a = self.active;
                        after = Some(Box::new(move |app: &mut App| app.navigate(a, p, true)));
                        keep = false;
                    }
                }
                Dialog::Errors(errs) => {
                    ui.heading("Some items failed");
                    egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                        for e in errs.iter() {
                            ui.label(e);
                        }
                    });
                    if ui.button("OK").clicked() {
                        keep = false;
                    }
                }
                Dialog::Preview(preview) => keep = !preview.show(ui),
                Dialog::Help => {
                    #[cfg(windows)]
                    {
                        if ui.button("Default folder app…").clicked() {
                            keep = false;
                            after = Some(Box::new(|app| {
                                app.dialog = Some(Dialog::FolderApp { status: folder_app::status(), error: None });
                            }));
                        }
                        ui.separator();
                    }
                    ui.heading("Keyboard");
                    egui::Grid::new("help").striped(true).show(ui, |ui| {
                        for (k, v) in crate::plat::help() {
                            ui.monospace(k);
                            ui.label(v);
                            ui.end_row();
                        }
                    });
                    if ui.button("Close").clicked() {
                        keep = false;
                    }
                }
                #[cfg(windows)]
                Dialog::FolderApp { status, error } => {
                    ui.heading("Default folder app");
                    ui.label("Use this copy of 8823 to open regular folders and drives for your Windows account.");
                    ui.weak("Keep 8823.exe at this location while it is registered.");
                    match status {
                        Ok(state) if state.selected == 2 => { ui.label("8823 is registered for folders and drives."); }
                        Ok(state) if state.selected == 1 => { ui.label("8823 is registered for either folders or drives."); }
                        Ok(_) => {}
                        Err(err) => { ui.colored_label(ui.visuals().error_fg_color, format!("Could not read folder settings. {err}")); }
                    }
                    if let Some(err) = error {
                        ui.colored_label(ui.visuals().error_fg_color, err.as_str());
                    }
                    let can_restore = status.as_ref().map_or(true, |state| state.can_restore);
                    let mut change = None;
                    ui.horizontal(|ui| {
                        if ui.button("Set 8823 as default").clicked() {
                            change = Some(true);
                        }
                        if ui.add_enabled(can_restore, egui::Button::new("Restore previous setting")).clicked() {
                            change = Some(false);
                        }
                        if ui.button("Close").clicked() {
                            keep = false;
                        }
                    });
                    if let Some(enabled) = change {
                        *error = folder_app::set_default(enabled).err()
                            .map(|err| format!("Could not change folder settings. {err}"));
                        *status = folder_app::status();
                    }
                }
            }
        });
        if modal.should_close() {
            keep = false;
        }
        if keep {
            self.dialog = Some(d);
        }
        if let Some(f) = after {
            f(self);
        }
    }

    fn create_folder(&mut self, p: usize, name: String) {
        if name.is_empty() {
            return;
        }
        let dir = self.panes[p].pane().path.clone();
        let target = fsx::unique_target(&dir, &name);
        match std::fs::create_dir(&target) {
            Ok(()) => {
                self.panes[p].pane_mut().pending_select = Some(target);
                self.load(p);
            }
            Err(e) => self.status = format!("Could not create folder: {e}"),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.pacer.frame_start();
        let ctx = ui.ctx().clone();
        let zoom = ctx.zoom_factor();
        if zoom != self.ui_zoom {
            self.ui_zoom = zoom;
            if let Err(err) = crate::config::save_ui_zoom(zoom) {
                self.status = format!("Could not save display scale: {err}");
            }
        }
        self.poll();
        self.thumbs.poll(&ctx);
        let mut actions = Vec::new();
        self.global_keys(&ctx, &mut actions);
        let (dual, stacked, tabs_vertical) = (self.dual, self.split_stacked, self.tabs_vertical);

        let pal = theme::pal(ui);
        let side_frame = egui::Frame::new().fill(pal.panel).inner_margin(egui::Margin { left: 8, right: 8, top: 4, bottom: 8 });
        egui::Panel::bottom("status")
            .frame(egui::Frame::new().fill(pal.panel).inner_margin(egui::Margin::symmetric(12, 5)))
            .show_separator_line(false)
            .show(ui, |ui| {
            ui.style_mut().override_font_id = Some(egui::FontId::proportional(12.0));
            ui.visuals_mut().override_text_color = Some(pal.weak);
            ui.horizontal(|ui| {
                let p = self.panes[self.active].pane();
                let sel_size: u64 = p.selected.iter().map(|&i| p.entries[i].size).sum();
                ui.label(format!("{} items", p.view.len()));
                if !p.selected.is_empty() {
                    ui.label(format!("·  {} selected ({})", p.selected.len(), fsx::human_size(sel_size)));
                }
                if p.loading {
                    ui.spinner();
                }
                if let Some((c, cut)) = &self.clipboard {
                    ui.weak(format!("· clipboard: {} {}", c.len(), if *cut { "to move" } else { "to copy" }));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("?").on_hover_text("Keyboard shortcuts (F1)").clicked() {
                        self.dialog = Some(Dialog::Help);
                    }
                    ui.menu_button(format!("{:.0}%", zoom * 100.0), egui::gui_zoom::zoom_menu_buttons)
                        .response
                        .on_hover_text("Display scale");
                    ui.checkbox(&mut self.type_to_filter, "Type to filter")
                        .on_hover_text("Start typing in the file list to filter it, without pressing Ctrl+F first");
                    if self.busy {
                        ui.spinner();
                    }
                    ui.weak(&self.status);
                });
            });
        });

        egui::Panel::left("sidebar")
            .default_size(200.0)
            .frame(side_frame)
            .show_separator_line(false)
            .show(ui, |ui| self.sidebar(ui, &mut actions));

        if tabs_vertical && !dual {
            egui::Panel::left("vtabs").default_size(180.0).frame(side_frame).show_separator_line(false).show(ui, |ui| {
                section(ui, "TABS");
                self.vertical_tabs(ui, 0);
            });
        }

        let central = egui::Frame::new().fill(pal.bg).inner_margin(egui::Margin { left: 10, right: 10, top: 8, bottom: 4 });
        egui::CentralPanel::default().frame(central).show(ui, |ui| {
            ui.horizontal(|ui| {
                let icons_w = 210.0;
                if !dual && !tabs_vertical {
                    let w = (ui.available_width() - icons_w).max(80.0);
                    self.horizontal_tabs(ui, 0, w);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| self.view_icons(ui));
            });
            ui.add_space(4.0);
            if dual && stacked {
                let avail = ui.available_rect_before_wrap();
                let gap = 6.0;
                let h = ((avail.height() - gap) / 2.0).max(60.0);
                for i in 0..2 {
                    let top = avail.top() + i as f32 * (h + gap);
                    let rect = egui::Rect::from_min_size(egui::pos2(avail.left(), top), egui::vec2(avail.width(), h));
                    ui.scope_builder(egui::UiBuilder::new().max_rect(rect).id_salt(("stacked", i)), |ui| {
                        self.split_pane_ui(ui, i, tabs_vertical, &mut actions);
                    });
                }
            } else if dual {
                ui.columns(2, |cols| {
                    for (i, col) in cols.iter_mut().enumerate() {
                        self.split_pane_ui(col, i, tabs_vertical, &mut actions);
                    }
                });
            } else {
                self.pane_ui(ui, 0, &mut actions);
            }
        });
        self.tab_drag(ui);
        if (dual, stacked, tabs_vertical) != (self.dual, self.split_stacked, self.tabs_vertical) {
            for group in &mut self.panes {
                group.scroll_to_selected = true;
            }
            ctx.request_repaint();
        }

        self.dialogs(&ctx);
        for a in actions {
            match a {
                Action::DragOut(p) => self.start_drag_out(frame, p),
                a => self.apply(a),
            }
        }
        if self.drag_out.is_some() {
            self.drag_out_tick(&ctx);
        }
        self.pacer.frame_end();
    }
}

fn pane_frame(ui: &mut egui::Ui, active: bool, add: impl FnOnce(&mut egui::Ui)) {
    let pal = theme::pal(ui);
    let stroke = if active { pal.accent.gamma_multiply(0.7) } else { pal.border };
    egui::Frame::new().stroke(egui::Stroke::new(1.0, stroke)).inner_margin(6.0).corner_radius(8.0).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        add(ui)
    });
}

#[derive(Clone, Copy)]
enum Icon {
    List,
    Gallery,
    Single,
    Dual,
    DualStacked,
    /// Tab strip position; `true` = currently on the left.
    TabsSide(bool),
}

/// A small painted toolbar button. Returns true when clicked.
fn icon_button(ui: &mut egui::Ui, icon: Icon, selected: bool, tip: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(28.0, 26.0), egui::Sense::click());
    let pal = theme::pal(ui);
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 6.0, pal.surface);
    } else if resp.hovered() {
        p.rect_filled(rect, 6.0, pal.hover);
    }
    let c = if selected { pal.accent } else if resp.hovered() { pal.strong } else { pal.weak };
    let st = egui::Stroke::new(1.3, c);
    let r = egui::Rect::from_center_size(rect.center(), egui::vec2(16.0, 12.0));
    match icon {
        Icon::List => {
            for k in 0..3 {
                let y = r.top() + 1.5 + k as f32 * 4.5;
                p.line_segment([egui::pos2(r.left(), y), egui::pos2(r.right(), y)], st);
            }
        }
        Icon::Gallery => {
            for (ix, iy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let min = egui::pos2(r.left() + 1.0 + ix as f32 * 8.0, r.top() + iy as f32 * 6.5);
                p.rect_filled(egui::Rect::from_min_size(min, egui::vec2(6.0, 5.0)), 1.0, c);
            }
        }
        Icon::Single => {
            p.rect_stroke(r, 2.0, st, egui::StrokeKind::Inside);
        }
        Icon::Dual => {
            p.rect_stroke(r, 2.0, st, egui::StrokeKind::Inside);
            p.line_segment([egui::pos2(r.center().x, r.top()), egui::pos2(r.center().x, r.bottom())], st);
        }
        Icon::DualStacked => {
            p.rect_stroke(r, 2.0, st, egui::StrokeKind::Inside);
            p.line_segment([egui::pos2(r.left(), r.center().y), egui::pos2(r.right(), r.center().y)], st);
        }
        Icon::TabsSide(left) => {
            p.rect_stroke(r, 2.0, st, egui::StrokeKind::Inside);
            let strip = if left {
                egui::Rect::from_min_max(r.min, egui::pos2(r.left() + 5.0, r.bottom()))
            } else {
                egui::Rect::from_min_max(r.min, egui::pos2(r.right(), r.top() + 4.0))
            };
            p.rect_filled(strip, 2.0, c);
        }
    }
    resp.on_hover_text(tip).clicked()
}

fn section(ui: &mut egui::Ui, title: &str) -> egui::Rect {
    ui.add_space(12.0);
    let pal = theme::pal(ui);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), egui::Sense::hover());
    ui.painter().text(
        egui::pos2(rect.left() + 10.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(10.5),
        pal.weak.gamma_multiply(0.85),
    );
    rect
}

/// A section title with a small "+" at its right edge. Returns true when the "+" is clicked.
fn section_with_add(ui: &mut egui::Ui, title: &str, tip: &str) -> bool {
    let rect = section(ui, title);
    let pal = theme::pal(ui);
    let pr = egui::Rect::from_center_size(egui::pos2(rect.right() - 16.0, rect.center().y), egui::vec2(20.0, 18.0));
    let resp = ui.interact(pr, ui.scope_id().with(("section-add", title)), egui::Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(pr, 4.0, pal.hover);
    }
    let st = egui::Stroke::new(1.3, if resp.hovered() { pal.strong } else { pal.weak });
    let m = pr.center();
    ui.painter().line_segment([m - egui::vec2(4.0, 0.0), m + egui::vec2(4.0, 0.0)], st);
    ui.painter().line_segment([m - egui::vec2(0.0, 4.0), m + egui::vec2(0.0, 4.0)], st);
    resp.on_hover_text(tip).clicked()
}

/// The sidebar favourites: what the user saved, or the standard folders until they change something.
fn initial_favorites() -> Vec<(String, PathBuf)> {
    let folders = fsx::standard_folders();
    let standard = fsx::quick_access(&folders);
    match crate::config::load_favorites() {
        Some(paths) => paths
            .into_iter()
            .map(|p| {
                let p = fix_redirected(p, &folders);
                (favorite_name(&p, &standard), p)
            })
            .collect(),
        None => standard,
    }
}

/// Favourites saved by older versions point at `home/Desktop` etc. even when Windows has
/// moved that folder (often into OneDrive); point them at the real folder instead.
fn fix_redirected(p: PathBuf, folders: &[(&str, PathBuf, PathBuf)]) -> PathBuf {
    for (_, real, legacy) in folders {
        if p == *legacy && real != legacy && real.is_dir() {
            return real.clone();
        }
    }
    p
}

/// Standard folders keep their names ("Home", "Desktop"…); any other folder shows its own name.
fn favorite_name(path: &Path, standard: &[(String, PathBuf)]) -> String {
    if let Some((label, _)) = standard.iter().find(|(_, p)| p == path) {
        return label.clone();
    }
    match path.file_name() {
        Some(n) => crate::text::display_name(n),
        None => path.display().to_string(),
    }
}

fn place_glyph(label: &str, path: &std::path::Path) -> Glyph {
    match label {
        "Home" => Glyph::Home,
        "Desktop" => Glyph::Desktop,
        "Documents" => Glyph::Doc,
        "Downloads" => Glyph::Download,
        "Pictures" => Glyph::Picture,
        "Music" => Glyph::Music,
        "Videos" | "Movies" => Glyph::Video,
        "Applications" => Glyph::Apps,
        "OneDrive" | "iCloud Drive" => Glyph::Cloud,
        _ if path.to_string_lossy().contains("CloudStorage") => Glyph::Cloud,
        _ => Glyph::Folder,
    }
}

/// One sidebar row: glyph + label, with a pill highlight when it is the current folder.
fn side_item(ui: &mut egui::Ui, g: Glyph, label: &str, selected: bool, tip: &str) -> bool {
    side_row(ui, g, label, selected, None).on_hover_text(tip).clicked()
}

/// The row itself. With a `drag_id` the row also senses drags, under an id that stays put
/// when rows are reordered.
fn side_row(ui: &mut egui::Ui, g: Glyph, label: &str, selected: bool, drag_id: Option<egui::Id>) -> egui::Response {
    let pal = theme::pal(ui);
    let size = egui::vec2(ui.available_width(), 24.0);
    let (rect, resp) = match drag_id {
        Some(id) => {
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            (rect, ui.interact(rect, id, egui::Sense::click_and_drag()))
        }
        None => ui.allocate_exact_size(size, egui::Sense::click()),
    };
    let p = ui.painter_at(rect);
    if selected {
        p.rect_filled(rect, 6.0, pal.surface);
    } else if resp.hovered() || resp.dragged() {
        p.rect_filled(rect, 6.0, pal.hover);
    }
    let icon_c = if selected { pal.accent } else { pal.weak };
    let text_c = if selected { pal.strong } else { pal.text };
    theme::glyph(&p, g, egui::pos2(rect.left() + 18.0, rect.center().y), icon_c);
    let galley = theme::elided(ui, label, egui::FontId::proportional(12.0), text_c, rect.width() - 44.0);
    theme::paint_left(&p, galley, egui::pos2(rect.left() + 36.0, rect.center().y), text_c);
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::App as _;

    #[test]
    fn tab_shortcuts_keep_the_other_pane_in_place() {
        let ctx = egui::Context::default();
        let mut app = App::new(&eframe::CreationContext::_new_kittest(ctx.clone()));
        app.dual = true;
        app.active = 1;
        app.panes[0].pane_mut().path = PathBuf::from("leftFolder");
        app.panes[1].pane_mut().path = PathBuf::from("rightFolder");
        app.panes[1].pane_mut().filter = "filter on the original tab".into();

        let mut press = |key| {
            let modifiers = egui::Modifiers::COMMAND;
            let input = egui::RawInput {
                events: vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }],
                ..Default::default()
            };
            ctx.run_ui(input, |ui| app.global_keys(ui.ctx(), &mut Vec::new())).textures_delta.clear();
            (
                app.dual,
                app.active,
                app.panes[0].pane().path.clone(),
                app.panes[1].pane().path.clone(),
                app.panes[1].pane().filter.clone(),
            )
        };
        assert_eq!(press(Key::T), (true, 1, "leftFolder".into(), "rightFolder".into(), String::new()));
        assert_eq!(press(Key::W), (true, 1, "leftFolder".into(), "rightFolder".into(), "filter on the original tab".into()));
    }

    #[test]
    fn both_panes_show_tab_strips_in_each_split_orientation() {
        let ctx = egui::Context::default();
        let mut app = App::new(&eframe::CreationContext::_new_kittest(ctx.clone()));
        let mut frame = eframe::Frame::_new_kittest();
        app.dual = true;

        for (stacked, tabs_vertical) in [(false, false), (true, false), (false, true), (true, true)] {
            app.split_stacked = stacked;
            app.tabs_vertical = tabs_vertical;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100.0, 700.0))),
                ..Default::default()
            };
            ctx.run_ui(input, |ui| {
                app.ui(ui, &mut frame);
                let tab_rect = |p: usize| {
                    let group = &app.panes[p];
                    let id = egui::Id::unique(("tab", group.tabs[group.selected].id));
                    ctx.read_response(id).expect("pane's tab").rect
                };
                let first = tab_rect(0);
                let second = tab_rect(1);
                if stacked {
                    assert!(first.bottom() < second.top(), "vertical tabs: {tabs_vertical}, {first:?}, {second:?}");
                } else {
                    assert!(first.right() < second.left(), "vertical tabs: {tabs_vertical}, {first:?}, {second:?}");
                }
            }).drop_without_applying_deltas();
        }
    }

    #[test]
    fn changing_split_layout_preserves_the_list_scroll_position() {
        let ctx = egui::Context::default();
        let mut app = App::new(&eframe::CreationContext::_new_kittest(ctx.clone()));
        let mut frame = eframe::Frame::_new_kittest();
        app.dual = true;
        let pane = app.panes[0].pane_mut();
        pane.path = PathBuf::from("scrollPositionTest");
        let entries = (0..160).map(|i| {
            let name = format!("file{i:03}");
            fsx::Entry::new(pane.path.join(&name), name.into(), None, None)
        }).collect();
        pane.set_entries(entries, (fsx::SortKey::Name, true), false);
        pane.move_cursor(80, false);

        let mut display = |app: &mut App| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100.0, 700.0))),
                ..Default::default()
            };
            ctx.run_ui(input, |ui| app.ui(ui, &mut frame)).drop_without_applying_deltas();
            app.panes[0].pane().scroll_y
        };

        let before = display(&mut app);
        assert!(before > 0.0, "initial list did not scroll");
        app.split_stacked = true;
        assert_eq!(display(&mut app), before);
        app.dual = false;
        assert_eq!(display(&mut app), before);
    }

    #[test]
    fn ctrl_i_previews_the_current_file() {
        let ctx = egui::Context::default();
        let mut app = App::new(&eframe::CreationContext::_new_kittest(ctx.clone()));
        let mut frame = eframe::Frame::_new_kittest();
        let pane = app.panes[0].pane_mut();
        pane.path = PathBuf::from("previewTest");
        let path = pane.path.join("previewTest.txt");
        let entry = fsx::Entry::new(path, "previewTest.txt".into(), None, None);
        let mut folder = fsx::Entry::new(pane.path.join("first"), "first".into(), None, None);
        folder.is_dir = true;
        pane.set_entries(vec![folder, entry], (fsx::SortKey::Name, true), false);

        let modifiers = egui::Modifiers { ctrl: true, command: !cfg!(target_os = "macos"), ..Default::default() };
        let key = |key, pressed, modifiers| egui::Event::Key {
            key, physical_key: None, pressed, repeat: false, modifiers,
        };
        let shortcut = |pressed| key(Key::I, pressed, modifiers);
        let mut send = |app: &mut App, events| {
            let input = egui::RawInput { events, ..Default::default() };
            ctx.run_ui(input, |ui| app.ui(ui, &mut frame)).drop_without_applying_deltas();
        };

        assert!(!app.preview_running);
        send(&mut app, vec![key(Key::ArrowDown, true, egui::Modifiers::NONE), shortcut(true)]);
        assert!(matches!(app.dialog, Some(Dialog::Preview(_))));
        send(&mut app, vec![shortcut(true)]);
        assert!(matches!(app.dialog, Some(Dialog::Preview(_))), "a held key should not close the preview");
        send(&mut app, vec![shortcut(false)]);
        send(&mut app, vec![shortcut(true)]);
        assert!(app.dialog.is_none());

        send(&mut app, vec![shortcut(false), egui::Event::Text("preview".into())]);
        assert!(ctx.egui_wants_keyboard_input(), "filter should have keyboard focus");
        assert_eq!(app.panes[0].pane().view.len(), 1);
        send(&mut app, vec![shortcut(true)]);
        assert!(matches!(app.dialog, Some(Dialog::Preview(_))));
        send(&mut app, vec![key(Key::Escape, true, egui::Modifiers::NONE)]);
        assert!(app.dialog.is_none());
        assert_eq!(app.panes[0].pane().filter, "preview");
    }

    #[test]
    fn stale_preview_does_not_upload_a_texture() {
        let ctx = egui::Context::default();
        let mut app = App::new(&eframe::CreationContext::_new_kittest(ctx.clone()));
        let stale_req = app.next_req;
        app.next_req += 1;
        app.preview_running = true;
        app.apply(Action::Preview(PathBuf::from("newPreview.txt")));
        let num_textures = ctx.tex_manager().read().num_allocated();
        app.tx.send(Msg::PreviewReady {
            req: stale_req,
            result: Ok(preview::Content::Raster {
                image: egui::ColorImage::filled([1, 1], egui::Color32::BLACK),
                pdf: false,
            }),
        }).unwrap();
        app.poll();
        assert_eq!(ctx.tex_manager().read().num_allocated(), num_textures);
        assert!(matches!(app.dialog, Some(Dialog::Preview(ref preview)) if preview.req == stale_req + 1));
    }

    #[test]
    fn the_second_pane_defers_its_listing_until_shown() {
        let ctx = egui::Context::default();
        let mut app = App::new(&eframe::CreationContext::_new_kittest(ctx.clone()));
        let mut frame = eframe::Frame::_new_kittest();
        let mut draw = |app: &mut App| {
            ctx.run_ui(egui::RawInput::default(), |ui| app.ui(ui, &mut frame))
                .drop_without_applying_deltas();
        };

        app.refresh_all();
        draw(&mut app);
        assert_eq!(app.panes[1].pane().req, 0);
        app.dual = true;
        draw(&mut app);
        let req = app.panes[1].pane().req;
        assert_ne!(req, 0);
        draw(&mut app);
        assert_eq!(app.panes[1].pane().req, req);
    }
}
