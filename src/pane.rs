//! A single file-list pane: its state, navigation history, selection and rendering.

use crate::fsx::{self, Entry, SortKey};
use crate::text::fold;
use eframe::egui::{self, Align2, Color32, FontId, Key, Rect, Sense, Stroke, Vec2, pos2, vec2};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub const ROW_H: f32 = 22.0;

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

    pub fn page_rows(&self) -> isize {
        ((self.view_h / ROW_H) as isize - 1).max(1)
    }

    /// Keyboard handling for the active pane when no text field has focus.
    pub fn keys(&mut self, ctx: &egui::Context, id: usize, out: &mut Vec<Action>) {
        let (ev, m) = ctx.input(|i| {
            let keys: Vec<Key> = i
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key { key, pressed: true, .. } => Some(*key),
                    _ => None,
                })
                .collect();
            (keys, i.modifiers)
        });
        let c = self.cursor as isize;
        for k in ev {
            match k {
                Key::ArrowDown if !m.alt => self.move_cursor(c + 1, m.shift),
                Key::ArrowUp if m.alt => out.push(Action::Up(id)),
                Key::ArrowUp => self.move_cursor(c - 1, m.shift),
                Key::PageDown => self.move_cursor(c + self.page_rows(), m.shift),
                Key::PageUp => self.move_cursor(c - self.page_rows(), m.shift),
                Key::Home => self.move_cursor(0, m.shift),
                Key::End => self.move_cursor(isize::MAX / 2, m.shift),
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

    pub fn ui(&mut self, ui: &mut egui::Ui, id: usize, active: bool, show_hidden: bool, out: &mut Vec<Action>) {
        let accent = ui.visuals().selection.bg_fill;
        self.header_ui(ui, id, show_hidden, out);
        ui.add_space(2.0);
        self.columns_ui(ui, show_hidden);

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
        if self.scroll_to_cursor {
            let top = self.cursor as f32 * ROW_H;
            let mut y = self.scroll_y;
            if top < y {
                y = top;
            } else if top + ROW_H > y + self.view_h {
                y = top + ROW_H - self.view_h;
            }
            scroll = scroll.vertical_scroll_offset(y.max(0.0));
            self.scroll_to_cursor = false;
        }

        ui.spacing_mut().item_spacing.y = 0.0;
        let n = self.view.len();
        let output = scroll.show_rows(ui, ROW_H, n, |ui, range| {
            for vi in range {
                self.row_ui(ui, id, vi, active, accent, out);
            }
        });
        self.scroll_y = output.state.clamped_offset().y;
        self.view_h = output.inner_rect.height();

        // Click on empty space below the rows: activate pane and offer paste/new-folder.
        let bg = ui.interact(output.inner_rect, ui.scope_id().with(("bg", id)), Sense::click());
        if bg.clicked() {
            out.push(Action::Activate(id));
            self.selected.clear();
        }
        bg.context_menu(|ui| {
            if ui.button("Paste\tCtrl+V").clicked() {
                out.push(Action::Paste(id));
            }
            if ui.button("New folder\tCtrl+Shift+N").clicked() {
                out.push(Action::NewFolder(id));
            }
            if ui.button("Refresh\tF5").clicked() {
                out.push(Action::Refresh(id));
            }
            if ui.button("Open terminal here").clicked() {
                out.push(Action::Terminal(self.path.clone()));
            }
        });
    }

    fn header_ui(&mut self, ui: &mut egui::Ui, id: usize, show_hidden: bool, out: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.back.is_empty(), egui::Button::new("◀")).on_hover_text("Back (Alt+←)").clicked() {
                out.push(Action::Back(id));
            }
            if ui.add_enabled(!self.fwd.is_empty(), egui::Button::new("▶")).on_hover_text("Forward (Alt+→)").clicked() {
                out.push(Action::Forward(id));
            }
            if ui.button("▲").on_hover_text("Parent folder (Backspace)").clicked() {
                out.push(Action::Up(id));
            }

            // Filter box on the right, path in the remaining space.
            let filter_w = 170.0;
            let path_w = (ui.available_width() - filter_w - 8.0).max(80.0);

            if let Some(text) = &mut self.path_edit {
                let resp = ui.add_sized([path_w, 20.0], egui::TextEdit::singleline(text).id(egui::Id::unique(("path", id))));
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
                ui.allocate_ui(vec2(path_w, 20.0), |ui| {
                    ui.set_width(path_w);
                    egui::ScrollArea::horizontal()
                        .id_salt(("crumbs", id))
                        .stick_to_right(true)
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                let parts: Vec<&Path> = self.path.ancestors().collect();
                                for (k, p) in parts.iter().rev().enumerate() {
                                    let label = p
                                        .file_name()
                                        .map(|s| s.to_string_lossy().into_owned())
                                        .unwrap_or_else(|| p.display().to_string());
                                    if k > 0 {
                                        ui.weak(" › ");
                                    }
                                    let r = ui.add(egui::Button::new(label).frame(false));
                                    if r.clicked() {
                                        out.push(Action::Navigate(id, p.to_path_buf()));
                                    }
                                }
                                let rest = ui.available_rect_before_wrap();
                                if ui.interact(rest, ui.scope_id().with("crumb-empty"), Sense::click()).clicked() {
                                    self.path_edit = Some(self.path.display().to_string());
                                }
                            });
                        });
                });
            }

            let fid = egui::Id::unique(("filter", id));
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .id(fid)
                    .desired_width(filter_w)
                    .hint_text("Filter (Ctrl+F)"),
            );
            if self.focus_filter {
                resp.request_focus();
                self.focus_filter = false;
            }
            if resp.changed() {
                self.rebuild_view(show_hidden);
                self.cursor = 0;
                self.scroll_to_cursor = true;
            }
            if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                out.push(Action::Activate(id));
            }
            if resp.gained_focus() {
                out.push(Action::Activate(id));
            }
        });
    }

    fn columns_ui(&mut self, ui: &mut egui::Ui, show_hidden: bool) {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
        let cols = Columns::new(rect, self.search.is_some());
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
            let color = if resp.hovered() { ui.visuals().strong_text_color() } else { ui.visuals().weak_text_color() };
            let (pos, align) = if right {
                (pos2(r.right() - 6.0, r.center().y), Align2::RIGHT_CENTER)
            } else {
                (pos2(r.left() + 6.0, r.center().y), Align2::LEFT_CENTER)
            };
            ui.painter().text(pos, align, text, FontId::proportional(12.5), color);
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
    }

    fn row_ui(&mut self, ui: &mut egui::Ui, id: usize, vi: usize, active: bool, accent: Color32, out: &mut Vec<Action>) {
        let ei = self.view[vi];
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
        let is_cursor = vi == self.cursor;
        let is_sel = self.selected.contains(&ei);
        let v = ui.visuals();
        let painter = ui.painter_at(rect);

        if is_sel {
            painter.rect_filled(rect, 0.0, if active { accent } else { accent.gamma_multiply(0.45) });
        } else if resp.hovered() {
            painter.rect_filled(rect, 0.0, v.widgets.hovered.weak_bg_fill.gamma_multiply(0.6));
        } else if vi % 2 == 1 {
            painter.rect_filled(rect, 0.0, v.faint_bg_color);
        }
        if is_cursor {
            let c = if active { v.strong_text_color() } else { v.weak_text_color() };
            painter.rect_stroke(rect.shrink(0.5), 2.0, Stroke::new(1.0, c), egui::StrokeKind::Inside);
        }

        let e = &self.entries[ei];
        let cols = Columns::new(rect, self.search.is_some());
        let font = FontId::proportional(14.0);
        let small = FontId::proportional(13.0);
        let text_c = if e.hidden { v.weak_text_color() } else { v.text_color() };
        let weak = v.weak_text_color();

        let renaming = self.rename.as_ref().is_some_and(|r| r.path == e.path);
        paint_icon(&painter, pos2(cols.name.left() + 8.0, rect.center().y), e.is_dir, &e.ext, weak);
        if !renaming {
            let clip = painter.with_clip_rect(cols.name.shrink2(vec2(4.0, 0.0)));
            let mut name = e.name.clone();
            if e.is_link {
                name.push_str("  ↪");
            }
            clip.text(pos2(cols.name.left() + 28.0, rect.center().y), Align2::LEFT_CENTER, name, font.clone(), text_c);
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
            let edit_rect = Rect::from_min_max(pos2(cols.name.left() + 26.0, rect.top() + 1.0), pos2(cols.name.right() - 4.0, rect.bottom() - 1.0));
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
        if resp.double_clicked() {
            self.cursor = vi;
            out.push(Action::OpenCursor(id));
        }
        resp.context_menu(|ui| {
            if ui.button("Open\tEnter").clicked() {
                out.push(Action::OpenCursor(id));
            }
            if is_dir && ui.button("Open in other pane").clicked() {
                out.push(Action::OpenInOther(id));
            }
            ui.separator();
            if ui.button("Copy\tCtrl+C").clicked() {
                out.push(Action::Copy(id));
            }
            if ui.button("Cut\tCtrl+X").clicked() {
                out.push(Action::Cut(id));
            }
            if ui.button("Paste\tCtrl+V").clicked() {
                out.push(Action::Paste(id));
            }
            ui.separator();
            if ui.button("Rename\tF2").clicked() {
                out.push(Action::StartRename(id));
            }
            if ui.button("Delete\tDel").clicked() {
                out.push(Action::Delete(id, false));
            }
            ui.separator();
            if ui.button("Copy path\tCtrl+Shift+C").clicked() {
                out.push(Action::CopyPath(id));
            }
            if ui.button("Show in Explorer").clicked() {
                out.push(Action::Reveal(path.clone()));
            }
        });
    }
}

/// Small vector icons: crisp at any DPI and no dependency on emoji fonts.
fn paint_icon(p: &egui::Painter, left_center: egui::Pos2, is_dir: bool, ext: &str, weak: Color32) {
    let x = left_center.x;
    let y = left_center.y;
    if is_dir {
        let c = Color32::from_rgb(0xE8, 0xB3, 0x3C);
        let tab = Rect::from_min_max(pos2(x, y - 6.0), pos2(x + 7.0, y - 3.0));
        let body = Rect::from_min_max(pos2(x, y - 4.0), pos2(x + 16.0, y + 6.0));
        p.rect_filled(tab, 1.0, c);
        p.rect_filled(body, 1.5, c);
    } else {
        let c = match ext {
            "pdf" => Color32::from_rgb(0xE0, 0x5A, 0x4F),
            "doc" | "docx" | "txt" | "md" | "rtf" => Color32::from_rgb(0x4F, 0x8B, 0xE0),
            "xls" | "xlsx" | "csv" => Color32::from_rgb(0x3F, 0xA9, 0x6B),
            "ppt" | "pptx" => Color32::from_rgb(0xE0, 0x86, 0x3F),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "heic" | "svg" => Color32::from_rgb(0xA8, 0x6B, 0xD8),
            "mp3" | "wav" | "flac" | "m4a" | "mp4" | "mov" | "mkv" | "avi" => Color32::from_rgb(0xD8, 0x6B, 0xA8),
            "zip" | "7z" | "rar" | "lzh" | "tar" | "gz" => Color32::from_rgb(0xB0, 0x8D, 0x57),
            "exe" | "msi" | "bat" | "cmd" | "ps1" => Color32::from_rgb(0x5F, 0xB8, 0xC8),
            _ => weak,
        };
        let r = Rect::from_min_max(pos2(x + 2.0, y - 7.0), pos2(x + 13.0, y + 7.0));
        p.rect_stroke(r, 1.5, Stroke::new(1.2, c), egui::StrokeKind::Inside);
        p.rect_filled(Rect::from_min_max(pos2(x + 4.5, y + 1.0), pos2(x + 10.5, y + 4.5)), 0.5, c);
    }
}

struct Columns {
    name: Rect,
    folder: Option<Rect>,
    ext: Rect,
    size: Rect,
    date: Rect,
}

impl Columns {
    fn new(r: Rect, search: bool) -> Self {
        let (date_w, size_w, ext_w) = (130.0, 80.0, 64.0);
        let fixed = date_w + size_w + ext_w;
        let flex = (r.width() - fixed).max(120.0);
        let (name_w, folder_w) = if search { (flex * 0.55, flex * 0.45) } else { (flex, 0.0) };
        let mut x = r.left();
        let mut take = |w: f32| {
            let c = Rect::from_min_max(pos2(x, r.top()), pos2(x + w, r.bottom()));
            x += w;
            c
        };
        let name = take(name_w);
        let folder = if search { Some(take(folder_w)) } else { None };
        let ext = take(ext_w);
        let size = take(size_w);
        let date = take(date_w);
        Columns { name, folder, ext, size, date }
    }
}
