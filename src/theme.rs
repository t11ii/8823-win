//! Look and feel: colour palettes (dark and light, following the system setting),
//! spacing, and the small vector glyphs used in the toolbar and sidebar.

use eframe::egui::{self, Color32, CornerRadius, Painter, Pos2, Rect, Stroke, Theme, pos2, vec2};

pub struct Pal {
    /// Main content background.
    pub bg: Color32,
    /// Sidebar / status bar background.
    pub panel: Color32,
    /// Raised surfaces: address bar, filter box, selected tab.
    pub surface: Color32,
    pub border: Color32,
    pub text: Color32,
    pub weak: Color32,
    pub strong: Color32,
    pub accent: Color32,
    /// Selected row / tile background, and its dimmer version for an inactive pane.
    pub sel: Color32,
    pub sel_inactive: Color32,
    pub hover: Color32,
    pub stripe: Color32,
    pub folder: Color32,
    pub folder_back: Color32,
}

const fn rgb(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

pub const DARK: Pal = Pal {
    bg: rgb(0x17191F),
    panel: rgb(0x111318),
    surface: rgb(0x22262F),
    border: rgb(0x2C313C),
    text: rgb(0xD5DAE3),
    weak: rgb(0x8A93A3),
    strong: rgb(0xF4F6FA),
    accent: rgb(0x6AA5FF),
    sel: rgb(0x26406B),
    sel_inactive: rgb(0x262C39),
    hover: rgb(0x1F232C),
    stripe: rgb(0x1A1C23),
    folder: rgb(0xEDBB55),
    folder_back: rgb(0xC9952F),
};

pub const LIGHT: Pal = Pal {
    bg: rgb(0xFFFFFF),
    panel: rgb(0xF3F4F7),
    surface: rgb(0xE9ECF1),
    border: rgb(0xD9DDE5),
    text: rgb(0x232833),
    weak: rgb(0x6C7482),
    strong: rgb(0x0E1218),
    accent: rgb(0x2F6FE4),
    sel: rgb(0xD6E4FF),
    sel_inactive: rgb(0xE9ECF1),
    hover: rgb(0xF0F2F6),
    stripe: rgb(0xFAFBFC),
    folder: rgb(0xF0B840),
    folder_back: rgb(0xD39A22),
};

pub fn pal(ui: &egui::Ui) -> &'static Pal {
    if ui.visuals().dark_mode { &DARK } else { &LIGHT }
}

fn visuals(p: &Pal, dark: bool) -> egui::Visuals {
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = p.bg;
    v.window_fill = if dark { p.surface } else { p.bg };
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.extreme_bg_color = if dark { p.panel } else { p.bg };
    v.text_edit_bg_color = Some(if dark { p.panel } else { p.bg });
    v.faint_bg_color = p.stripe;
    v.code_bg_color = p.surface;
    v.hyperlink_color = p.accent;
    v.weak_text_color = Some(p.weak);
    v.selection.bg_fill = p.sel;
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.striped = false;

    let r = CornerRadius::same(6);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.bg;
    w.noninteractive.weak_bg_fill = p.bg;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = r;

    w.inactive.bg_fill = p.surface;
    w.inactive.weak_bg_fill = p.surface;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = r;

    w.hovered.bg_fill = p.border;
    w.hovered.weak_bg_fill = p.border;
    w.hovered.bg_stroke = Stroke::NONE;
    w.hovered.fg_stroke = Stroke::new(1.2, p.strong);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.sel;
    w.active.weak_bg_fill = p.sel;
    w.active.bg_stroke = Stroke::NONE;
    w.active.fg_stroke = Stroke::new(1.2, p.strong);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;

    w.open.bg_fill = p.surface;
    w.open.weak_bg_fill = p.surface;
    w.open.bg_stroke = Stroke::new(1.0, p.border);
    w.open.fg_stroke = Stroke::new(1.0, p.strong);
    w.open.corner_radius = r;
    v
}

pub fn install(ctx: &egui::Context) {
    // Follows the system light/dark setting; THEME_8823=light|dark forces one.
    match std::env::var("THEME_8823").as_deref() {
        Ok("light") => ctx.set_theme(egui::ThemePreference::Light),
        Ok("dark") => ctx.set_theme(egui::ThemePreference::Dark),
        _ => ctx.set_theme(egui::ThemePreference::System),
    }
    ctx.set_visuals_of(Theme::Dark, visuals(&DARK, true));
    ctx.set_visuals_of(Theme::Light, visuals(&LIGHT, false));
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = vec2(8.0, 4.0);
        s.spacing.button_padding = vec2(8.0, 4.0);
        s.spacing.menu_margin = egui::Margin::same(6);
        s.spacing.window_margin = egui::Margin::same(14);
        s.spacing.scroll.bar_width = 8.0;
        s.spacing.scroll.floating = true;
        s.interaction.selectable_labels = false;
    });
}

// ---------------------------------------------------------------------------
// Glyphs: 16px line icons drawn with the painter, so they stay crisp at any scale.

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    Back,
    Forward,
    Up,
    Search,
    Home,
    Desktop,
    Doc,
    Download,
    Picture,
    Music,
    Video,
    Apps,
    Cloud,
    Drive,
    Clock,
    Folder,
}

pub fn glyph(p: &Painter, g: Glyph, c: Pos2, color: Color32) {
    let s = Stroke::new(1.4, color);
    let at = |dx: f32, dy: f32| pos2(c.x + dx, c.y + dy);
    let line = |pts: &[(f32, f32)]| {
        p.line(pts.iter().map(|&(x, y)| at(x, y)).collect(), s);
    };
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32, r: f32| {
        p.rect_stroke(Rect::from_min_max(at(x0, y0), at(x1, y1)), r, s, egui::StrokeKind::Middle);
    };
    match g {
        Glyph::Back => line(&[(2.5, -5.0), (-2.5, 0.0), (2.5, 5.0)]),
        Glyph::Forward => line(&[(-2.5, -5.0), (2.5, 0.0), (-2.5, 5.0)]),
        Glyph::Up => {
            line(&[(0.0, 5.5), (0.0, -5.0)]);
            line(&[(-4.5, -0.5), (0.0, -5.0), (4.5, -0.5)]);
        }
        Glyph::Search => {
            p.circle_stroke(at(-1.5, -1.5), 4.2, s);
            line(&[(1.6, 1.6), (5.5, 5.5)]);
        }
        Glyph::Home => {
            line(&[(-6.5, -0.5), (0.0, -6.5), (6.5, -0.5)]);
            line(&[(-4.5, -1.5), (-4.5, 6.0), (4.5, 6.0), (4.5, -1.5)]);
        }
        Glyph::Desktop => {
            rect(-6.5, -5.5, 6.5, 3.0, 1.5);
            line(&[(-3.0, 6.0), (3.0, 6.0)]);
            line(&[(0.0, 3.0), (0.0, 6.0)]);
        }
        Glyph::Doc => {
            line(&[(1.5, -6.5), (-4.5, -6.5), (-4.5, 6.5), (4.5, 6.5), (4.5, -3.5), (1.5, -6.5), (1.5, -3.5), (4.5, -3.5)]);
            line(&[(-2.0, 0.0), (2.0, 0.0)]);
            line(&[(-2.0, 3.0), (2.0, 3.0)]);
        }
        Glyph::Download => {
            line(&[(0.0, -6.5), (0.0, 2.0)]);
            line(&[(-3.5, -1.5), (0.0, 2.0), (3.5, -1.5)]);
            line(&[(-6.0, 3.0), (-6.0, 6.0), (6.0, 6.0), (6.0, 3.0)]);
        }
        Glyph::Picture => {
            rect(-6.5, -5.5, 6.5, 5.5, 1.5);
            p.circle_filled(at(-3.0, -2.0), 1.3, color);
            line(&[(-6.0, 4.5), (-1.5, 0.5), (1.0, 3.0), (3.0, 1.0), (6.0, 4.0)]);
        }
        Glyph::Music => {
            p.circle_stroke(at(-3.0, 4.0), 2.2, s);
            line(&[(-0.8, 4.0), (-0.8, -6.0), (5.0, -4.5), (5.0, -1.5)]);
        }
        Glyph::Video => {
            rect(-6.5, -5.0, 6.5, 5.0, 2.0);
            p.add(egui::Shape::convex_polygon(vec![at(-1.8, -2.8), at(3.0, 0.0), at(-1.8, 2.8)], color, Stroke::NONE));
        }
        Glyph::Apps => {
            for (x, y) in [(-6.0, -6.0), (1.0, -6.0), (-6.0, 1.0), (1.0, 1.0)] {
                rect(x, y, x + 5.0, y + 5.0, 1.2);
            }
        }
        Glyph::Cloud => {
            p.rect_filled(Rect::from_min_max(at(-6.5, 0.0), at(6.5, 5.5)), 2.75, color);
            p.circle_filled(at(-2.0, -0.5), 3.6, color);
            p.circle_filled(at(2.8, 0.5), 2.6, color);
        }
        Glyph::Drive => {
            rect(-6.5, -4.0, 6.5, 4.0, 2.0);
            p.circle_filled(at(3.5, 0.0), 1.2, color);
            line(&[(-4.0, 0.0), (0.0, 0.0)]);
        }
        Glyph::Clock => {
            p.circle_stroke(c, 6.0, s);
            line(&[(0.0, -3.5), (0.0, 0.0), (2.6, 1.6)]);
        }
        Glyph::Folder => {
            line(&[(-6.5, 5.0), (-6.5, -5.0), (-2.0, -5.0), (0.0, -3.0), (6.5, -3.0), (6.5, 5.0), (-6.5, 5.0)]);
        }
    }
}

/// A square toolbar button showing a glyph. Returns true when clicked.
pub fn glyph_button(ui: &mut egui::Ui, g: Glyph, enabled: bool, tip: &str) -> bool {
    let pal = pal(ui);
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), sense);
    let color = if !enabled {
        pal.weak.gamma_multiply(0.45)
    } else if resp.hovered() {
        pal.strong
    } else {
        pal.text
    };
    if enabled && resp.hovered() {
        ui.painter().rect_filled(rect, 6.0, pal.surface);
    }
    glyph(ui.painter(), g, rect.center(), color);
    resp.on_hover_text(tip).clicked()
}

/// Lay out one line of text, shortened with "…" when it is wider than `max_w`.
pub fn elided(ui: &egui::Ui, text: &str, font: egui::FontId, color: Color32, max_w: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap.max_width = max_w.max(8.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    ui.fonts_mut(|f| f.layout_job(job))
}

/// Paint a galley vertically centred at `left_center`.
pub fn paint_left(p: &Painter, galley: std::sync::Arc<egui::Galley>, left_center: Pos2, color: Color32) {
    let y = left_center.y - galley.size().y / 2.0;
    p.galley(pos2(left_center.x, y), galley, color);
}
