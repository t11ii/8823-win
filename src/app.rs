use crate::fsx::{self, Bg, Msg};
use crate::pane::{Action, Pane, Rename, SearchState};
use crate::text::{fold, fuzzy_score};
use eframe::egui::{self, Key};
use std::path::PathBuf;
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

pub struct App {
    panes: [Pane; 2],
    active: usize,
    dual: bool,
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
    quick: Vec<(String, PathBuf)>,
    drives: Vec<PathBuf>,
    font: Option<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let font = crate::fonts::install(&cc.egui_ctx);
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
            quick: fsx::quick_access(),
            drives: fsx::drives(),
            font,
        };
        app.status = match &app.font {
            Some(f) => format!("Japanese font: {f}"),
            None => "No Japanese font found; set FONT_8823 to a .ttf/.ttc path".into(),
        };
        app.load(0);
        app.load(1);
        app
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
                    let p = &mut self.panes[pane];
                    if p.req != req || p.path != path {
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
                    let p = &mut self.panes[pane];
                    if p.req != req {
                        continue;
                    }
                    if let Some(s) = &mut p.search {
                        s.scanned = scanned;
                    }
                    p.entries.extend(items);
                    p.rebuild_view(sh);
                }
                Msg::SearchDone { pane, req, scanned } => {
                    let sh = self.show_hidden;
                    let p = &mut self.panes[pane];
                    if p.req != req {
                        continue;
                    }
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
            pane.keys(ctx, a, out);
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
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(6.0);
            ui.weak("QUICK ACCESS");
            let cur = self.panes[self.active].path.clone();
            for (label, path) in &self.quick {
                if ui.selectable_label(&cur == path, label.as_str()).on_hover_text(path.display().to_string()).clicked() {
                    out.push(Action::Navigate(self.active, path.clone()));
                }
            }
            ui.add_space(10.0);
            ui.weak("DRIVES");
            for d in &self.drives {
                if ui.selectable_label(&cur == d, d.display().to_string()).clicked() {
                    out.push(Action::Navigate(self.active, d.clone()));
                }
            }
            if !self.recent.is_empty() {
                ui.add_space(10.0);
                ui.weak("RECENT");
                for r in self.recent.iter().take(8) {
                    let name = r.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| r.display().to_string());
                    if ui.selectable_label(false, name).on_hover_text(r.display().to_string()).clicked() {
                        out.push(Action::Navigate(self.active, r.clone()));
                    }
                }
            }
        });
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
        let mut actions = Vec::new();
        self.global_keys(&ctx, &mut actions);

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                let p = &self.panes[self.active];
                let sel_size: u64 = p.selected.iter().map(|&i| p.entries[i].size).sum();
                ui.label(format!("{} items", p.view.len()));
                if !p.selected.is_empty() {
                    ui.label(format!("· {} selected ({})", p.selected.len(), fsx::human_size(sel_size)));
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
                    if ui.small_button(if self.dual { "Single pane" } else { "Dual pane" }).on_hover_text(crate::plat::keys("Ctrl+\\")).clicked() {
                        self.dual = !self.dual;
                        self.active = 0;
                    }
                    if self.busy {
                        ui.spinner();
                    }
                    ui.weak(&self.status);
                });
            });
        });

        egui::Panel::left("sidebar").default_size(190.0).show(ui, |ui| self.sidebar(ui, &mut actions));

        egui::CentralPanel::default().show(ui, |ui| {
            let sh = self.show_hidden;
            if self.dual {
                ui.columns(2, |cols| {
                    for (i, col) in cols.iter_mut().enumerate() {
                        let active = self.active == i;
                        pane_frame(col, active, |ui| self.panes[i].ui(ui, i, active, sh, &mut actions));
                    }
                });
            } else {
                self.panes[0].ui(ui, 0, true, sh, &mut actions);
            }
        });

        self.dialogs(&ctx);
        for a in actions {
            self.apply(a);
        }
    }
}

fn pane_frame(ui: &mut egui::Ui, active: bool, add: impl FnOnce(&mut egui::Ui)) {
    let stroke = if active { ui.visuals().selection.stroke } else { ui.visuals().widgets.noninteractive.bg_stroke };
    egui::Frame::new().stroke(egui::Stroke::new(1.0, stroke.color)).inner_margin(4.0).corner_radius(4.0).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        add(ui)
    });
}
