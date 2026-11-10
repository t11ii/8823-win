//! A single file-list pane: its state, navigation history, selection and rendering.

use crate::fsx::{self, Entry, SortKey};
use crate::theme::{self, Glyph};
use crate::thumbs::Thumbs;
use crate::text::fold;
use eframe::egui::{self, Align2, Color32, FontId, Key, Rect, Sense, Stroke, Vec2, pos2, vec2};
use crate::plat::{BACK_KEY, DELETE_KEY, FWD_KEY, keys as k};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub const ROW_H: f32 = 26.0;
const TILE_W: f32 = 132.0;
const TILE_H: f32 = 156.0;

pub struct SearchState {
    pub query: String,
    pub running: bool,
    pub scanned: usize,
    pub cancel: Arc<AtomicBool>,
}

pub struct Rename {
    pub path: PathBuf,
    pub text: String,
    pub focus: bool,
}

pub struct Pane {
    pub path: PathBuf,
    pub entries: Vec<Entry>,
    /// Indices into `entries` after hidden-file and filter rules, in display order.
    pub view: Vec<usize>,
    pub cursor: usize,
    pub anchor: usize,
    pub selected: HashSet<usize>,
    pub back: Vec<PathBuf>,
    pub fwd: Vec<PathBuf>,
    pub filter: String,
    pub sort: SortKey,
    pub asc: bool,
    pub loading: bool,
    pub req: u64,
    pub error: Option<String>,
    pub search: Option<SearchState>,
    pub pending_select: Option<PathBuf>,
    pub scroll_to_cursor: bool,
    pub scroll_y: f32,
    pub view_h: f32,
    pub rename: Option<Rename>,
    pub path_edit: Option<String>,
    pub focus_filter: bool,
    /// Where the filter box was last drawn; the IME candidate window is placed there.
    pub filter_rect: Rect,
    pub filter_cursor_to_end: bool,
    /// Gallery (thumbnail grid) instead of the detail list.
    pub gallery: bool,
    /// Items per visual row: 1 in the list, the column count in the gallery.
    pub per_row: usize,
}

/// Things a pane asks the app to do; applied after the frame's UI is built.
pub enum Action {
    Activate(usize),
    Navigate(usize, PathBuf),
    OpenCursor(usize),
    Up(usize),
    Back(usize),
    Forward(usize),
    Refresh(usize),
    Copy(usize),
    Cut(usize),
    Paste(usize),
    Delete(usize, bool),
    StartRename(usize),
    CommitRename(usize, PathBuf, String),
    NewFolder(usize),
    OpenInOther(usize),
    CopyPath(usize),
    Reveal(PathBuf),
    Terminal(PathBuf),
    CloseSearch(usize),
    OpenInNewTab(PathBuf),
    /// Add the folder to the sidebar favourites, or remove it if it is already there.
    ToggleFavorite(PathBuf),
}

/// Where the app publishes the favourite folders (egui temp data), so menus here can say Add or Remove.
pub fn favorites_id() -> egui::Id {
    egui::Id::unique("8823-favorites")
}

fn is_favorite(ui: &egui::Ui, path: &Path) -> bool {
    ui.ctx()
        .data(|d| d.get_temp::<Arc<Vec<PathBuf>>>(favorites_id()))
        .is_some_and(|v| v.iter().any(|f| f == path))
}

fn favorite_label(ui: &egui::Ui, path: &Path, this_folder: bool) -> String {
    match (is_favorite(ui, path), this_folder) {
        (false, false) => format!("Add to Favorites\t{}", k("Ctrl+D")),
        (false, true) => format!("Add this folder to Favorites\t{}", k("Ctrl+D")),
        (true, false) => "Remove from Favorites".to_string(),
        (true, true) => "Remove this folder from Favorites".to_string(),
    }
}

impl Pane {
    pub fn new(path: PathBuf) -> Self {
        Pane {
            path,
            entries: Vec::new(),
            view: Vec::new(),
            cursor: 0,
            anchor: 0,
            selected: HashSet::new(),
            back: Vec::new(),
            fwd: Vec::new(),
            filter: String::new(),
            sort: SortKey::Name,
            asc: true,
            loading: false,
            req: 0,
            error: None,
            search: None,
            pending_select: None,
            scroll_to_cursor: true,
            scroll_y: 0.0,
            view_h: 400.0,
            rename: None,
            path_edit: None,
            focus_filter: false,
            filter_rect: Rect::NOTHING,
            filter_cursor_to_end: false,
            gallery: false,
            per_row: 1,
        }
    }

    pub fn cursor_entry(&self) -> Option<&Entry> {
        self.view.get(self.cursor).map(|&i| &self.entries[i])
    }

    /// Selected entries, or the entry under the cursor when nothing is selected.
    pub fn targets(&self) -> Vec<PathBuf> {
        if self.selected.is_empty() {
            self.cursor_entry().map(|e| vec![e.path.clone()]).unwrap_or_default()
        } else {
            let mut v: Vec<usize> = self.view.iter().copied().filter(|i| self.selected.contains(i)).collect();
            v.sort_by_key(|i| self.view.iter().position(|x| x == i));
            v.into_iter().map(|i| self.entries[i].path.clone()).collect()
        }
    }

    pub fn set_entries(&mut self, mut entries: Vec<Entry>, show_hidden: bool) {
        let keep = self
            .pending_select
            .take()
            .or_else(|| self.cursor_entry().map(|e| e.path.clone()));
        let (sort, asc) = (self.sort, self.asc);
        entries.sort_by(|a, b| fsx::compare(a, b, sort, asc));
        self.entries = entries;
        self.selected.clear();
        self.rebuild_view(show_hidden);
        self.cursor = keep
            .and_then(|p| self.view.iter().position(|&i| self.entries[i].path == p))
            .unwrap_or(0);
        self.anchor = self.cursor;
        self.scroll_to_cursor = true;
    }

    pub fn resort(&mut self, show_hidden: bool) {
        let keep = self.cursor_entry().map(|e| e.path.clone());
        let sel: HashSet<PathBuf> = self.selected.iter().map(|&i| self.entries[i].path.clone()).collect();
        let (sort, asc) = (self.sort, self.asc);
        self.entries.sort_by(|a, b| fsx::compare(a, b, sort, asc));
        self.selected = self.entries.iter().enumerate().filter(|(_, e)| sel.contains(&e.path)).map(|(i, _)| i).collect();
        self.rebuild_view(show_hidden);
        self.cursor = keep.and_then(|p| self.view.iter().position(|&i| self.entries[i].path == p)).unwrap_or(0);
        self.scroll_to_cursor = true;
    }

    pub fn rebuild_view(&mut self, show_hidden: bool) {
        let f = fold(&self.filter);
        let terms: Vec<&str> = f.split_whitespace().collect();
        self.view = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| show_hidden || !e.hidden || self.search.is_some())
            .filter(|(_, e)| terms.iter().all(|t| e.key.contains(t)))
            .map(|(i, _)| i)
            .collect();
        if self.cursor >= self.view.len() {
            self.cursor = self.view.len().saturating_sub(1);
        }
    }

    pub fn move_cursor(&mut self, to: isize, extend: bool) {
        if self.view.is_empty() {
            return;
        }
        let to = to.clamp(0, self.view.len() as isize - 1) as usize;
        self.cursor = to;
        if extend {
            let (a, b) = (self.anchor.min(to), self.anchor.max(to));
            self.selected = self.view[a..=b].iter().copied().collect();
        } else {
            self.anchor = to;
        }
        self.scroll_to_cursor = true;
    }

    /// Type-to-filter: typing while the list has focus jumps into the filter box.
    ///
    /// To make Japanese input work from the very first key, the IME is kept enabled while
    /// the list is focused (egui only enables it for a focused text field). A composition
    /// that starts here moves focus to the filter box, which then receives the rest of it.
    fn type_to_filter(&mut self, ctx: &egui::Context) {
        let r = if self.filter_rect.is_positive() { self.filter_rect } else { Rect::from_min_size(pos2(0.0, 0.0), vec2(1.0, 1.0)) };
        ctx.output_mut(|o| {
            o.ime = Some(egui::output::IMEOutput {
                purpose: egui::IMEPurpose::Normal,
                rect: r,
                cursor_rect: Rect::from_min_size(pos2(r.left() + 6.0, r.top()), vec2(1.0, r.height())),
                should_interrupt_composition: false,
            });
        });
        let (text, composing) = ctx.input(|i| {
            let mut text = String::new();
            let mut composing = false;
            if i.modifiers.command || i.modifiers.ctrl || i.modifiers.alt {
                return (text, composing);
            }
            for e in &i.events {
                match e {
                    egui::Event::Text(t) => text.push_str(t),
                    egui::Event::Ime(egui::ImeEvent::Commit(t)) => text.push_str(t),
                    egui::Event::Ime(egui::ImeEvent::Preedit { text: t, .. }) if !t.is_empty() => composing = true,
                    _ => {}
                }
            }
            (text, composing)
        });
        // A lone space keeps its list meaning (toggle selection), also when an IME
        // turns it into a full-width space.
        let text = if text.trim().is_empty() { String::new() } else { text };
        if !text.is_empty() || composing {
            self.filter.push_str(&text);
            self.focus_filter = true;
            self.filter_cursor_to_end = true;
        }
    }

    fn item_h(&self) -> f32 {
        if self.gallery { TILE_H } else { ROW_H }
    }

    pub fn page_rows(&self) -> isize {
        ((self.view_h / self.item_h()) as isize - 1).max(1) * self.per_row as isize
    }

    /// Keyboard handling for the active pane when no text field has focus.
    pub fn keys(&mut self, ctx: &egui::Context, id: usize, type_to_filter: bool, out: &mut Vec<Action>) {
        if type_to_filter {
            self.type_to_filter(ctx);
        }
        let (ev, m) = ctx.input(|i| {
            let mut keys: Vec<Key> = Vec::new();
            for e in &i.events {
                match e {
                    egui::Event::Key { key, pressed: true, .. } => keys.push(*key),
                    // egui-winit turns ⌘/Ctrl+C/X/V (and Shift+Del, Ctrl/Shift+Ins on Windows)
                    // into clipboard events instead of key events.
                    egui::Event::Copy => out.push(Action::Copy(id)),
                    egui::Event::Cut if i.modifiers.shift && !i.modifiers.command => out.push(Action::Delete(id, true)),
                    egui::Event::Cut => out.push(Action::Cut(id)),
                    egui::Event::Paste(_) | egui::Event::PasteImage(_) => out.push(Action::Paste(id)),
                    _ => {}
                }
            }
            (keys, i.modifiers)
        });
        let c = self.cursor as isize;
        let pr = self.per_row.max(1) as isize;
        for k in ev {
            // macOS Finder conventions, checked first so they win over the generic bindings.
            #[cfg(target_os = "macos")]
            {
                let cmd = m.mac_cmd || m.command;
                let handled = match k {
                    Key::Backspace | Key::Delete if cmd => {
                        out.push(Action::Delete(id, m.alt));
                        true
                    }
                    Key::ArrowUp if cmd => {
                        out.push(Action::Up(id));
                        true
                    }
                    Key::ArrowDown | Key::O if cmd => {
                        out.push(Action::OpenCursor(id));
                        true
                    }
                    Key::OpenBracket if cmd => {
                        out.push(Action::Back(id));
                        true
                    }
                    Key::CloseBracket if cmd => {
                        out.push(Action::Forward(id));
                        true
                    }
                    Key::C if cmd && m.alt => {
                        out.push(Action::CopyPath(id));
                        true
                    }
                    _ => false,
                };
                if handled {
                    continue;
                }
            }
            match k {
                Key::ArrowDown if !m.alt => self.move_cursor(c + pr, m.shift),
                Key::ArrowUp if m.alt => out.push(Action::Up(id)),
                Key::ArrowUp => self.move_cursor(c - pr, m.shift),
                Key::ArrowLeft if self.gallery && !m.alt && !m.command => self.move_cursor(c - 1, m.shift),
                Key::ArrowRight if self.gallery && !m.alt && !m.command => self.move_cursor(c + 1, m.shift),
                Key::PageDown => self.move_cursor(c + self.page_rows(), m.shift),
                Key::PageUp => self.move_cursor(c - self.page_rows(), m.shift),
                Key::Home => self.move_cursor(0, m.shift),
                Key::End => self.move_cursor(isize::MAX / 2, m.shift),
                Key::Enter if m.command => {
                    if let Some(e) = self.cursor_entry().filter(|e| e.is_dir) {
                        out.push(Action::OpenInNewTab(e.path.clone()));
                    }
                }
                Key::Enter => out.push(Action::OpenCursor(id)),
                Key::Backspace => out.push(Action::Up(id)),
                Key::ArrowLeft if m.alt => out.push(Action::Back(id)),
                Key::ArrowRight if m.alt => out.push(Action::Forward(id)),
                Key::Space => {
                    if let Some(&i) = self.view.get(self.cursor) {
                        if !self.selected.remove(&i) {
                            self.selected.insert(i);
                        }
                        self.move_cursor(c + 1, false);
                    }
                }
                Key::A if m.command => self.selected = self.view.iter().copied().collect(),
                Key::C if m.command && m.shift => out.push(Action::CopyPath(id)),
                Key::C if m.command => out.push(Action::Copy(id)),
                Key::X if m.command => out.push(Action::Cut(id)),
                Key::V if m.command => out.push(Action::Paste(id)),
                Key::N if m.command && m.shift => out.push(Action::NewFolder(id)),
                Key::F2 => out.push(Action::StartRename(id)),
                Key::F5 => out.push(Action::Refresh(id)),
                Key::Delete => out.push(Action::Delete(id, m.shift)),
                Key::L if m.command => self.path_edit = Some(self.path.display().to_string()),
                Key::F if m.command && !m.shift => self.focus_filter = true,
                Key::Escape => {
                    if self.search.is_some() {
                        out.push(Action::CloseSearch(id));
                    } else if !self.filter.is_empty() {
                        self.filter.clear(); // the app rebuilds the view right after
                    } else {
                        self.selected.clear();
                    }
                }
                _ => {}
            }
        }
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        id: usize,
        active: bool,
        show_hidden: bool,
        thumbs: &mut Thumbs,
        out: &mut Vec<Action>,
    ) {
        self.header_ui(ui, id, show_hidden, out);
        ui.add_space(2.0);
        if self.gallery {
            self.per_row = ((ui.available_width() - 14.0) / TILE_W).floor().max(1.0) as usize;
        } else {
            self.per_row = 1;
            self.columns_ui(ui, show_hidden);
        }

        if let Some(err) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, format!("⚠ {err}"));
        }
        if let Some(s) = &self.search {
            ui.horizontal(|ui| {
                let state = if s.running { "Searching" } else { "Found" };
                ui.label(format!("{state} “{}”: {} hits, {} scanned", s.query, self.view.len(), s.scanned));
                if ui.small_button("✕ Close search").clicked() {
                    out.push(Action::CloseSearch(id));
                }
            });
        }

        let mut scroll = egui::ScrollArea::vertical().id_salt(("list", id)).auto_shrink([false, false]);
        let ih = self.item_h();
        if self.scroll_to_cursor {
            let top = (self.cursor / self.per_row.max(1)) as f32 * ih;
            let mut y = self.scroll_y;
            if top < y {
                y = top;
            } else if top + ih > y + self.view_h {
                y = top + ih - self.view_h;
            }
            scroll = scroll.vertical_scroll_offset(y.max(0.0));
            self.scroll_to_cursor = false;
        }

        // Background of the list (empty space below the rows): activate pane, offer paste/new-folder.
        // It must be registered BEFORE the rows: egui gives clicks to the widget registered last,
        // so a background added afterwards would swallow every click on a row.
        let bg_rect = ui.available_rect_before_wrap();
        let bg = ui.interact(bg_rect, ui.scope_id().with(("bg", id)), Sense::click());
        if bg.clicked() {
            out.push(Action::Activate(id));
            self.selected.clear();
        }
        bg.context_menu(|ui| {
            if ui.button(format!("Paste\t{}", k("Ctrl+V"))).clicked() {
                out.push(Action::Paste(id));
            }
            if ui.button(format!("New folder\t{}", k("Ctrl+Shift+N"))).clicked() {
                out.push(Action::NewFolder(id));
            }
            if ui.button("Refresh\tF5").clicked() {
                out.push(Action::Refresh(id));
            }
            if ui.button("Open terminal here").clicked() {
                out.push(Action::Terminal(self.path.clone()));
            }
            ui.separator();
            if ui.button(favorite_label(ui, &self.path, true)).clicked() {
                out.push(Action::ToggleFavorite(self.path.clone()));
            }
        });

        ui.spacing_mut().item_spacing.y = 0.0;
        let n = self.view.len();
        let output = if self.gallery {
            let pr = self.per_row.max(1);
            let rows = n.div_ceil(pr);
            scroll.show_rows(ui, TILE_H, rows, |ui, range| {
                for row in range {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        for vi in row * pr..((row + 1) * pr).min(n) {
                            self.tile_ui(ui, id, vi, active, thumbs, out);
                        }
                    });
                }
            })
        } else {
            scroll.show_rows(ui, ROW_H, n, |ui, range| {
                for vi in range {
                    self.row_ui(ui, id, vi, active, out);
                }
            })
        };
        self.scroll_y = output.state.clamped_offset().y;
        self.view_h = output.inner_rect.height();
    }

    fn header_ui(&mut self, ui: &mut egui::Ui, id: usize, show_hidden: bool, out: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            let pal = theme::pal(ui);
            ui.spacing_mut().item_spacing.x = 2.0;
            if theme::glyph_button(ui, Glyph::Back, !self.back.is_empty(), &format!("Back ({})", k(BACK_KEY))) {
                out.push(Action::Back(id));
            }
            if theme::glyph_button(ui, Glyph::Forward, !self.fwd.is_empty(), &format!("Forward ({})", k(FWD_KEY))) {
                out.push(Action::Forward(id));
            }
            if theme::glyph_button(ui, Glyph::Up, self.path.parent().is_some(), "Parent folder") {
                out.push(Action::Up(id));
            }
            ui.add_space(4.0);
            ui.spacing_mut().item_spacing.x = 8.0;

            // Filter box on the right, address bar in the remaining space.
            let filter_w = 190.0;
            let bar_h = 26.0;
            let path_w = (ui.available_width() - filter_w - 8.0).max(80.0);

            if let Some(text) = &mut self.path_edit {
                let resp = ui.add_sized([path_w, bar_h], egui::TextEdit::singleline(text).id(egui::Id::unique(("path", id))).align(egui::Align2::LEFT_CENTER));
                if !resp.has_focus() && !resp.lost_focus() {
                    resp.request_focus();
                }
                if resp.lost_focus() {
                    if ui.input(|i| i.key_pressed(Key::Enter)) {
                        let p = PathBuf::from(text.trim().trim_matches('"'));
                        out.push(Action::Navigate(id, p));
                    }
                    self.path_edit = None;
                }
            } else {
                // Address bar: breadcrumbs on a raised surface; click the empty part to type a path.
                let (bar, bar_resp) = ui.allocate_exact_size(vec2(path_w, bar_h), Sense::click());
                ui.painter().rect_filled(bar, 6.0, pal.surface);
                if bar_resp.clicked() {
                    self.path_edit = Some(self.path.display().to_string());
                }
                bar_resp.on_hover_cursor(egui::CursorIcon::Text);
                let inner = bar.shrink2(vec2(8.0, 0.0));
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::left_to_right(egui::Align::Center)));
                child.set_clip_rect(inner);
                egui::ScrollArea::horizontal()
                    .id_salt(("crumbs", id))
                    .stick_to_right(true)
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .show(&mut child, |ui| {
                        ui.horizontal_centered(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            let parts: Vec<&Path> = self.path.ancestors().collect();
                            let n = parts.len();
                            for (k, p) in parts.iter().rev().enumerate() {
                                let label = p
                                    .file_name()
                                    .map(crate::text::display_name)
                                    .unwrap_or_else(|| p.display().to_string());
                                if k > 0 {
                                    let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                                    ui.painter().line(
                                        vec![r.center() + vec2(-1.5, -3.5), r.center() + vec2(2.0, 0.0), r.center() + vec2(-1.5, 3.5)],
                                        Stroke::new(1.2, pal.weak),
                                    );
                                }
                                let last = k + 1 == n;
                                let text = egui::RichText::new(label).color(if last { pal.strong } else { pal.weak });
                                let r = ui.add(egui::Button::new(text).frame(false));
                                if r.clicked() {
                                    out.push(Action::Navigate(id, p.to_path_buf()));
                                }
                            }
                        });
                    });
            }

            let fid = egui::Id::unique(("filter", id));
            // While typing in the filter: ↑/↓ move through the list, Enter opens, Esc clears.
            if ui.memory(|m| m.has_focus(fid)) {
                let (up, down) = ui.input_mut(|i| {
                    (i.consume_key(egui::Modifiers::NONE, Key::ArrowUp), i.consume_key(egui::Modifiers::NONE, Key::ArrowDown))
                });
                if down {
                    self.move_cursor(self.cursor as isize + 1, false);
                }
                if up {
                    self.move_cursor(self.cursor as isize - 1, false);
                }
            }
            let focused = ui.memory(|m| m.has_focus(fid));
            let (frect, _) = ui.allocate_exact_size(vec2(filter_w, bar_h), Sense::hover());
            ui.painter().rect_filled(frect, 6.0, pal.surface);
            if focused || !self.filter.is_empty() {
                ui.painter().rect_stroke(frect, 6.0, Stroke::new(1.0, pal.accent), egui::StrokeKind::Inside);
            }
            theme::glyph(
                ui.painter(),
                Glyph::Search,
                pos2(frect.left() + 14.0, frect.center().y),
                if focused || !self.filter.is_empty() { pal.accent } else { pal.weak },
            );
            let edit_rect = Rect::from_min_max(pos2(frect.left() + 26.0, frect.top()), pos2(frect.right() - 6.0, frect.bottom()));
            let resp = ui.put(
                edit_rect,
                egui::TextEdit::singleline(&mut self.filter)
                    .id(fid)
                    .frame(egui::Frame::NONE)
                    .align(egui::Align2::LEFT_CENTER)
                    .hint_text(format!("Filter ({})", k("Ctrl+F"))),
            );
            self.filter_rect = frect;
            if self.focus_filter {
                resp.request_focus();
                self.focus_filter = false;
                // Typed-ahead text was appended for us: show it filtered right away.
                self.rebuild_view(show_hidden);
                self.cursor = 0;
                self.scroll_to_cursor = true;
            }
            if self.filter_cursor_to_end {
                if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), resp.id) {
                    let end = egui::text::CCursor::new(self.filter.chars().count());
                    st.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
                    st.store(ui.ctx(), resp.id);
                }
                self.filter_cursor_to_end = false;
            }
            if resp.changed() {
                self.rebuild_view(show_hidden);
                self.cursor = 0;
                self.scroll_to_cursor = true;
            }
            if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                out.push(Action::Activate(id));
                out.push(Action::OpenCursor(id));
            }
            if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                self.filter.clear();
                self.rebuild_view(show_hidden);
            }
            if resp.gained_focus() {
                out.push(Action::Activate(id));
            }
        });
    }

    fn columns_ui(&mut self, ui: &mut egui::Ui, show_hidden: bool) {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
        let mut colw = load_colw(ui.ctx());
        let cols = Columns::new(rect, self.search.is_some(), &colw);
        let painter = ui.painter();
        painter.line_segment(
            [rect.left_bottom(), rect.right_bottom()],
            Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
        );
        let mut hdr = |r: Rect, label: &str, key: Option<SortKey>, right: bool| {
            let resp = ui.interact(r, ui.scope_id().with(label), Sense::click());
            let mut text = label.to_string();
            if key == Some(self.sort) {
                text.push_str(if self.asc { " ▲" } else { " ▼" });
            }
            let pal = theme::pal(ui);
            let color = if resp.hovered() || key == Some(self.sort) { pal.text } else { pal.weak };
            let (pos, align) = if right {
                (pos2(r.right() - 6.0, r.center().y), Align2::RIGHT_CENTER)
            } else {
                (pos2(r.left() + 6.0, r.center().y), Align2::LEFT_CENTER)
            };
            ui.painter().text(pos, align, text, FontId::proportional(12.0), color);
            if let (true, Some(k)) = (resp.clicked(), key) {
                if self.sort == k {
                    self.asc = !self.asc;
                } else {
                    self.sort = k;
                    self.asc = true;
                }
                self.resort(show_hidden);
            }
        };
        hdr(cols.name, "Name", Some(SortKey::Name), false);
        if let Some(r) = cols.folder {
            hdr(r, "Folder", None, false);
        }
        hdr(cols.ext, "Type", Some(SortKey::Ext), false);
        hdr(cols.size, "Size", Some(SortKey::Size), true);
        hdr(cols.date, "Modified", Some(SortKey::Modified), false);

        // Drag the border at the right edge of a column header to resize that column.
        // Double-click a border to reset all widths.
        let line_c = ui.visuals().widgets.noninteractive.bg_stroke.color;
        let mut edges: Vec<(&str, Rect)> = vec![("name", cols.name)];
        if let Some(r) = cols.folder {
            edges.push(("folder", r));
        }
        edges.extend([("ext", cols.ext), ("size", cols.size), ("date", cols.date)]);
        let mut changed = false;
        for (key, r) in edges {
            let x = r.right();
            let hit = Rect::from_min_max(pos2(x - 4.0, rect.top()), pos2(x + 4.0, rect.bottom()));
            let resp = ui.interact(hit, ui.scope_id().with(("col-edge", key)), Sense::click_and_drag());
            let hot = resp.hovered() || resp.dragged();
            if hot {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            ui.painter().line_segment(
                [pos2(x, rect.top() + 6.0), pos2(x, rect.bottom() - 6.0)],
                Stroke::new(if hot { 2.0 } else { 1.0 }, if hot { theme::pal(ui).accent } else { line_c }),
            );
            if resp.double_clicked() {
                colw = ColW::default();
                changed = true;
            } else if resp.dragged() {
                let dx = resp.drag_delta().x;
                let slot = match key {
                    "name" => {
                        let cur = colw.name.unwrap_or(r.width());
                        colw.name = Some(cur);
                        colw.name.as_mut().unwrap()
                    }
                    "folder" => &mut colw.folder,
                    "ext" => &mut colw.ext,
                    "size" => &mut colw.size,
                    _ => &mut colw.date,
                };
                *slot = (*slot + dx).max(40.0);
                changed = true;
            }
        }
        if changed {
            ui.ctx().data_mut(|d| d.insert_temp(colw_id(), colw));
        }
    }

    fn row_ui(&mut self, ui: &mut egui::Ui, id: usize, vi: usize, active: bool, out: &mut Vec<Action>) {
        let ei = self.view[vi];
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
        let is_cursor = vi == self.cursor;
        let is_sel = self.selected.contains(&ei);
        let pal = theme::pal(ui);
        let painter = ui.painter_at(rect);

        // Selection and hover are drawn as rounded pills, slightly inset from the pane edge.
        let pill = rect.shrink2(vec2(2.0, 1.0));
        if is_sel {
            painter.rect_filled(pill, 5.0, if active { pal.sel } else { pal.sel_inactive });
        } else if resp.hovered() {
            painter.rect_filled(pill, 5.0, pal.hover);
        }
        if is_cursor && active {
            painter.rect_stroke(pill, 5.0, Stroke::new(1.0, pal.accent.gamma_multiply(if is_sel { 0.9 } else { 0.55 })), egui::StrokeKind::Inside);
        }

        let e = &self.entries[ei];
        let cols = Columns::new(rect, self.search.is_some(), &load_colw(ui.ctx()));
        let font = FontId::proportional(14.0);
        let small = FontId::proportional(12.5);
        let text_c = if is_sel {
            pal.strong
        } else if e.hidden {
            pal.weak
        } else {
            pal.text
        };
        let weak = if is_sel && active { pal.text } else { pal.weak };

        let renaming = self.rename.as_ref().is_some_and(|r| r.path == e.path);
        paint_icon(&painter, pos2(cols.name.left() + 10.0, rect.center().y), e.is_dir, &e.ext, pal);
        if !renaming {
            let clip = painter.with_clip_rect(cols.name.shrink2(vec2(4.0, 0.0)));
            let mut name = e.name.clone();
            if e.is_link {
                name.push_str("  ↪");
            }
            let g = theme::elided(ui, &name, font.clone(), text_c, cols.name.width() - 42.0);
            theme::paint_left(&clip, g, pos2(cols.name.left() + 34.0, rect.center().y), text_c);
        }
        if let (Some(r), Some(rel)) = (cols.folder, &e.rel) {
            painter
                .with_clip_rect(r.shrink2(vec2(4.0, 0.0)))
                .text(pos2(r.left() + 6.0, rect.center().y), Align2::LEFT_CENTER, rel, small.clone(), weak);
        }
        let ext = if e.is_dir { "Folder".to_string() } else { e.ext.to_uppercase() };
        painter
            .with_clip_rect(cols.ext.shrink2(vec2(4.0, 0.0)))
            .text(pos2(cols.ext.left() + 6.0, rect.center().y), Align2::LEFT_CENTER, ext, small.clone(), weak);
        if !e.is_dir {
            painter.text(
                pos2(cols.size.right() - 6.0, rect.center().y),
                Align2::RIGHT_CENTER,
                fsx::human_size(e.size),
                small.clone(),
                weak,
            );
        }
        painter.text(
            pos2(cols.date.left() + 6.0, rect.center().y),
            Align2::LEFT_CENTER,
            fsx::format_time(e.modified),
            small,
            weak,
        );

        if renaming {
            let r = self.rename.as_mut().unwrap();
            let edit_rect = Rect::from_min_max(pos2(cols.name.left() + 30.0, rect.top() + 2.0), pos2(cols.name.right() - 4.0, rect.bottom() - 2.0));
            let te = ui.put(edit_rect, egui::TextEdit::singleline(&mut r.text).id(egui::Id::unique(("rename", id))).margin(Vec2::new(2.0, 1.0)));
            if r.focus {
                te.request_focus();
                // Pre-select the stem so typing replaces the name but keeps the extension.
                let stem = r.text.rsplit_once('.').map(|(s, _)| s.chars().count()).filter(|&n| n > 0 && !e.is_dir).unwrap_or(r.text.chars().count());
                if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), te.id) {
                    st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(stem),
                    )));
                    st.store(ui.ctx(), te.id);
                }
                r.focus = false;
            } else if te.lost_focus() {
                if ui.input(|i| i.key_pressed(Key::Escape)) {
                    self.rename = None;
                } else {
                    let r = self.rename.take().unwrap();
                    out.push(Action::CommitRename(id, r.path, r.text));
                }
            }
            return;
        }

        self.item_interact(ui, &resp, id, vi, ei, out);
    }

    /// Click, selection, open and context-menu handling shared by list rows and gallery tiles.
    fn item_interact(&mut self, ui: &mut egui::Ui, resp: &egui::Response, id: usize, vi: usize, ei: usize, out: &mut Vec<Action>) {
        let e = &self.entries[ei];
        let path = e.path.clone();
        let is_dir = e.is_dir;
        if resp.clicked() || resp.secondary_clicked() {
            out.push(Action::Activate(id));
            let m = ui.input(|i| i.modifiers);
            if resp.secondary_clicked() {
                if !self.selected.contains(&ei) {
                    self.selected.clear();
                    self.cursor = vi;
                    self.anchor = vi;
                }
            } else if m.command {
                if !self.selected.remove(&ei) {
                    self.selected.insert(ei);
                }
                self.cursor = vi;
                self.anchor = vi;
            } else if m.shift {
                self.move_cursor(vi as isize, true);
            } else {
                self.selected.clear();
                self.selected.insert(ei);
                self.cursor = vi;
                self.anchor = vi;
            }
        }
        if resp.middle_clicked() && is_dir {
            out.push(Action::OpenInNewTab(path.clone()));
        }
        if resp.double_clicked() {
            self.cursor = vi;
            out.push(Action::OpenCursor(id));
        }
        resp.context_menu(|ui| {
            if ui.button("Open\tEnter").clicked() {
                out.push(Action::OpenCursor(id));
            }
            if is_dir && ui.button(format!("Open in new tab\t{}", k("Ctrl+Enter"))).clicked() {
                out.push(Action::OpenInNewTab(path.clone()));
            }
            if is_dir && ui.button("Open in other pane").clicked() {
                out.push(Action::OpenInOther(id));
            }
            if is_dir && ui.button(favorite_label(ui, &path, false)).clicked() {
                out.push(Action::ToggleFavorite(path.clone()));
            }
            ui.separator();
            if ui.button(format!("Copy\t{}", k("Ctrl+C"))).clicked() {
                out.push(Action::Copy(id));
            }
            if ui.button(format!("Cut\t{}", k("Ctrl+X"))).clicked() {
                out.push(Action::Cut(id));
            }
            if ui.button(format!("Paste\t{}", k("Ctrl+V"))).clicked() {
                out.push(Action::Paste(id));
            }
            ui.separator();
            if ui.button("Rename\tF2").clicked() {
                out.push(Action::StartRename(id));
            }
            if ui.button(format!("Move to {}\t{}", crate::plat::TRASH, k(DELETE_KEY))).clicked() {
                out.push(Action::Delete(id, false));
            }
            ui.separator();
            if ui.button(format!("Copy path\t{}", k("Ctrl+Shift+C"))).clicked() {
                out.push(Action::CopyPath(id));
            }
            if ui.button(format!("Show in {}", crate::plat::FILE_BROWSER)).clicked() {
                out.push(Action::Reveal(path.clone()));
            }
        });
    }

    fn tile_ui(&mut self, ui: &mut egui::Ui, id: usize, vi: usize, active: bool, thumbs: &mut Thumbs, out: &mut Vec<Action>) {
        let ei = self.view[vi];
        let (rect, resp) = ui.allocate_exact_size(vec2(TILE_W, TILE_H), Sense::click());
        let is_cursor = vi == self.cursor;
        let is_sel = self.selected.contains(&ei);
        let pal = theme::pal(ui);
        let painter = ui.painter_at(rect);
        let inner = rect.shrink(4.0);
        if is_sel {
            painter.rect_filled(inner, 8.0, if active { pal.sel } else { pal.sel_inactive });
        } else if resp.hovered() {
            painter.rect_filled(inner, 8.0, pal.hover);
        }
        if is_cursor && active {
            painter.rect_stroke(inner, 8.0, Stroke::new(1.0, pal.accent.gamma_multiply(if is_sel { 0.9 } else { 0.55 })), egui::StrokeKind::Inside);
        }

        let e = &self.entries[ei];
        let img_area = Rect::from_min_max(inner.min + vec2(8.0, 8.0), pos2(inner.max.x - 8.0, inner.min.y + 104.0));
        let mut drew = false;
        if !e.is_dir && crate::thumbs::is_image(&e.ext) {
            if let Some(tex) = thumbs.get(&e.path) {
                let size = tex.size_vec2();
                let scale = (img_area.width() / size.x).min(img_area.height() / size.y).min(1.0);
                let r = Rect::from_center_size(img_area.center(), size * scale);
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                painter.add(egui::epaint::RectShape::filled(r, 4.0, Color32::WHITE).with_texture(tex.id(), uv));
                painter.rect_stroke(r, 4.0, Stroke::new(1.0, pal.border), egui::StrokeKind::Outside);
                drew = true;
            }
        }
        if !drew {
            let c = img_area.center();
            paint_icon_scaled(&painter, pos2(c.x - 8.0 * 3.2, c.y), e.is_dir, &e.ext, pal, 3.2);
        }
        let text_c = if is_sel { pal.strong } else if e.hidden { pal.weak } else { pal.text };
        let name_rect = Rect::from_min_max(pos2(inner.left() + 4.0, inner.top() + 110.0), pos2(inner.right() - 4.0, inner.bottom() - 2.0));
        let galley = ui.fonts_mut(|f| {
            let mut job = egui::text::LayoutJob::simple(e.name.clone(), FontId::proportional(12.5), text_c, name_rect.width());
            job.wrap.max_rows = 2;
            job.wrap.break_anywhere = true;
            job.halign = egui::Align::Center;
            f.layout_job(job)
        });
        painter.with_clip_rect(name_rect).galley(pos2(name_rect.center().x, name_rect.top()), galley, text_c);

        self.item_interact(ui, &resp, id, vi, ei, out);
    }
}

/// Small vector file icons: crisp at any DPI and no dependency on emoji fonts.
fn paint_icon(p: &egui::Painter, left_center: egui::Pos2, is_dir: bool, ext: &str, pal: &theme::Pal) {
    paint_icon_scaled(p, left_center, is_dir, ext, pal, 1.0);
}

fn ext_color(ext: &str, pal: &theme::Pal) -> Color32 {
    match ext {
        "pdf" => Color32::from_rgb(0xE5, 0x5F, 0x55),
        "doc" | "docx" | "txt" | "md" | "rtf" => Color32::from_rgb(0x5B, 0x93, 0xE6),
        "xls" | "xlsx" | "csv" => Color32::from_rgb(0x41, 0xAE, 0x70),
        "ppt" | "pptx" => Color32::from_rgb(0xE6, 0x8A, 0x45),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "heic" | "svg" | "tif" | "tiff" | "ico" => Color32::from_rgb(0xAD, 0x72, 0xDE),
        "mp3" | "wav" | "flac" | "m4a" | "mp4" | "mov" | "mkv" | "avi" => Color32::from_rgb(0xDD, 0x6F, 0xAD),
        "zip" | "7z" | "rar" | "lzh" | "tar" | "gz" => Color32::from_rgb(0xB5, 0x92, 0x5B),
        "exe" | "msi" | "bat" | "cmd" | "ps1" | "sh" | "app" | "dmg" => Color32::from_rgb(0x55, 0xB5, 0xC6),
        _ => pal.weak,
    }
}

/// `left_center` is the middle of the icon's left edge; the icon is 16*k wide.
fn paint_icon_scaled(p: &egui::Painter, left_center: egui::Pos2, is_dir: bool, ext: &str, pal: &theme::Pal, k: f32) {
    let x = left_center.x;
    let y = left_center.y;
    let at = |dx: f32, dy: f32| pos2(x + dx * k, y + dy * k);
    if is_dir {
        // Two-tone folder: darker back with a tab, lighter front flap.
        p.rect_filled(Rect::from_min_max(at(0.0, -6.5), at(7.0, -3.0)), 1.5 * k, pal.folder_back);
        p.rect_filled(Rect::from_min_max(at(0.0, -5.0), at(16.0, 6.5)), 2.0 * k, pal.folder_back);
        p.rect_filled(Rect::from_min_max(at(0.0, -2.5), at(16.0, 6.5)), 2.0 * k, pal.folder);
    } else {
        // A sheet with a folded corner, tinted by file type.
        let c = ext_color(ext, pal);
        let outline = vec![at(2.5, -7.5), at(9.5, -7.5), at(13.5, -3.5), at(13.5, 7.5), at(2.5, 7.5)];
        p.add(egui::Shape::convex_polygon(outline, c.gamma_multiply(0.22), Stroke::new(1.1 * k.sqrt(), c)));
        p.line(vec![at(9.5, -7.5), at(9.5, -3.5), at(13.5, -3.5)], Stroke::new(1.1 * k.sqrt(), c));
        if k > 1.5 {
            let st = Stroke::new(1.1 * k.sqrt(), c.gamma_multiply(0.8));
            p.line_segment([at(5.0, 0.5), at(11.0, 0.5)], st);
            p.line_segment([at(5.0, 3.5), at(11.0, 3.5)], st);
        }
    }
}

/// Column widths, shared by every pane and tab (kept in egui's temp memory).
#[derive(Clone, Copy)]
struct ColW {
    /// `None`: the Name column takes whatever the fixed columns leave.
    name: Option<f32>,
    folder: f32,
    ext: f32,
    size: f32,
    date: f32,
}

impl Default for ColW {
    fn default() -> Self {
        ColW { name: None, folder: 220.0, ext: 70.0, size: 84.0, date: 150.0 }
    }
}

fn colw_id() -> egui::Id {
    egui::Id::unique("8823-column-widths")
}

fn load_colw(ctx: &egui::Context) -> ColW {
    ctx.data(|d| d.get_temp(colw_id())).unwrap_or_default()
}

struct Columns {
    name: Rect,
    folder: Option<Rect>,
    ext: Rect,
    size: Rect,
    date: Rect,
}

impl Columns {
    fn new(r: Rect, search: bool, w: &ColW) -> Self {
        let folder_w = if search { w.folder } else { 0.0 };
        let fixed = w.ext + w.size + w.date + folder_w;
        let name_w = w.name.unwrap_or((r.width() - fixed).max(120.0));
        let mut x = r.left();
        let mut take = |w: f32| {
            let c = Rect::from_min_max(pos2(x, r.top()), pos2(x + w, r.bottom()));
            x += w;
            c
        };
        let name = take(name_w);
        let folder = if search { Some(take(folder_w)) } else { None };
        let ext = take(w.ext);
        let size = take(w.size);
        let date = take(w.date);
        Columns { name, folder, ext, size, date }
    }
}
