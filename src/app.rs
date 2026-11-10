use crate::fsx::{self, Bg, Msg};
use crate::pane::{Action, Pane, Rename, SearchState};
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
    Help,
}

/// A tab: one or two panes. The active tab's state lives directly in `App`
/// (`panes`, `active`, `dual`); the others are parked in `App::tabs`.
struct Tab {
    /// Stable identity, so a tab can be dragged while its position changes.
    id: u64,
    panes: [Pane; 2],
    active: usize,
    dual: bool,
}

impl Tab {
    fn new(id: u64, path: PathBuf) -> Self {
        Tab { id, panes: [Pane::new(path.clone()), Pane::new(path)], active: 0, dual: false }
    }
}

pub struct App {
    panes: [Pane; 2],
    active: usize,
    dual: bool,
    /// All tabs in order. `tabs[cur]` is a placeholder while that tab is active.
    tabs: Vec<Tab>,
    cur: usize,
    next_tab_id: u64,
    /// Tab strip on the left (stacked vertically) instead of along the top.
    tabs_vertical: bool,
    /// Dual pane arranged top/bottom instead of left/right.
    split_stacked: bool,
    thumbs: crate::thumbs::Thumbs,
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
    recent: Vec<PathBuf>,
    /// Sidebar favourites as (label, folder), in display order.
    quick: Vec<(String, PathBuf)>,
    /// Favourites were reordered by a drag that is still going; save when it ends.
    fav_dirty: bool,
    drives: Vec<PathBuf>,
    font: Option<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let font = crate::fonts::install(&cc.egui_ctx);
        theme::install(&cc.egui_ctx);
        let (tx, rx) = channel();
        let start = std::env::args_os()
            .nth(1)
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(fsx::home_dir);
        let mut app = App {
            panes: [Pane::new(start.clone()), Pane::new(start)],
            active: 0,
            dual: false,
            tabs: vec![Tab::new(0, PathBuf::new())],
            cur: 0,
            next_tab_id: 1,
            tabs_vertical: false,
            split_stacked: false,
            thumbs: crate::thumbs::Thumbs::new(&cc.egui_ctx),
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
            recent: Vec::new(),
            quick: initial_favorites(),
            fav_dirty: false,
            drives: fsx::drives(),
            font,
        };
        if app.font.is_none() {
            app.status = "No Japanese font found; set FONT_8823 to a .ttf/.ttc path".into();
        }
        app.publish_favorites();
        app.load(0);
        app.load(1);
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
        let name = favorite_name(&path, &fsx::quick_access());
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
        let pane = &mut self.panes[p];
        pane.req = req;
        pane.loading = true;
        let path = pane.path.clone();
        fsx::spawn_listing(self.bg(), p, req, path);
    }

    fn navigate(&mut self, p: usize, path: PathBuf, record: bool) {
        if !path.is_dir() {
            self.panes[p].error = Some(format!("Not a folder: {}", path.display()));
            return;
        }
        let pane = &mut self.panes[p];
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

    /// Swap the live tab state with the slot `tabs[cur]`.
    fn swap_cur(&mut self) {
        let t = &mut self.tabs[self.cur];
        std::mem::swap(&mut self.panes, &mut t.panes);
        std::mem::swap(&mut self.active, &mut t.active);
        std::mem::swap(&mut self.dual, &mut t.dual);
    }

    fn switch_tab(&mut self, to: usize) {
        if to >= self.tabs.len() || to == self.cur {
            return;
        }
        self.swap_cur();
        self.cur = to;
        self.swap_cur();
        // The folder may have changed while the tab was in the background.
        self.refresh_all();
    }

    fn new_tab(&mut self, path: PathBuf) {
        let sh = self.show_hidden;
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let gallery = self.panes[self.active].gallery;
        let mut tab = Tab::new(id, path);
        tab.panes[0].gallery = gallery;
        self.tabs.insert(self.cur + 1, tab);
        self.switch_tab(self.cur + 1);
        for p in &mut self.panes {
            p.rebuild_view(sh);
        }
    }

    fn close_tab(&mut self, i: usize) {
        if self.tabs.len() <= 1 {
            return;
        }
        if i == self.cur {
            let to = if i + 1 < self.tabs.len() { i + 1 } else { i - 1 };
            self.switch_tab(to);
        }
        let mut t = self.tabs.remove(i);
        for p in &mut t.panes {
            if let Some(s) = p.search.take() {
                s.cancel.store(true, Ordering::Relaxed);
            }
        }
        if i < self.cur {
            self.cur -= 1;
        }
    }

    fn tab_title(&self, i: usize) -> String {
        let (panes, active) = if i == self.cur { (&self.panes, self.active) } else { (&self.tabs[i].panes, self.tabs[i].active) };
        let p = &panes[active].path;
        p.file_name().map(crate::text::display_name).unwrap_or_else(|| p.display().to_string())
    }

    /// The pane a background result belongs to: in the active tab or a parked one.
    fn pane_for(&mut self, pane: usize, req: u64) -> Option<&mut Pane> {
        if self.panes[pane].req == req {
            return Some(&mut self.panes[pane]);
        }
        let cur = self.cur;
        self.tabs
            .iter_mut()
            .enumerate()
            .filter(|(i, _)| *i != cur)
            .map(|(_, t)| &mut t.panes[pane])
            .find(|p| p.req == req)
    }

    fn move_tab(&mut self, from: usize, to: usize) {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let cur_id = self.tabs[self.cur].id;
        let t = self.tabs.remove(from);
        self.tabs.insert(to, t);
        self.cur = self.tabs.iter().position(|t| t.id == cur_id).unwrap_or(0);
    }

    /// The tab strip: along the top (`vertical == false`) or stacked in a left column.
    /// Click to switch, drag to reorder, middle-click or × to close.
    fn tab_bar(&mut self, ui: &mut egui::Ui, vertical: bool) {
        let mut switch = None;
        let mut close = None;
        let mut add = false;
        let mut rects: Vec<egui::Rect> = Vec::new();
        let mut dragging: Option<usize> = None;
        let n = self.tabs.len();
        let font = egui::FontId::proportional(13.0);
        let mut body = |ui: &mut egui::Ui| {
            for i in 0..n {
                let title = self.tab_title(i);
                let selected = i == self.cur;
                let pal = theme::pal(ui);
                let color = if selected { pal.strong } else { pal.weak };
                let max_w = if vertical { (ui.available_width() - 30.0).max(40.0) } else { 180.0 };
                let galley = theme::elided(ui, &title, font.clone(), color, max_w);
                let text_w = galley.size().x.min(max_w);
                let w = if vertical { ui.available_width() } else { text_w + 22.0 + if n > 1 { 20.0 } else { 0.0 } };
                let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 28.0), egui::Sense::hover());
                let id = egui::Id::unique(("tab", self.tabs[i].id));
                let r = ui.interact(rect, id, egui::Sense::click_and_drag());
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
                let r = r.on_hover_text(self.tab_path(i));
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
            let pal = theme::pal(ui);
            let (pr, presp) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::click());
            if presp.hovered() {
                ui.painter().rect_filled(pr, 6.0, pal.surface);
            }
            let st = egui::Stroke::new(1.4, if presp.hovered() { pal.strong } else { pal.weak });
            let m = pr.center();
            ui.painter().line_segment([m - egui::vec2(5.0, 0.0), m + egui::vec2(5.0, 0.0)], st);
            ui.painter().line_segment([m - egui::vec2(0.0, 5.0), m + egui::vec2(0.0, 5.0)], st);
            if presp.on_hover_text(format!("New tab ({})", crate::plat::keys("Ctrl+T"))).clicked() {
                add = true;
            }
        };
        if vertical {
            ui.spacing_mut().item_spacing.y = 2.0;
            body(ui);
        } else {
            ui.spacing_mut().item_spacing.x = 2.0;
            body(ui);
        }

        // Drag to reorder: move the dragged tab once the pointer passes a neighbour's middle.
        if let (Some(i), Some(pos)) = (dragging, ui.ctx().pointer_interact_pos()) {
            let along = |p: egui::Pos2| if vertical { p.y } else { p.x };
            let mut to = i;
            for (j, r) in rects.iter().enumerate() {
                if (j > i && along(pos) > along(r.center())) || (j < i && along(pos) < along(r.center()) && to == i) {
                    to = j;
                }
            }
            if to != i {
                self.move_tab(i, to);
                switch = None;
            }
        }
        if let Some(i) = switch {
            self.switch_tab(i);
        }
        if let Some(i) = close {
            self.close_tab(i);
        }
        if add {
            let p = self.panes[self.active].path.clone();
            self.new_tab(p);
        }
    }

    fn tab_path(&self, i: usize) -> String {
        let (panes, active) = if i == self.cur { (&self.panes, self.active) } else { (&self.tabs[i].panes, self.tabs[i].active) };
        panes[active].path.display().to_string()
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
        let g = self.panes[a].gallery;
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
        let p = &mut self.panes[self.active];
        p.gallery = on;
        p.scroll_to_cursor = true;
    }

    fn refresh_all(&mut self) {
        for p in 0..2 {
            if self.panes[p].search.is_none() {
                self.load(p);
            }
        }
    }

    fn poll(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Listing { pane, req, path, result } => {
                    let sh = self.show_hidden;
                    let Some(p) = self.pane_for(pane, req) else { continue };
                    if p.path != path {
                        continue;
                    }
                    p.loading = false;
                    match result {
                        Ok(entries) => {
                            p.error = None;
                            p.set_entries(entries, sh);
                        }
                        Err(e) => {
                            p.error = Some(e);
                            p.set_entries(Vec::new(), sh);
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

    fn start_search(&mut self, p: usize, query: String) {
        let req = self.next_req;
        self.next_req += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        let pane = &mut self.panes[p];
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

    fn apply(&mut self, a: Action) {
        let sh = self.show_hidden;
        match a {
            Action::Activate(p) => self.active = p,
            Action::Navigate(p, path) => self.navigate(p, path, true),
            Action::OpenCursor(p) => {
                let Some(e) = self.panes[p].cursor_entry().cloned() else { return };
                if e.is_dir {
                    self.navigate(p, e.path, true);
                } else if let Err(err) = open::that_detached(&e.path) {
                    self.status = format!("Could not open {}: {err}", e.name);
                }
            }
            Action::Up(p) => {
                let pane = &mut self.panes[p];
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
                let pane = &mut self.panes[p];
                if let Some(prev) = pane.back.pop() {
                    pane.fwd.push(pane.path.clone());
                    pane.pending_select = Some(pane.path.clone());
                    self.navigate(p, prev, false);
                }
            }
            Action::Forward(p) => {
                let pane = &mut self.panes[p];
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
                let t = self.panes[p].targets();
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
                if self.panes[p].search.is_some() {
                    return;
                }
                if let Some((src, cut)) = self.clipboard.clone() {
                    if cut {
                        self.clipboard = None;
                    }
                    self.busy = true;
                    self.status = "Working…".into();
                    fsx::spawn_paste(self.bg(), src, self.panes[p].path.clone(), cut);
                }
            }
            Action::Delete(p, permanent) => {
                let paths = self.panes[p].targets();
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
                let pane = &mut self.panes[p];
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
                        self.panes[p].pending_select = Some(to);
                        self.refresh_all();
                    }
                    Err(e) => self.status = format!("Rename failed: {e}"),
                }
            }
            Action::NewFolder(p) => {
                self.dialog = Some(Dialog::NewFolder { pane: p, name: "新しいフォルダー".into(), focus: true });
            }
            Action::OpenInNewTab(path) => self.new_tab(path),
            Action::ToggleFavorite(path) => {
                if self.is_favorite(&path) {
                    self.remove_favorite(&path);
                } else {
                    self.add_favorite(path);
                }
            }
            Action::OpenInOther(p) => {
                if let Some(e) = self.panes[p].cursor_entry().cloned() {
                    self.dual = true;
                    let o = 1 - p;
                    self.navigate(o, e.path, true);
                }
            }
            Action::CopyPath(p) => {
                let t = self.panes[p].targets();
                let s: Vec<String> = t.iter().map(|x| x.display().to_string()).collect();
                if !s.is_empty() {
                    self.ctx.copy_text(s.join("\r\n"));
                    self.status = format!("Copied {} path(s)", s.len());
                }
            }
            Action::Reveal(path) => crate::plat::reveal(&path),
            Action::Terminal(path) => crate::plat::terminal(&path),
            Action::CloseSearch(p) => {
                if let Some(s) = self.panes[p].search.take() {
                    s.cancel.store(true, Ordering::Relaxed);
                }
                self.load(p);
            }
        }
        let _ = sh;
    }

    fn global_keys(&mut self, ctx: &egui::Context, out: &mut Vec<Action>) {
        if self.dialog.is_some() {
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
            let g = self.panes[self.active].gallery;
            self.set_gallery(!g);
        }
        // ⌘/Ctrl+D: add the selected folders to Favorites, or this folder when none is selected.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::D)) {
            let pane = &self.panes[self.active];
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
        if t_new {
            let p = self.panes[self.active].path.clone();
            self.new_tab(p);
        }
        if t_close {
            self.close_tab(self.cur);
        }
        let n = self.tabs.len();
        if t_next {
            self.switch_tab((self.cur + 1) % n);
        }
        if t_prev {
            self.switch_tab((self.cur + n - 1) % n);
        }
        if let Some(j) = t_jump {
            // ⌘9 is always the last tab, as in browsers.
            self.switch_tab(if j == 8 { n - 1 } else { j });
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
            for p in &mut self.panes {
                p.rebuild_view(sh);
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
            let pane = &mut self.panes[a];
            let had_filter = !pane.filter.is_empty();
            pane.keys(ctx, a, self.type_to_filter, out);
            if had_filter && pane.filter.is_empty() {
                pane.rebuild_view(sh);
            }
        }

        // Files dropped from Explorer/Finder are copied into the active folder.
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if !dropped.is_empty() {
            self.busy = true;
            fsx::spawn_paste(self.bg(), dropped, self.panes[self.active].path.clone(), false);
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui, out: &mut Vec<Action>) {
        let mut add_current = false;
        let mut remove: Option<PathBuf> = None;
        let mut reorder: Option<(usize, usize)> = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            let cur = self.panes[self.active].path.clone();
            let a = self.active;
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
                    out.push(Action::OpenInNewTab(path.clone()));
                }
                if r.dragged() {
                    dragging = Some(i);
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                }
                r.context_menu(|ui| {
                    if ui.button("Open in new tab").clicked() {
                        out.push(Action::OpenInNewTab(path.clone()));
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
            let p = self.panes[self.active].path.clone();
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
        let mut keep = true;
        let mut after: Option<Box<dyn FnOnce(&mut App)>> = None;
        let modal = egui::Modal::new(egui::Id::unique("dialog")).show(ctx, |ui| {
            ui.set_width(460.0);
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
                    ui.weak(self.panes[*pane].path.display().to_string());
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
                Dialog::Help => {
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
        let dir = self.panes[p].path.clone();
        let target = fsx::unique_target(&dir, &name);
        match std::fs::create_dir(&target) {
            Ok(()) => {
                self.panes[p].pending_select = Some(target);
                self.load(p);
            }
            Err(e) => self.status = format!("Could not create folder: {e}"),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll();
        self.thumbs.poll(&ctx);
        let mut actions = Vec::new();
        self.global_keys(&ctx, &mut actions);

        let pal = theme::pal(ui);
        let side_frame = egui::Frame::new().fill(pal.panel).inner_margin(egui::Margin { left: 8, right: 8, top: 4, bottom: 8 });
        egui::Panel::bottom("status")
            .frame(egui::Frame::new().fill(pal.panel).inner_margin(egui::Margin::symmetric(12, 5)))
            .show_separator_line(false)
            .show(ui, |ui| {
            ui.style_mut().override_font_id = Some(egui::FontId::proportional(12.0));
            ui.visuals_mut().override_text_color = Some(pal.weak);
            ui.horizontal(|ui| {
                let p = &self.panes[self.active];
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

        if self.tabs_vertical {
            egui::Panel::left("vtabs").default_size(180.0).frame(side_frame).show_separator_line(false).show(ui, |ui| {
                section(ui, "TABS");
                egui::ScrollArea::vertical().id_salt("vtabs-scroll").show(ui, |ui| self.tab_bar(ui, true));
            });
        }

        let central = egui::Frame::new().fill(pal.bg).inner_margin(egui::Margin { left: 10, right: 10, top: 8, bottom: 4 });
        egui::CentralPanel::default().frame(central).show(ui, |ui| {
            ui.horizontal(|ui| {
                let icons_w = 210.0;
                if !self.tabs_vertical {
                    let w = (ui.available_width() - icons_w).max(80.0);
                    egui::ScrollArea::horizontal()
                        .id_salt("tabs-scroll")
                        .max_width(w)
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                        .show(ui, |ui| ui.horizontal(|ui| self.tab_bar(ui, false)));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| self.view_icons(ui));
            });
            ui.add_space(4.0);
            let sh = self.show_hidden;
            if self.dual && self.split_stacked {
                let avail = ui.available_rect_before_wrap();
                let gap = 6.0;
                let h = ((avail.height() - gap) / 2.0).max(60.0);
                for i in 0..2 {
                    let top = avail.top() + i as f32 * (h + gap);
                    let rect = egui::Rect::from_min_size(egui::pos2(avail.left(), top), egui::vec2(avail.width(), h));
                    let active = self.active == i;
                    ui.scope_builder(egui::UiBuilder::new().max_rect(rect).id_salt(("stacked", i)), |ui| {
                        ui.set_clip_rect(rect.expand(1.0));
                        pane_frame(ui, active, |ui| self.panes[i].ui(ui, i, active, sh, &mut self.thumbs, &mut actions));
                    });
                }
            } else if self.dual {
                ui.columns(2, |cols| {
                    for (i, col) in cols.iter_mut().enumerate() {
                        let active = self.active == i;
                        pane_frame(col, active, |ui| self.panes[i].ui(ui, i, active, sh, &mut self.thumbs, &mut actions));
                    }
                });
            } else {
                self.panes[0].ui(ui, 0, true, sh, &mut self.thumbs, &mut actions);
            }
        });

        self.dialogs(&ctx);
        for a in actions {
            self.apply(a);
        }
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
    let standard = fsx::quick_access();
    match crate::config::load_favorites() {
        Some(paths) => paths.into_iter().map(|p| (favorite_name(&p, &standard), p)).collect(),
        None => standard,
    }
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
    let size = egui::vec2(ui.available_width(), 28.0);
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
    let galley = theme::elided(ui, label, egui::FontId::proportional(13.5), text_c, rect.width() - 44.0);
    theme::paint_left(&p, galley, egui::pos2(rect.left() + 36.0, rect.center().y), text_c);
    resp
}
