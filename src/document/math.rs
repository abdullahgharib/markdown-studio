use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use eframe::egui;
use egui::epaint::text::{FontDefinitions, FontFamily, FontId, Fonts, LayoutJob, TextFormat};
use egui::{Color32, FontData, Galley, Id, Pos2, Response, Sense, Stroke, Vec2};
use rustc_hash::{FxHashMap, FxHasher};

use crate::ui::theme::WordTheme;

/// Categorized Greek symbol palette `(glyph, LaTeX snippet)` for the Equations Ribbon.
pub const MATH_GREEK_SYMBOLS: &[(&str, &str)] = &[
    ("α", r"\alpha"),
    ("β", r"\beta"),
    ("γ", r"\gamma"),
    ("δ", r"\delta"),
    ("ε", r"\varepsilon"),
    ("ζ", r"\zeta"),
    ("η", r"\eta"),
    ("θ", r"\theta"),
    ("λ", r"\lambda"),
    ("μ", r"\mu"),
    ("ν", r"\nu"),
    ("ξ", r"\xi"),
    ("π", r"\pi"),
    ("ρ", r"\rho"),
    ("σ", r"\sigma"),
    ("τ", r"\tau"),
    ("φ", r"\varphi"),
    ("χ", r"\chi"),
    ("ψ", r"\psi"),
    ("ω", r"\omega"),
    ("Γ", r"\Gamma"),
    ("Δ", r"\Delta"),
    ("Θ", r"\Theta"),
    ("Λ", r"\Lambda"),
    ("Σ", r"\Sigma"),
    ("Φ", r"\Phi"),
    ("Ψ", r"\Psi"),
    ("Ω", r"\Omega"),
];

/// Categorized math operators `(glyph, LaTeX snippet)` for the Equations Ribbon.
pub const MATH_OPERATOR_SYMBOLS: &[(&str, &str)] = &[
    ("±", r"\pm"),
    ("∓", r"\mp"),
    ("×", r"\times"),
    ("÷", r"\div"),
    ("⋅", r"\cdot"),
    ("∘", r"\circ"),
    ("⊕", r"\oplus"),
    ("⊗", r"\otimes"),
    ("∑", r"\sum_{i=1}^{n}"),
    ("∏", r"\prod_{i=1}^{n}"),
    ("∫", r"\int_{a}^{b}"),
    ("∬", r"\iint"),
    ("∮", r"\oint"),
    ("∂", r"\partial"),
    ("∇", r"\nabla"),
    ("∞", r"\infty"),
    ("°", r"^{\circ}"),
    ("′", "'"),
];

/// Categorized math relations, set & logic symbols `(glyph, LaTeX snippet)`.
pub const MATH_RELATION_SYMBOLS: &[(&str, &str)] = &[
    ("=", "="),
    ("≠", r"\neq"),
    ("≈", r"\approx"),
    ("≡", r"\equiv"),
    ("≤", r"\le"),
    ("≥", r"\ge"),
    ("≪", r"\ll"),
    ("≫", r"\gg"),
    ("∝", r"\propto"),
    ("∈", r"\in"),
    ("∉", r"\notin"),
    ("⊂", r"\subset"),
    ("⊆", r"\subseteq"),
    ("∪", r"\cup"),
    ("∩", r"\cap"),
    ("∀", r"\forall"),
    ("∃", r"\exists"),
    ("→", r"\to"),
    ("⇒", r"\Rightarrow"),
    ("⇔", r"\Leftrightarrow"),
    ("∴", r"\therefore"),
];

/// Categorized accents, blackboard sets & boxed snippets `(label, LaTeX snippet)`.
pub const MATH_ACCENT_SNIPPETS: &[(&str, &str)] = &[
    ("x̂ Hat", r"\hat{x}"),
    ("x̄ Bar", r"\bar{x}"),
    ("x⃗ Vec", r"\vec{x}"),
    ("x̃ Tilde", r"\tilde{x}"),
    ("ẋ Dot", r"\dot{x}"),
    ("ẍ DDot", r"\ddot{x}"),
    ("Overline", r"\overline{AB}"),
    ("Underline", r"\underline{x}"),
    ("ℝ Real", r"\mathbb{R}"),
    ("ℕ Nat", r"\mathbb{N}"),
    ("ℤ Int", r"\mathbb{Z}"),
    ("ℂ Complex", r"\mathbb{C}"),
    ("Boxed", r"\boxed{x}"),
];

/// Built-in textbook equation templates `(title, LaTeX source)`.
pub const MATH_TEXTBOOK_TEMPLATES: &[(&str, &str)] = &[
    ("Quadratic Formula", r"x = \frac{-b \pm \sqrt{b^2 - 4ac}}{2a}"),
    ("Gaussian Integral", r"\int_{-\infty}^{\infty} e^{-x^2}\,dx = \sqrt{\pi}"),
    ("Euler's Identity", r"e^{i\pi} + 1 = 0"),
    ("Taylor Series", r"f(x) = \sum_{n=0}^{\infty} \frac{f^{(n)}(a)}{n!}(x - a)^n"),
    ("Derivative Limit", r"f'(x) = \lim_{h \to 0} \frac{f(x + h) - f(x)}{h}"),
    ("2×2 Matrix", "A = \\begin{pmatrix} a & b \\\\ c & d \\end{pmatrix}"),
    ("Piecewise Cases", "f(x) = \\begin{cases} x^2 & x \\ge 0 \\\\ -x & x < 0 \\end{cases}"),
    ("Aligned Derivation", "(a + b)^2 &= (a + b)(a + b) \\\\\n&= a^2 + 2ab + b^2"),
    ("Pythagorean Calc", "a = 3\nb = 4\nc = \\sqrt{a^2 + b^2}"),
];

/// Font styling variant for a math atom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MathFontStyle {
    Normal,
    Roman,
    Bold,
    Italic,
    Blackboard,
    Calligraphic,
    Monospace,
    Operator,
}

/// TeX-like atom spacing class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MathAtomKind {
    Ord,
    Op,
    Rel,
    Open,
    Close,
    Punct,
}

/// Matrix / alignment environment type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatrixEnv {
    Plain,
    Paren,
    Bracket,
    Brace,
    VBar,
    DoubleVBar,
    Cases,
    Aligned,
}

/// Mathematical accent / decoration kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccentKind {
    Hat,
    Overline,
    Underline,
    Vector,
    LeftVector,
    Tilde,
    Dot,
    DDot,
}

/// Parsed LaTeX mathematical expression tree.
#[derive(Debug, Clone, PartialEq)]
pub enum MathNode {
    Empty,
    Row(Vec<MathNode>),
    Text {
        text: String,
        style: MathFontStyle,
        kind: MathAtomKind,
    },
    Space(f32),
    Frac {
        num: Box<MathNode>,
        den: Box<MathNode>,
        has_bar: bool,
    },
    Sqrt {
        degree: Option<Box<MathNode>>,
        radicand: Box<MathNode>,
    },
    Scripts {
        base: Box<MathNode>,
        sub: Option<Box<MathNode>>,
        sup: Option<Box<MathNode>>,
        limits: bool,
    },
    Delimited {
        left: Option<String>,
        right: Option<String>,
        body: Box<MathNode>,
    },
    Accent {
        kind: AccentKind,
        body: Box<MathNode>,
    },
    Boxed(Box<MathNode>),
    Matrix {
        env: MatrixEnv,
        rows: Vec<Vec<MathNode>>,
    },
}

/// Scalable delimiter bracket shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BracketKind {
    LeftParen,
    RightParen,
    LeftBracket,
    RightBracket,
    LeftBrace,
    RightBrace,
    VBar,
    DoubleVBar,
    LeftAngle,
    RightAngle,
    LeftFloor,
    RightFloor,
    LeftCeil,
    RightCeil,
}

/// Positioned drawing primitive produced by 2D mathematical typesetting.
/// Coordinates are in points relative to `(x = 0.0, baseline_y = 0.0)`,
/// where negative `y` extends upward and positive `y` extends downward.
#[derive(Debug, Clone, PartialEq)]
pub enum MathPrimitive {
    Text {
        x: f32,
        baseline_y: f32,
        text: String,
        font_size: f32,
        italic: bool,
        bold: bool,
        monospace: bool,
    },
    Line {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        stroke_width: f32,
    },
    Polyline {
        points: Vec<(f32, f32)>,
        stroke_width: f32,
    },
    FilledCircle {
        cx: f32,
        cy: f32,
        radius: f32,
    },
    Bracket {
        kind: BracketKind,
        x: f32,
        top_y: f32,
        bottom_y: f32,
        width: f32,
        stroke_width: f32,
    },
}

/// Complete 2D math box metrics and positioned drawing primitives.
#[derive(Debug, Clone)]
pub struct MathLayout {
    pub width: f32,
    pub ascent: f32,
    pub descent: f32,
    pub primitives: Vec<MathPrimitive>,
}

impl MathLayout {
    pub fn total_height(&self) -> f32 {
        self.ascent + self.descent
    }
}

// ---------------------------------------------------------------------------
// Shared Offscreen Font Atlas & Anti-Aliased Software RgbaCanvas
// ---------------------------------------------------------------------------

fn build_offscreen_font_definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let mut extra_fallbacks: Vec<String> = Vec::new();

    for path in [
        r"C:\Windows\Fonts\segoeui.ttf",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert("offscreen_regular".to_owned(), FontData::from_owned(bytes).into());
            if let Some(list) = fonts.families.get_mut(&FontFamily::Proportional) {
                list.insert(0, "offscreen_regular".to_owned());
            }
            break;
        }
    }

    for path in [
        r"C:\Windows\Fonts\tahoma.ttf",
        r"C:\Windows\Fonts\arial.ttf",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert("offscreen_unicode".to_owned(), FontData::from_owned(bytes).into());
            extra_fallbacks.push("offscreen_unicode".to_owned());
            break;
        }
    }

    for path in [
        r"C:\Windows\Fonts\seguisym.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert("offscreen_symbols".to_owned(), FontData::from_owned(bytes).into());
            extra_fallbacks.push("offscreen_symbols".to_owned());
            break;
        }
    }

    if let Some(list) = fonts.families.get_mut(&FontFamily::Proportional) {
        let insert_at = usize::from(!list.is_empty() && list[0] == "offscreen_regular");
        for (i, name) in extra_fallbacks.iter().enumerate() {
            list.insert(insert_at + i, name.clone());
        }
    }

    for path in [
        r"C:\Windows\Fonts\CascadiaMono.ttf",
        r"C:\Windows\Fonts\consola.ttf",
        "/System/Library/Fonts/Menlo.ttc",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert("offscreen_mono".to_owned(), FontData::from_owned(bytes).into());
            if let Some(list) = fonts.families.get_mut(&FontFamily::Monospace) {
                list.insert(0, "offscreen_mono".to_owned());
            }
            break;
        }
    }
    if let Some(list) = fonts.families.get_mut(&FontFamily::Monospace) {
        for name in &extra_fallbacks {
            if !list.contains(name) {
                list.push(name.clone());
            }
        }
    }

    let prop_chain = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mut bold_chain = Vec::with_capacity(1 + prop_chain.len());
    for path in [
        r"C:\Windows\Fonts\segoeuib.ttf",
        r"C:\Windows\Fonts\arialbd.ttf",
        "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert("offscreen_bold".to_owned(), FontData::from_owned(bytes).into());
            bold_chain.push("offscreen_bold".to_owned());
            break;
        }
    }
    bold_chain.extend(prop_chain);
    fonts
        .families
        .insert(FontFamily::Name("bold".into()), bold_chain);
    fonts
}

pub(crate) fn offscreen_fonts_2x() -> &'static Mutex<Fonts> {
    static OFFSCREEN_FONTS: OnceLock<Mutex<Fonts>> = OnceLock::new();
    OFFSCREEN_FONTS.get_or_init(|| {
        Mutex::new(Fonts::new(2.0, 4096, build_offscreen_font_definitions()))
    })
}

pub(crate) fn make_font_id(size: f32, bold: bool, monospace: bool) -> FontId {
    let family = if monospace {
        FontFamily::Monospace
    } else if bold {
        FontFamily::Name("bold".into())
    } else {
        FontFamily::Proportional
    };
    FontId::new(size.max(6.0), family)
}

pub(crate) fn measure_text_with_fonts(
    fonts: &Fonts,
    text: &str,
    font_size: f32,
    italic: bool,
    bold: bool,
    monospace: bool,
) -> (f32, f32, f32) {
    if text.is_empty() {
        return (0.0, font_size * 0.72, font_size * 0.20);
    }
    let font_id = make_font_id(font_size, bold, monospace);
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    job.append(
        text,
        0.0,
        TextFormat {
            font_id: font_id.clone(),
            color: Color32::BLACK,
            italics: italic,
            ..Default::default()
        },
    );
    let galley = fonts.layout_job(job);
    let w = galley.size().x + if italic { font_size * 0.10 } else { 0.0 };
    let (asc, desc) = if let Some(row) = galley.rows.first() {
        let mut max_asc = font_size * 0.72;
        let mut max_desc = font_size * 0.18;
        for g in &row.glyphs {
            max_asc = max_asc.max(g.font_ascent);
            max_desc = max_desc.max((g.line_height - g.font_ascent).max(font_size * 0.16));
        }
        (max_asc, max_desc)
    } else {
        (font_size * 0.74, font_size * 0.22)
    };
    (w, asc, desc)
}

/// Anti-aliased CPU RGBA canvas shared by Math and Mermaid PNG exporters.
pub(crate) struct RgbaCanvas {
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub data: Vec<u8>,
}

impl RgbaCanvas {
    pub fn new(logical_w: f32, logical_h: f32, scale: f32, bg: [u8; 4]) -> Self {
        let w = ((logical_w * scale).ceil() as u32).clamp(1, 8192);
        let h = ((logical_h * scale).ceil() as u32).clamp(1, 8192);
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            data.extend_from_slice(&bg);
        }
        Self {
            width: w,
            height: h,
            scale,
            data,
        }
    }

    #[inline]
    pub fn blend_px(&mut self, px: i32, py: i32, color: [u8; 4], cov: f32) {
        if px < 0 || py < 0 || px >= self.width as i32 || py >= self.height as i32 {
            return;
        }
        let alpha = (color[3] as f32 / 255.0) * cov.clamp(0.0, 1.0);
        if alpha <= 0.002 {
            return;
        }
        let idx = ((py as u32 * self.width + px as u32) * 4) as usize;
        let inv = 1.0 - alpha;
        self.data[idx] = (color[0] as f32 * alpha + self.data[idx] as f32 * inv)
            .round()
            .clamp(0.0, 255.0) as u8;
        self.data[idx + 1] = (color[1] as f32 * alpha + self.data[idx + 1] as f32 * inv)
            .round()
            .clamp(0.0, 255.0) as u8;
        self.data[idx + 2] = (color[2] as f32 * alpha + self.data[idx + 2] as f32 * inv)
            .round()
            .clamp(0.0, 255.0) as u8;
        let dst_a = self.data[idx + 3] as f32 / 255.0;
        let out_a = (alpha + dst_a * inv).clamp(0.0, 1.0);
        self.data[idx + 3] = (out_a * 255.0).round() as u8;
    }

    pub fn draw_line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, thickness: f32, color: [u8; 4]) {
        let ax = x0 * self.scale;
        let ay = y0 * self.scale;
        let bx = x1 * self.scale;
        let by = y1 * self.scale;
        let half_t = (thickness * self.scale * 0.5).max(0.45);
        let pad = half_t + 1.5;

        let min_x = ((ax.min(bx) - pad).floor() as i32).max(0);
        let max_x = ((ax.max(bx) + pad).ceil() as i32).min(self.width as i32 - 1);
        let min_y = ((ay.min(by) - pad).floor() as i32).max(0);
        let max_y = ((ay.max(by) + pad).ceil() as i32).min(self.height as i32 - 1);

        let dx = bx - ax;
        let dy = by - ay;
        let len_sq = dx * dx + dy * dy;

        for py in min_y..=max_y {
            let cy = py as f32 + 0.5;
            for px in min_x..=max_x {
                let cx = px as f32 + 0.5;
                let t = if len_sq <= 1e-6 {
                    0.0
                } else {
                    (((cx - ax) * dx + (cy - ay) * dy) / len_sq).clamp(0.0, 1.0)
                };
                let proj_x = ax + t * dx;
                let proj_y = ay + t * dy;
                let dist = ((cx - proj_x).powi(2) + (cy - proj_y).powi(2)).sqrt();
                let cov = (half_t + 0.5 - dist).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend_px(px, py, color, cov);
                }
            }
        }
    }

    pub fn draw_polyline(&mut self, pts: &[(f32, f32)], closed: bool, thickness: f32, color: [u8; 4]) {
        if pts.len() < 2 {
            return;
        }
        for w in pts.windows(2) {
            self.draw_line(w[0].0, w[0].1, w[1].0, w[1].1, thickness, color);
        }
        if closed && pts.len() > 2 {
            let last = pts[pts.len() - 1];
            let first = pts[0];
            self.draw_line(last.0, last.1, first.0, first.1, thickness, color);
        }
    }

    pub fn fill_circle(&mut self, cx: f32, cy: f32, radius: f32, color: [u8; 4]) {
        let scx = cx * self.scale;
        let scy = cy * self.scale;
        let sr = (radius * self.scale).max(0.5);
        let min_x = ((scx - sr - 1.5).floor() as i32).max(0);
        let max_x = ((scx + sr + 1.5).ceil() as i32).min(self.width as i32 - 1);
        let min_y = ((scy - sr - 1.5).floor() as i32).max(0);
        let max_y = ((scy + sr + 1.5).ceil() as i32).min(self.height as i32 - 1);
        for py in min_y..=max_y {
            let y = py as f32 + 0.5 - scy;
            for px in min_x..=max_x {
                let x = px as f32 + 0.5 - scx;
                let dist = (x * x + y * y).sqrt();
                let cov = (sr + 0.5 - dist).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend_px(px, py, color, cov);
                }
            }
        }
    }

    pub fn stroke_circle(&mut self, cx: f32, cy: f32, radius: f32, thickness: f32, color: [u8; 4]) {
        let scx = cx * self.scale;
        let scy = cy * self.scale;
        let sr = (radius * self.scale).max(0.5);
        let half_t = (thickness * self.scale * 0.5).max(0.45);
        let min_x = ((scx - sr - half_t - 1.5).floor() as i32).max(0);
        let max_x = ((scx + sr + half_t + 1.5).ceil() as i32).min(self.width as i32 - 1);
        let min_y = ((scy - sr - half_t - 1.5).floor() as i32).max(0);
        let max_y = ((scy + sr + half_t + 1.5).ceil() as i32).min(self.height as i32 - 1);
        for py in min_y..=max_y {
            let y = py as f32 + 0.5 - scy;
            for px in min_x..=max_x {
                let x = px as f32 + 0.5 - scx;
                let dist = ((x * x + y * y).sqrt() - sr).abs();
                let cov = (half_t + 0.5 - dist).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend_px(px, py, color, cov);
                }
            }
        }
    }

    fn rounded_box_sdf(px: f32, py: f32, cx: f32, cy: f32, hx: f32, hy: f32, r: f32) -> f32 {
        let r = r.min(hx).min(hy).max(0.0);
        let qx = (px - cx).abs() - (hx - r);
        let qy = (py - cy).abs() - (hy - r);
        let out_dist = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
        let in_dist = qx.max(qy).min(0.0);
        out_dist + in_dist - r
    }

    pub fn fill_rounded_rect(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        radius: f32,
        color: [u8; 4],
    ) {
        let sx0 = x0 * self.scale;
        let sy0 = y0 * self.scale;
        let sx1 = x1 * self.scale;
        let sy1 = y1 * self.scale;
        let cx = (sx0 + sx1) * 0.5;
        let cy = (sy0 + sy1) * 0.5;
        let hx = ((sx1 - sx0) * 0.5).max(0.5);
        let hy = ((sy1 - sy0) * 0.5).max(0.5);
        let sr = radius * self.scale;

        let min_x = ((sx0 - 1.5).floor() as i32).max(0);
        let max_x = ((sx1 + 1.5).ceil() as i32).min(self.width as i32 - 1);
        let min_y = ((sy0 - 1.5).floor() as i32).max(0);
        let max_y = ((sy1 + 1.5).ceil() as i32).min(self.height as i32 - 1);

        for py in min_y..=max_y {
            let fy = py as f32 + 0.5;
            for px in min_x..=max_x {
                let fx = px as f32 + 0.5;
                let sdf = Self::rounded_box_sdf(fx, fy, cx, cy, hx, hy, sr);
                let cov = (0.5 - sdf).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend_px(px, py, color, cov);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn stroke_rounded_rect(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        radius: f32,
        thickness: f32,
        color: [u8; 4],
    ) {
        let sx0 = x0 * self.scale;
        let sy0 = y0 * self.scale;
        let sx1 = x1 * self.scale;
        let sy1 = y1 * self.scale;
        let cx = (sx0 + sx1) * 0.5;
        let cy = (sy0 + sy1) * 0.5;
        let hx = ((sx1 - sx0) * 0.5).max(0.5);
        let hy = ((sy1 - sy0) * 0.5).max(0.5);
        let sr = radius * self.scale;
        let half_t = (thickness * self.scale * 0.5).max(0.45);

        let min_x = ((sx0 - half_t - 1.5).floor() as i32).max(0);
        let max_x = ((sx1 + half_t + 1.5).ceil() as i32).min(self.width as i32 - 1);
        let min_y = ((sy0 - half_t - 1.5).floor() as i32).max(0);
        let max_y = ((sy1 + half_t + 1.5).ceil() as i32).min(self.height as i32 - 1);

        for py in min_y..=max_y {
            let fy = py as f32 + 0.5;
            for px in min_x..=max_x {
                let fx = px as f32 + 0.5;
                let sdf = Self::rounded_box_sdf(fx, fy, cx, cy, hx, hy, sr).abs();
                let cov = (half_t + 0.5 - sdf).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend_px(px, py, color, cov);
                }
            }
        }
    }

    pub fn fill_polygon(&mut self, pts: &[(f32, f32)], color: [u8; 4]) {
        if pts.len() < 3 {
            return;
        }
        let spts: Vec<(f32, f32)> = pts.iter().map(|&(x, y)| (x * self.scale, y * self.scale)).collect();
        let mut min_x = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for &(x, y) in &spts {
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
        let ix0 = ((min_x - 1.0).floor() as i32).max(0);
        let ix1 = ((max_x + 1.0).ceil() as i32).min(self.width as i32 - 1);
        let iy0 = ((min_y - 1.0).floor() as i32).max(0);
        let iy1 = ((max_y + 1.0).ceil() as i32).min(self.height as i32 - 1);

        let point_in_poly = |px: f32, py: f32| -> bool {
            let mut inside = false;
            let n = spts.len();
            let mut j = n - 1;
            for i in 0..n {
                let (xi, yi) = spts[i];
                let (xj, yj) = spts[j];
                if ((yi > py) != (yj > py)) && (px < (xj - xi) * (py - yi) / (yj - yi) + xi) {
                    inside = !inside;
                }
                j = i;
            }
            inside
        };

        const SUB: [f32; 3] = [0.2, 0.5, 0.8];
        for py in iy0..=iy1 {
            for px in ix0..=ix1 {
                let mut hits = 0u32;
                for &sy in &SUB {
                    for &sx in &SUB {
                        if point_in_poly(px as f32 + sx, py as f32 + sy) {
                            hits += 1;
                        }
                    }
                }
                if hits > 0 {
                    self.blend_px(px, py, color, hits as f32 / 9.0);
                }
            }
        }
    }

    pub fn draw_galley(
        &mut self,
        galley: &Galley,
        origin_x: f32,
        origin_y: f32,
        font_img: &egui::epaint::FontImage,
    ) {
        let atlas_w = font_img.size[0];
        let atlas_h = font_img.size[1];
        if atlas_w == 0 || atlas_h == 0 {
            return;
        }
        for row in &galley.rows {
            for glyph in &row.glyphs {
                let uv = glyph.uv_rect;
                if uv.is_nothing() {
                    continue;
                }
                let fmt = &galley.job.sections[glyph.section_index as usize].format;
                let color = [fmt.color.r(), fmt.color.g(), fmt.color.b(), fmt.color.a()];
                let gx0 = (origin_x + glyph.pos.x + uv.offset.x) * self.scale;
                let gy0 = (origin_y + glyph.pos.y + uv.offset.y) * self.scale;

                let ux0 = uv.min[0] as usize;
                let uy0 = uv.min[1] as usize;
                let ux1 = (uv.max[0] as usize).min(atlas_w);
                let uy1 = (uv.max[1] as usize).min(atlas_h);
                let gw = ux1.saturating_sub(ux0);
                let gh = uy1.saturating_sub(uy0);
                if gw == 0 || gh == 0 {
                    continue;
                }

                let top_shear = if fmt.italics {
                    uv.size.y * self.scale * 0.22
                } else {
                    0.0
                };

                for dy in 0..gh {
                    let frac_from_bottom = 1.0 - (dy as f32 / gh.max(1) as f32);
                    let shear_x = top_shear * frac_from_bottom;
                    let dst_y = gy0.round() as i32 + dy as i32;
                    let src_row = (uy0 + dy) * atlas_w;
                    for dx in 0..gw {
                        let cov = font_img.pixels[src_row + ux0 + dx];
                        if cov > 0.004 {
                            let dst_x = (gx0 + shear_x).round() as i32 + dx as i32;
                            self.blend_px(dst_x, dst_y, color, cov);
                        }
                    }
                }
            }
        }
    }

    pub fn into_png_bytes(self) -> Option<Vec<u8>> {
        let img = image::RgbaImage::from_raw(self.width, self.height, self.data)?;
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .ok()?;
        Some(out.into_inner())
    }
}

// ---------------------------------------------------------------------------
// LaTeX Math Lexer & Recursive-Descent Parser
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Char(char),
    Cmd(String),
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Sup,
    Sub,
    Amp,
    NewRow,
    Space,
}

fn tokenize_latex(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let n = chars.len();
    let mut i = 0usize;

    while i < n {
        let ch = chars[i];
        if ch == '%' {
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if ch.is_whitespace() {
            while i < n && chars[i].is_whitespace() {
                i += 1;
            }
            if !tokens.is_empty() && !matches!(tokens.last(), Some(Token::Space)) {
                tokens.push(Token::Space);
            }
            continue;
        }
        match ch {
            '{' => {
                tokens.push(Token::LBrace);
                i += 1;
            }
            '}' => {
                tokens.push(Token::RBrace);
                i += 1;
            }
            '[' => {
                tokens.push(Token::LBracket);
                i += 1;
            }
            ']' => {
                tokens.push(Token::RBracket);
                i += 1;
            }
            '^' => {
                tokens.push(Token::Sup);
                i += 1;
            }
            '_' => {
                tokens.push(Token::Sub);
                i += 1;
            }
            '&' => {
                tokens.push(Token::Amp);
                i += 1;
            }
            '\\' => {
                i += 1;
                if i >= n {
                    break;
                }
                if chars[i] == '\\' {
                    tokens.push(Token::NewRow);
                    i += 1;
                    // Skip optional \\[len]
                    let mut k = i;
                    while k < n && chars[k].is_whitespace() {
                        k += 1;
                    }
                    if k < n && chars[k] == '[' {
                        k += 1;
                        while k < n && chars[k] != ']' {
                            k += 1;
                        }
                        if k < n && chars[k] == ']' {
                            i = k + 1;
                        }
                    }
                } else if chars[i].is_ascii_alphabetic() {
                    let start = i;
                    while i < n && chars[i].is_ascii_alphabetic() {
                        i += 1;
                    }
                    // Allow starred environments/commands like align*
                    if i < n && chars[i] == '*' {
                        i += 1;
                    }
                    let name: String = chars[start..i].iter().collect();
                    tokens.push(Token::Cmd(name));
                    // TeX eats trailing spaces after control words
                    while i < n && chars[i].is_whitespace() {
                        i += 1;
                    }
                } else {
                    let sym = chars[i];
                    i += 1;
                    tokens.push(Token::Cmd(sym.to_string()));
                }
            }
            other => {
                tokens.push(Token::Char(other));
                i += 1;
            }
        }
    }
    tokens
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<&'a Token> {
        let t = self.tokens.get(self.pos);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(Token::Space)) {
            self.pos += 1;
        }
    }

    fn parse_sequence(&mut self, stop_at_rbrace: bool, stop_at_right_cmd: bool) -> MathNode {
        let mut items: Vec<MathNode> = Vec::new();
        while let Some(tok) = self.peek() {
            if stop_at_rbrace && matches!(tok, Token::RBrace) {
                break;
            }
            if stop_at_right_cmd && matches!(tok, Token::Cmd(c) if c == "right") {
                break;
            }
            if matches!(tok, Token::Amp | Token::NewRow) {
                break;
            }
            if matches!(tok, Token::Cmd(c) if c == "end") {
                break;
            }
            if matches!(tok, Token::Cmd(c) if c == "over" || c == "choose" || c == "atop") {
                let cmd = match self.next() {
                    Some(Token::Cmd(c)) => c.clone(),
                    _ => unreachable!(),
                };
                let num = simplify_row(items);
                let den = self.parse_sequence(stop_at_rbrace, stop_at_right_cmd);
                let frac = MathNode::Frac {
                    num: Box::new(num),
                    den: Box::new(den),
                    has_bar: cmd == "over",
                };
                return if cmd == "choose" {
                    MathNode::Delimited {
                        left: Some("(".to_string()),
                        right: Some(")".to_string()),
                        body: Box::new(frac),
                    }
                } else {
                    frac
                };
            }
            if let Some(atom) = self.parse_scripted_atom() {
                if !matches!(atom, MathNode::Empty) {
                    items.push(atom);
                }
            }
        }
        simplify_row(items)
    }

    fn parse_scripted_atom(&mut self) -> Option<MathNode> {
        self.skip_spaces();
        let base = self.parse_atom()?;
        let mut sub: Option<MathNode> = None;
        let mut sup: Option<MathNode> = None;
        let mut explicit_limits: Option<bool> = None;

        loop {
            self.skip_spaces();
            match self.peek() {
                Some(Token::Sub) => {
                    self.next();
                    let arg = self.parse_arg_or_atom();
                    sub = Some(arg);
                }
                Some(Token::Sup) => {
                    self.next();
                    let arg = self.parse_arg_or_atom();
                    sup = Some(match sup {
                        Some(prev) => MathNode::Row(vec![prev, arg]),
                        None => arg,
                    });
                }
                Some(Token::Char('\'')) => {
                    let mut count = 0usize;
                    while matches!(self.peek(), Some(Token::Char('\''))) {
                        self.next();
                        count += 1;
                    }
                    let primes = match count {
                        1 => "′".to_string(),
                        2 => "″".to_string(),
                        3 => "‴".to_string(),
                        n => "′".repeat(n),
                    };
                    let p_node = MathNode::Text {
                        text: primes,
                        style: MathFontStyle::Roman,
                        kind: MathAtomKind::Ord,
                    };
                    sup = Some(match sup {
                        Some(prev) => MathNode::Row(vec![prev, p_node]),
                        None => p_node,
                    });
                }
                Some(Token::Cmd(c)) if c == "limits" => {
                    self.next();
                    explicit_limits = Some(true);
                }
                Some(Token::Cmd(c)) if c == "nolimits" => {
                    self.next();
                    explicit_limits = Some(false);
                }
                _ => break,
            }
        }

        if sub.is_some() || sup.is_some() {
            let default_limits = is_limits_base(&base);
            Some(MathNode::Scripts {
                base: Box::new(base),
                sub: sub.map(Box::new),
                sup: sup.map(Box::new),
                limits: explicit_limits.unwrap_or(default_limits),
            })
        } else {
            Some(base)
        }
    }

    fn parse_arg_or_atom(&mut self) -> MathNode {
        self.skip_spaces();
        match self.peek() {
            Some(Token::LBrace) => {
                self.next();
                let inner = self.parse_sequence(true, false);
                if matches!(self.peek(), Some(Token::RBrace)) {
                    self.next();
                }
                inner
            }
            Some(_) => self.parse_atom().unwrap_or(MathNode::Empty),
            None => MathNode::Empty,
        }
    }

    fn parse_optional_bracket_arg(&mut self) -> Option<MathNode> {
        self.skip_spaces();
        if !matches!(self.peek(), Some(Token::LBracket)) {
            return None;
        }
        self.next();
        let mut items = Vec::new();
        while let Some(tok) = self.peek() {
            if matches!(tok, Token::RBracket | Token::RBrace) {
                break;
            }
            if let Some(a) = self.parse_scripted_atom() {
                items.push(a);
            }
        }
        if matches!(self.peek(), Some(Token::RBracket)) {
            self.next();
        }
        Some(simplify_row(items))
    }

    fn parse_plain_brace_text(&mut self) -> String {
        self.skip_spaces();
        if !matches!(self.peek(), Some(Token::LBrace)) {
            return String::new();
        }
        self.next();
        let mut out = String::new();
        let mut depth = 1usize;
        while let Some(tok) = self.next() {
            match tok {
                Token::LBrace => {
                    depth += 1;
                    out.push('{');
                }
                Token::RBrace => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push('}');
                }
                Token::Char(c) => out.push(*c),
                Token::Space => out.push(' '),
                Token::LBracket => out.push('['),
                Token::RBracket => out.push(']'),
                Token::Cmd(c) => {
                    if c == "{" || c == "}" || c == "%" || c == "$" || c == "&" || c == "#" || c == "_" {
                        out.push_str(c);
                    } else if c == " " {
                        out.push(' ');
                    } else if let Some((sym, _)) = lookup_latex_symbol(c) {
                        out.push_str(sym);
                    } else {
                        out.push_str(c);
                    }
                }
                Token::Sub => out.push('_'),
                Token::Sup => out.push('^'),
                Token::Amp => out.push('&'),
                Token::NewRow => out.push('\n'),
            }
        }
        out
    }

    fn parse_delimiter_token(&mut self) -> Option<String> {
        self.skip_spaces();
        match self.next()? {
            Token::Char('.') => None,
            Token::Char(c) => match c {
                '(' | ')' | '|' | '/' => Some(c.to_string()),
                '<' => Some("⟨".to_string()),
                '>' => Some("⟩".to_string()),
                _ => Some(c.to_string()),
            },
            Token::LBracket => Some("[".to_string()),
            Token::RBracket => Some("]".to_string()),
            Token::Cmd(c) => match c.as_str() {
                "{" => Some("{".to_string()),
                "}" => Some("}".to_string()),
                "|" => Some("‖".to_string()),
                "langle" => Some("⟨".to_string()),
                "rangle" => Some("⟩".to_string()),
                "lfloor" => Some("⌊".to_string()),
                "rfloor" => Some("⌋".to_string()),
                "lceil" => Some("⌈".to_string()),
                "rceil" => Some("⌉".to_string()),
                "lvert" | "rvert" => Some("|".to_string()),
                "lVert" | "rVert" => Some("‖".to_string()),
                "lbrace" => Some("{".to_string()),
                "rbrace" => Some("}".to_string()),
                _ => lookup_latex_symbol(c).map(|(s, _)| s.to_string()),
            },
            _ => None,
        }
    }

    fn parse_atom(&mut self) -> Option<MathNode> {
        self.skip_spaces();
        let tok = self.next()?.clone();
        match tok {
            Token::LBrace => {
                let inner = self.parse_sequence(true, false);
                if matches!(self.peek(), Some(Token::RBrace)) {
                    self.next();
                }
                Some(inner)
            }
            Token::RBrace => Some(MathNode::Empty),
            Token::LBracket => Some(MathNode::Text {
                text: "[".to_string(),
                style: MathFontStyle::Roman,
                kind: MathAtomKind::Open,
            }),
            Token::RBracket => Some(MathNode::Text {
                text: "]".to_string(),
                style: MathFontStyle::Roman,
                kind: MathAtomKind::Close,
            }),
            Token::Char(c) => Some(char_to_math_node(c)),
            Token::Cmd(cmd) => Some(self.parse_command(&cmd)),
            Token::Space | Token::Sup | Token::Sub | Token::Amp | Token::NewRow => {
                Some(MathNode::Empty)
            }
        }
    }

    fn parse_command(&mut self, cmd: &str) -> MathNode {
        match cmd {
            "frac" | "dfrac" | "tfrac" | "cfrac" => {
                let num = self.parse_arg_or_atom();
                let den = self.parse_arg_or_atom();
                MathNode::Frac {
                    num: Box::new(num),
                    den: Box::new(den),
                    has_bar: true,
                }
            }
            "binom" | "tbinom" | "dbinom" => {
                let num = self.parse_arg_or_atom();
                let den = self.parse_arg_or_atom();
                MathNode::Delimited {
                    left: Some("(".to_string()),
                    right: Some(")".to_string()),
                    body: Box::new(MathNode::Frac {
                        num: Box::new(num),
                        den: Box::new(den),
                        has_bar: false,
                    }),
                }
            }
            "sqrt" => {
                let deg = self.parse_optional_bracket_arg();
                let rad = self.parse_arg_or_atom();
                MathNode::Sqrt {
                    degree: deg.map(Box::new),
                    radicand: Box::new(rad),
                }
            }
            "left" => {
                let left = self.parse_delimiter_token();
                let body = self.parse_sequence(false, true);
                let right = if matches!(self.peek(), Some(Token::Cmd(c)) if c == "right") {
                    self.next();
                    self.parse_delimiter_token()
                } else {
                    None
                };
                MathNode::Delimited {
                    left,
                    right,
                    body: Box::new(body),
                }
            }
            "right" => MathNode::Empty,
            "bigl" | "bigr" | "Bigl" | "Bigr" | "biggl" | "biggr" | "Biggl" | "Biggr"
            | "big" | "Big" | "bigg" | "Bigg" => {
                if let Some(d) = self.parse_delimiter_token() {
                    MathNode::Text {
                        text: d,
                        style: MathFontStyle::Roman,
                        kind: MathAtomKind::Ord,
                    }
                } else {
                    MathNode::Empty
                }
            }
            "hat" | "widehat" => MathNode::Accent {
                kind: AccentKind::Hat,
                body: Box::new(self.parse_arg_or_atom()),
            },
            "bar" | "overline" => MathNode::Accent {
                kind: AccentKind::Overline,
                body: Box::new(self.parse_arg_or_atom()),
            },
            "underline" => MathNode::Accent {
                kind: AccentKind::Underline,
                body: Box::new(self.parse_arg_or_atom()),
            },
            "vec" | "overrightarrow" => MathNode::Accent {
                kind: AccentKind::Vector,
                body: Box::new(self.parse_arg_or_atom()),
            },
            "overleftarrow" => MathNode::Accent {
                kind: AccentKind::LeftVector,
                body: Box::new(self.parse_arg_or_atom()),
            },
            "tilde" | "widetilde" => MathNode::Accent {
                kind: AccentKind::Tilde,
                body: Box::new(self.parse_arg_or_atom()),
            },
            "dot" => MathNode::Accent {
                kind: AccentKind::Dot,
                body: Box::new(self.parse_arg_or_atom()),
            },
            "ddot" => MathNode::Accent {
                kind: AccentKind::DDot,
                body: Box::new(self.parse_arg_or_atom()),
            },
            "text" | "textrm" | "mbox" => {
                let s = self.parse_plain_brace_text();
                MathNode::Text {
                    text: s,
                    style: MathFontStyle::Roman,
                    kind: MathAtomKind::Ord,
                }
            }
            "textbf" | "mathbf" | "boldsymbol" | "bm" => {
                let mut inner = self.parse_arg_or_atom();
                apply_font_style(&mut inner, MathFontStyle::Bold);
                inner
            }
            "textit" | "mathit" => {
                let mut inner = self.parse_arg_or_atom();
                apply_font_style(&mut inner, MathFontStyle::Italic);
                inner
            }
            "mathrm" | "operatorname" => {
                let mut inner = self.parse_arg_or_atom();
                apply_font_style(&mut inner, MathFontStyle::Roman);
                inner
            }
            "mathbb" => {
                let s = self.parse_plain_brace_text();
                let mapped: String = s.chars().map(map_blackboard_char).collect();
                MathNode::Text {
                    text: mapped,
                    style: MathFontStyle::Blackboard,
                    kind: MathAtomKind::Ord,
                }
            }
            "mathcal" | "mathscr" | "mathfrak" => {
                let mut inner = self.parse_arg_or_atom();
                apply_font_style(&mut inner, MathFontStyle::Calligraphic);
                inner
            }
            "texttt" | "mathtt" => {
                let s = self.parse_plain_brace_text();
                MathNode::Text {
                    text: s,
                    style: MathFontStyle::Monospace,
                    kind: MathAtomKind::Ord,
                }
            }
            "boxed" => MathNode::Boxed(Box::new(self.parse_arg_or_atom())),
            "tag" => {
                let raw = self.parse_plain_brace_text();
                let t = raw.trim();
                let formatted = if t.starts_with('(') && t.ends_with(')') {
                    t.to_string()
                } else {
                    format!("({t})")
                };
                MathNode::Row(vec![
                    MathNode::Space(1.4),
                    MathNode::Text {
                        text: formatted,
                        style: MathFontStyle::Roman,
                        kind: MathAtomKind::Ord,
                    },
                ])
            }
            "begin" => self.parse_environment(),
            "," => MathNode::Space(0.17),
            ":" | ">" => MathNode::Space(0.22),
            ";" => MathNode::Space(0.28),
            "!" => MathNode::Space(-0.14),
            " " => MathNode::Space(0.28),
            "quad" => MathNode::Space(0.95),
            "qquad" => MathNode::Space(1.85),
            "displaystyle" | "textstyle" | "scriptstyle" | "scriptscriptstyle" => MathNode::Empty,
            "not" => {
                self.skip_spaces();
                match self.peek() {
                    Some(Token::Char('=')) => {
                        self.next();
                        MathNode::Text {
                            text: "≠".to_string(),
                            style: MathFontStyle::Roman,
                            kind: MathAtomKind::Rel,
                        }
                    }
                    Some(Token::Cmd(next_c)) if next_c == "in" => {
                        self.next();
                        MathNode::Text {
                            text: "∉".to_string(),
                            style: MathFontStyle::Roman,
                            kind: MathAtomKind::Rel,
                        }
                    }
                    Some(Token::Cmd(next_c)) if next_c == "equiv" => {
                        self.next();
                        MathNode::Text {
                            text: "≢".to_string(),
                            style: MathFontStyle::Roman,
                            kind: MathAtomKind::Rel,
                        }
                    }
                    _ => MathNode::Text {
                        text: "¬".to_string(),
                        style: MathFontStyle::Roman,
                        kind: MathAtomKind::Ord,
                    },
                }
            }
            _ => {
                if let Some((sym, kind)) = lookup_latex_symbol(cmd) {
                    MathNode::Text {
                        text: sym.to_string(),
                        style: MathFontStyle::Roman,
                        kind,
                    }
                } else if is_named_operator(cmd) {
                    MathNode::Text {
                        text: cmd.to_string(),
                        style: MathFontStyle::Operator,
                        kind: MathAtomKind::Op,
                    }
                } else {
                    MathNode::Text {
                        text: cmd.to_string(),
                        style: MathFontStyle::Roman,
                        kind: MathAtomKind::Ord,
                    }
                }
            }
        }
    }

    fn parse_environment(&mut self) -> MathNode {
        let env_name = self.parse_plain_brace_text();
        let env = match env_name.trim() {
            "pmatrix" => MatrixEnv::Paren,
            "bmatrix" => MatrixEnv::Bracket,
            "Bmatrix" => MatrixEnv::Brace,
            "vmatrix" => MatrixEnv::VBar,
            "Vmatrix" => MatrixEnv::DoubleVBar,
            "cases" | "dcases" => MatrixEnv::Cases,
            "aligned" | "align" | "align*" | "split" | "eqnarray" | "eqnarray*" => {
                MatrixEnv::Aligned
            }
            "array" => {
                // Consume column alignment spec {c|cc}
                let _ = self.parse_plain_brace_text();
                MatrixEnv::Plain
            }
            _ => MatrixEnv::Plain,
        };

        let mut rows: Vec<Vec<MathNode>> = Vec::new();
        let mut cur_row: Vec<MathNode> = Vec::new();

        loop {
            self.skip_spaces();
            if matches!(self.peek(), Some(Token::Cmd(c)) if c == "end") {
                self.next();
                let _ = self.parse_plain_brace_text();
                break;
            }
            if self.peek().is_none() {
                break;
            }
            let cell = self.parse_sequence(false, false);
            cur_row.push(cell);
            match self.peek() {
                Some(Token::Amp) => {
                    self.next();
                }
                Some(Token::NewRow) => {
                    self.next();
                    rows.push(std::mem::take(&mut cur_row));
                }
                Some(Token::Cmd(c)) if c == "end" => {}
                _ => {
                    if self.peek().is_none() {
                        break;
                    }
                    self.next();
                }
            }
        }
        if !cur_row.is_empty() && (cur_row.len() > 1 || !matches!(cur_row[0], MathNode::Empty)) {
            rows.push(cur_row);
        }
        if rows.is_empty() {
            return MathNode::Empty;
        }
        if matches!(env, MatrixEnv::Plain) && rows.len() == 1 && rows[0].len() == 1 {
            return rows.into_iter().next().unwrap().into_iter().next().unwrap();
        }
        MathNode::Matrix { env, rows }
    }
}

fn simplify_row(mut items: Vec<MathNode>) -> MathNode {
    items.retain(|n| !matches!(n, MathNode::Empty));
    match items.len() {
        0 => MathNode::Empty,
        1 => items.into_iter().next().unwrap(),
        _ => MathNode::Row(items),
    }
}

fn apply_font_style(node: &mut MathNode, style: MathFontStyle) {
    match node {
        MathNode::Text { style: s, .. } => *s = style,
        MathNode::Boxed(inner) => apply_font_style(inner, style),
        MathNode::Row(children) => {
            for c in children {
                apply_font_style(c, style);
            }
        }
        _ => {}
    }
}

fn char_to_math_node(ch: char) -> MathNode {
    let kind = match ch {
        '+' | '-' | '*' | '/' => MathAtomKind::Op,
        '=' | '<' | '>' => MathAtomKind::Rel,
        '(' | '[' => MathAtomKind::Open,
        ')' | ']' => MathAtomKind::Close,
        ',' | ';' | ':' => MathAtomKind::Punct,
        _ => MathAtomKind::Ord,
    };
    let text = match ch {
        '-' => "−".to_string(),
        other => other.to_string(),
    };
    let style = if ch.is_ascii_alphabetic() {
        MathFontStyle::Normal
    } else {
        MathFontStyle::Roman
    };
    MathNode::Text { text, style, kind }
}

fn is_limits_base(node: &MathNode) -> bool {
    match node {
        MathNode::Text { text, .. } => matches!(
            text.as_str(),
            "∑" | "∏" | "∐" | "⋃" | "⋂" | "⋁" | "⋀" | "⨁" | "⨂" | "lim" | "limsup" | "liminf"
                | "max" | "min" | "sup" | "inf"
        ),
        _ => false,
    }
}

fn is_named_operator(cmd: &str) -> bool {
    matches!(
        cmd,
        "sin"
            | "cos"
            | "tan"
            | "cot"
            | "sec"
            | "csc"
            | "arcsin"
            | "arccos"
            | "arctan"
            | "sinh"
            | "cosh"
            | "tanh"
            | "coth"
            | "ln"
            | "log"
            | "lg"
            | "exp"
            | "lim"
            | "limsup"
            | "liminf"
            | "sup"
            | "inf"
            | "max"
            | "min"
            | "arg"
            | "det"
            | "dim"
            | "gcd"
            | "hom"
            | "ker"
            | "Pr"
            | "deg"
            | "mod"
            | "bmod"
            | "pmod"
            | "rank"
            | "tr"
            | "var"
            | "cov"
    )
}

fn map_blackboard_char(ch: char) -> char {
    match ch {
        'C' => 'ℂ',
        'H' => 'ℍ',
        'N' => 'ℕ',
        'P' => 'ℙ',
        'Q' => 'ℚ',
        'R' => 'ℝ',
        'Z' => 'ℤ',
        other => other,
    }
}

fn lookup_latex_symbol(cmd: &str) -> Option<(&'static str, MathAtomKind)> {
    use MathAtomKind::*;
    let pair = match cmd {
        // Greek lowercase
        "alpha" => ("α", Ord),
        "beta" => ("β", Ord),
        "gamma" => ("γ", Ord),
        "delta" => ("δ", Ord),
        "epsilon" => ("ϵ", Ord),
        "varepsilon" => ("ε", Ord),
        "zeta" => ("ζ", Ord),
        "eta" => ("η", Ord),
        "theta" => ("θ", Ord),
        "vartheta" => ("ϑ", Ord),
        "iota" => ("ι", Ord),
        "kappa" => ("κ", Ord),
        "lambda" => ("λ", Ord),
        "mu" => ("μ", Ord),
        "nu" => ("ν", Ord),
        "xi" => ("ξ", Ord),
        "pi" => ("π", Ord),
        "varpi" => ("ϖ", Ord),
        "rho" => ("ρ", Ord),
        "varrho" => ("ϱ", Ord),
        "sigma" => ("σ", Ord),
        "varsigma" => ("ς", Ord),
        "tau" => ("τ", Ord),
        "upsilon" => ("υ", Ord),
        "phi" => ("ϕ", Ord),
        "varphi" => ("φ", Ord),
        "chi" => ("χ", Ord),
        "psi" => ("ψ", Ord),
        "omega" => ("ω", Ord),
        // Greek uppercase
        "Gamma" => ("Γ", Ord),
        "Delta" => ("Δ", Ord),
        "Theta" => ("Θ", Ord),
        "Lambda" => ("Λ", Ord),
        "Xi" => ("Ξ", Ord),
        "Pi" => ("Π", Ord),
        "Sigma" => ("Σ", Ord),
        "Upsilon" => ("Υ", Ord),
        "Phi" => ("Φ", Ord),
        "Psi" => ("Ψ", Ord),
        "Omega" => ("Ω", Ord),
        // Big operators
        "sum" => ("∑", Op),
        "prod" => ("∏", Op),
        "coprod" => ("∐", Op),
        "int" => ("∫", Op),
        "iint" => ("∬", Op),
        "iiint" => ("∭", Op),
        "oint" => ("∮", Op),
        "bigcup" => ("⋃", Op),
        "bigcap" => ("⋂", Op),
        "bigvee" => ("⋁", Op),
        "bigwedge" => ("⋀", Op),
        "bigoplus" => ("⨁", Op),
        "bigotimes" => ("⨂", Op),
        // Binary operators
        "pm" => ("±", Op),
        "mp" => ("∓", Op),
        "times" => ("×", Op),
        "div" => ("÷", Op),
        "cdot" => ("⋅", Op),
        "ast" => ("∗", Op),
        "star" => ("⋆", Op),
        "circ" => ("∘", Op),
        "bullet" => ("•", Op),
        "oplus" => ("⊕", Op),
        "ominus" => ("⊖", Op),
        "otimes" => ("⊗", Op),
        "oslash" => ("⊘", Op),
        "odot" => ("⊙", Op),
        "cap" => ("∩", Op),
        "cup" => ("∪", Op),
        "uplus" => ("⊎", Op),
        "sqcap" => ("⊓", Op),
        "sqcup" => ("⊔", Op),
        "vee" | "lor" => ("∨", Op),
        "wedge" | "land" => ("∧", Op),
        "setminus" | "smallsetminus" => ("∖", Op),
        // Relations & Arrows
        "le" | "leq" => ("≤", Rel),
        "ge" | "geq" => ("≥", Rel),
        "neq" | "ne" => ("≠", Rel),
        "approx" => ("≈", Rel),
        "equiv" => ("≡", Rel),
        "cong" => ("≅", Rel),
        "sim" => ("∼", Rel),
        "simeq" => ("≃", Rel),
        "propto" => ("∝", Rel),
        "ll" => ("≪", Rel),
        "gg" => ("≫", Rel),
        "prec" => ("≺", Rel),
        "succ" => ("≻", Rel),
        "preceq" => ("⪯", Rel),
        "succeq" => ("⪰", Rel),
        "subset" => ("⊂", Rel),
        "supset" => ("⊃", Rel),
        "subseteq" => ("⊆", Rel),
        "supseteq" => ("⊇", Rel),
        "nsubseteq" => ("⊈", Rel),
        "in" => ("∈", Rel),
        "notin" => ("∉", Rel),
        "ni" => ("∋", Rel),
        "vdash" => ("⊢", Rel),
        "models" => ("⊨", Rel),
        "perp" => ("⊥", Rel),
        "parallel" => ("∥", Rel),
        "mid" => ("∣", Rel),
        "nmid" => ("∤", Rel),
        "to" | "rightarrow" => ("→", Rel),
        "leftarrow" | "gets" => ("←", Rel),
        "leftrightarrow" => ("↔", Rel),
        "Rightarrow" => ("⇒", Rel),
        "Leftarrow" => ("⇐", Rel),
        "Leftrightarrow" | "iff" => ("⇔", Rel),
        "implies" => ("⟹", Rel),
        "impliedby" => ("⟸", Rel),
        "mapsto" => ("↦", Rel),
        "hookrightarrow" => ("↪", Rel),
        "uparrow" => ("↑", Rel),
        "downarrow" => ("↓", Rel),
        "updownarrow" => ("↕", Rel),
        "nearrow" => ("↗", Rel),
        "searrow" => ("↘", Rel),
        "swarrow" => ("↙", Rel),
        "nwarrow" => ("↖", Rel),
        // Misc symbols & escaped literals
        "infty" => ("∞", Ord),
        "partial" => ("∂", Ord),
        "nabla" => ("∇", Ord),
        "forall" => ("∀", Ord),
        "exists" => ("∃", Ord),
        "nexists" => ("∄", Ord),
        "emptyset" | "varnothing" => ("∅", Ord),
        "aleph" => ("ℵ", Ord),
        "hbar" => ("ℏ", Ord),
        "ell" => ("ℓ", Ord),
        "wp" => ("℘", Ord),
        "Re" => ("ℜ", Ord),
        "Im" => ("ℑ", Ord),
        "angle" => ("∠", Ord),
        "triangle" => ("△", Ord),
        "square" => ("□", Ord),
        "diamond" => ("◇", Ord),
        "prime" => ("′", Ord),
        "degree" => ("°", Ord),
        "dots" | "ldots" => ("…", Ord),
        "cdots" => ("⋯", Ord),
        "vdots" => ("⋮", Ord),
        "ddots" => ("⋱", Ord),
        "therefore" => ("∴", Rel),
        "because" => ("∵", Rel),
        "neg" | "lnot" => ("¬", Ord),
        "top" => ("⊤", Ord),
        "bot" => ("⊥", Ord),
        "checkmark" => ("✓", Ord),
        "langle" => ("⟨", Open),
        "rangle" => ("⟩", Close),
        "lfloor" => ("⌊", Open),
        "rfloor" => ("⌋", Close),
        "lceil" => ("⌈", Open),
        "rceil" => ("⌉", Close),
        "{" => ("{", Open),
        "}" => ("}", Close),
        "|" => ("‖", Ord),
        "%" => ("%", Ord),
        "$" => ("$", Ord),
        "&" => ("&", Ord),
        "#" => ("#", Ord),
        "_" => ("_", Ord),
        _ => return None,
    };
    Some(pair)
}

/// Parses a LaTeX math string into a `MathNode` tree, including top-level `\\` aligned rows.
pub fn parse_latex_math(input: &str) -> MathNode {
    let tokens = tokenize_latex(input);
    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
    };
    let mut rows: Vec<Vec<MathNode>> = Vec::new();
    let mut cur_row: Vec<MathNode> = Vec::new();
    loop {
        let seq = parser.parse_sequence(false, false);
        if !matches!(seq, MathNode::Empty) {
            cur_row.push(seq);
        }
        match parser.peek() {
            Some(Token::Amp) => {
                parser.next();
            }
            Some(Token::NewRow) => {
                parser.next();
                if !cur_row.is_empty() {
                    rows.push(std::mem::take(&mut cur_row));
                }
            }
            _ => break,
        }
    }
    if !cur_row.is_empty() {
        rows.push(cur_row);
    }
    if rows.is_empty() {
        MathNode::Empty
    } else if rows.len() == 1 && rows[0].len() == 1 {
        rows.into_iter().next().unwrap().into_iter().next().unwrap()
    } else {
        MathNode::Matrix {
            env: MatrixEnv::Aligned,
            rows,
        }
    }
}

/// Converts a LaTeX math expression into readable Unicode plain text (for search/bookmarks/fallback).
pub fn math_to_plain_text(latex: &str) -> String {
    let node = parse_latex_math(latex);
    let mut out = String::new();
    node_to_plain_text(&node, &mut out);
    out
}

fn node_to_plain_text(node: &MathNode, out: &mut String) {
    match node {
        MathNode::Empty => {}
        MathNode::Row(items) => {
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 && matches!(item, MathNode::Text { kind: MathAtomKind::Rel | MathAtomKind::Op, .. }) {
                    out.push(' ');
                }
                node_to_plain_text(item, out);
                if idx + 1 < items.len()
                    && matches!(item, MathNode::Text { kind: MathAtomKind::Rel | MathAtomKind::Op | MathAtomKind::Punct, .. })
                {
                    out.push(' ');
                }
            }
        }
        MathNode::Text { text, .. } => out.push_str(text),
        MathNode::Space(em) => {
            if *em > 0.05 {
                out.push(' ');
            }
        }
        MathNode::Frac { num, den, has_bar } => {
            out.push('(');
            node_to_plain_text(num, out);
            out.push(if *has_bar { '/' } else { ' ' });
            node_to_plain_text(den, out);
            out.push(')');
        }
        MathNode::Sqrt { degree, radicand } => {
            if let Some(d) = degree {
                node_to_plain_text(d, out);
            }
            out.push_str("√(");
            node_to_plain_text(radicand, out);
            out.push(')');
        }
        MathNode::Scripts { base, sub, sup, .. } => {
            node_to_plain_text(base, out);
            if let Some(s) = sub {
                out.push('_');
                node_to_plain_text(s, out);
            }
            if let Some(s) = sup {
                out.push('^');
                node_to_plain_text(s, out);
            }
        }
        MathNode::Delimited { left, right, body } => {
            if let Some(l) = left {
                out.push_str(l);
            }
            node_to_plain_text(body, out);
            if let Some(r) = right {
                out.push_str(r);
            }
        }
        MathNode::Accent { body, .. } => node_to_plain_text(body, out),
        MathNode::Boxed(body) => {
            out.push('[');
            node_to_plain_text(body, out);
            out.push(']');
        }
        MathNode::Matrix { rows, .. } => {
            out.push('[');
            for (r_idx, row) in rows.iter().enumerate() {
                if r_idx > 0 {
                    out.push_str("; ");
                }
                for (c_idx, cell) in row.iter().enumerate() {
                    if c_idx > 0 {
                        out.push_str(", ");
                    }
                    node_to_plain_text(cell, out);
                }
            }
            out.push(']');
        }
    }
}

// ---------------------------------------------------------------------------
// 2D Mathematical Typesetting Layout Engine
// ---------------------------------------------------------------------------

trait TextMeasurer {
    fn measure(&mut self, text: &str, font_size: f32, italic: bool, bold: bool, mono: bool) -> (f32, f32, f32);
}

struct FontsMeasurer<'a>(&'a Fonts);
impl TextMeasurer for FontsMeasurer<'_> {
    fn measure(&mut self, text: &str, font_size: f32, italic: bool, bold: bool, mono: bool) -> (f32, f32, f32) {
        measure_text_with_fonts(self.0, text, font_size, italic, bold, mono)
    }
}

fn translate_primitives(primitives: &mut [MathPrimitive], dx: f32, dy: f32) {
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    for p in primitives {
        match p {
            MathPrimitive::Text { x, baseline_y, .. } => {
                *x += dx;
                *baseline_y += dy;
            }
            MathPrimitive::Line { x0, y0, x1, y1, .. } => {
                *x0 += dx;
                *y0 += dy;
                *x1 += dx;
                *y1 += dy;
            }
            MathPrimitive::Polyline { points, .. } => {
                for pt in points {
                    pt.0 += dx;
                    pt.1 += dy;
                }
            }
            MathPrimitive::FilledCircle { cx, cy, .. } => {
                *cx += dx;
                *cy += dy;
            }
            MathPrimitive::Bracket {
                x,
                top_y,
                bottom_y,
                ..
            } => {
                *x += dx;
                *top_y += dy;
                *bottom_y += dy;
            }
        }
    }
}

fn atom_kind_of(node: &MathNode) -> MathAtomKind {
    match node {
        MathNode::Text { kind, .. } => *kind,
        MathNode::Scripts { base, .. } => atom_kind_of(base),
        MathNode::Delimited { .. } => MathAtomKind::Ord,
        _ => MathAtomKind::Ord,
    }
}

fn inter_atom_gap(prev: MathAtomKind, next: MathAtomKind, fs: f32) -> f32 {
    match (prev, next) {
        (MathAtomKind::Rel, _) | (_, MathAtomKind::Rel) => fs * 0.22,
        (MathAtomKind::Op, MathAtomKind::Ord | MathAtomKind::Open)
        | (MathAtomKind::Ord | MathAtomKind::Close, MathAtomKind::Op) => fs * 0.16,
        (MathAtomKind::Punct, _) => fs * 0.16,
        _ => fs * 0.03,
    }
}

fn is_big_operator(node: &MathNode) -> bool {
    matches!(
        node,
        MathNode::Text { text, .. }
            if matches!(
                text.as_str(),
                "∑" | "∏" | "∐" | "∫" | "∬" | "∭" | "∮" | "⋃" | "⋂" | "⋁" | "⋀" | "⨁" | "⨂"
            )
    )
}

fn parse_bracket_kind(delim: &str, is_left: bool) -> Option<BracketKind> {
    match (delim, is_left) {
        ("(", _) => Some(BracketKind::LeftParen),
        (")", _) => Some(BracketKind::RightParen),
        ("[", _) => Some(BracketKind::LeftBracket),
        ("]", _) => Some(BracketKind::RightBracket),
        ("{", _) => Some(BracketKind::LeftBrace),
        ("}", _) => Some(BracketKind::RightBrace),
        ("|", _) => Some(BracketKind::VBar),
        ("‖", _) => Some(BracketKind::DoubleVBar),
        ("⟨", _) => Some(BracketKind::LeftAngle),
        ("⟩", _) => Some(BracketKind::RightAngle),
        ("⌊", _) => Some(BracketKind::LeftFloor),
        ("⌋", _) => Some(BracketKind::RightFloor),
        ("⌈", _) => Some(BracketKind::LeftCeil),
        ("⌉", _) => Some(BracketKind::RightCeil),
        _ => None,
    }
}

fn bracket_width_for_height(kind: BracketKind, total_h: f32, fs: f32) -> f32 {
    match kind {
        BracketKind::VBar => (fs * 0.28).max(4.0),
        BracketKind::DoubleVBar => (fs * 0.44).max(6.0),
        BracketKind::LeftBrace | BracketKind::RightBrace => {
            (fs * 0.42 + total_h * 0.08).clamp(7.0, fs * 0.95)
        }
        BracketKind::LeftParen | BracketKind::RightParen => {
            (fs * 0.34 + total_h * 0.07).clamp(5.5, fs * 0.85)
        }
        BracketKind::LeftAngle | BracketKind::RightAngle => {
            (fs * 0.36 + total_h * 0.07).clamp(6.0, fs * 0.9)
        }
        _ => (fs * 0.36).max(5.5),
    }
}

fn layout_node(
    node: &MathNode,
    fs: f32,
    display: bool,
    measurer: &mut dyn TextMeasurer,
) -> MathLayout {
    let stroke_w = (fs * 0.062).max(1.0);
    let axis_y = -fs * 0.28;

    match node {
        MathNode::Empty => MathLayout {
            width: 0.0,
            ascent: fs * 0.70,
            descent: fs * 0.16,
            primitives: Vec::new(),
        },
        MathNode::Space(em) => MathLayout {
            width: (*em * fs).max(0.0),
            ascent: fs * 0.70,
            descent: fs * 0.16,
            primitives: Vec::new(),
        },
        MathNode::Text { text, style, .. } => {
            if text.is_empty() {
                return MathLayout {
                    width: 0.0,
                    ascent: fs * 0.70,
                    descent: fs * 0.16,
                    primitives: Vec::new(),
                };
            }
            let eff_fs = if is_big_operator(node) {
                if display {
                    fs * 1.45
                } else {
                    fs * 1.15
                }
            } else {
                fs
            };
            let italic = matches!(style, MathFontStyle::Normal | MathFontStyle::Italic);
            let bold = matches!(style, MathFontStyle::Bold);
            let monospace = matches!(style, MathFontStyle::Monospace);
            let (w, mut asc, mut desc) = measurer.measure(text, eff_fs, italic, bold, monospace);
            let mut baseline_y = 0.0;
            if is_big_operator(node) {
                // Center big operator vertically around the math axis
                let mid = (-asc + desc) * 0.5;
                let shift = axis_y - mid;
                baseline_y += shift;
                asc -= shift;
                desc += shift;
            }
            MathLayout {
                width: w,
                ascent: asc,
                descent: desc,
                primitives: vec![MathPrimitive::Text {
                    x: 0.0,
                    baseline_y,
                    text: text.clone(),
                    font_size: eff_fs,
                    italic,
                    bold,
                    monospace,
                }],
            }
        }
        MathNode::Row(items) => {
            let mut cur_x = 0.0_f32;
            let mut max_asc = fs * 0.70;
            let mut max_desc = fs * 0.16;
            let mut primitives = Vec::new();
            let mut prev_kind: Option<MathAtomKind> = None;

            for item in items {
                if let MathNode::Space(em) = item {
                    cur_x = (cur_x + *em * fs).max(0.0);
                    continue;
                }
                let kind = atom_kind_of(item);
                if let Some(pk) = prev_kind {
                    cur_x += inter_atom_gap(pk, kind, fs);
                }
                let mut sub = layout_node(item, fs, display, measurer);
                translate_primitives(&mut sub.primitives, cur_x, 0.0);
                primitives.extend(sub.primitives);
                cur_x += sub.width;
                max_asc = max_asc.max(sub.ascent);
                max_desc = max_desc.max(sub.descent);
                prev_kind = Some(kind);
            }
            MathLayout {
                width: cur_x,
                ascent: max_asc,
                descent: max_desc,
                primitives,
            }
        }
        MathNode::Frac { num, den, has_bar } => {
            let child_fs = (fs * if display { 0.88 } else { 0.76 }).max(8.5);
            let mut num_l = layout_node(num, child_fs, false, measurer);
            let mut den_l = layout_node(den, child_fs, false, measurer);
            let side_pad = fs * 0.16;
            let bar_w = num_l.width.max(den_l.width) + side_pad;
            let gap = fs * 0.15;

            let num_x = (bar_w - num_l.width) * 0.5;
            let num_base_y = axis_y - gap - num_l.descent;
            let den_x = (bar_w - den_l.width) * 0.5;
            let den_base_y = axis_y + gap + den_l.ascent;

            translate_primitives(&mut num_l.primitives, num_x, num_base_y);
            translate_primitives(&mut den_l.primitives, den_x, den_base_y);

            let mut primitives = Vec::with_capacity(num_l.primitives.len() + den_l.primitives.len() + 1);
            primitives.extend(num_l.primitives);
            primitives.extend(den_l.primitives);
            if *has_bar {
                primitives.push(MathPrimitive::Line {
                    x0: 0.0,
                    y0: axis_y,
                    x1: bar_w,
                    y1: axis_y,
                    stroke_width: stroke_w,
                });
            }

            let ascent = (-(num_base_y - num_l.ascent)).max(fs * 0.75);
            let descent = (den_base_y + den_l.descent).max(fs * 0.25);
            MathLayout {
                width: bar_w + fs * 0.06,
                ascent,
                descent,
                primitives,
            }
        }
        MathNode::Sqrt { degree, radicand } => {
            let mut rad = layout_node(radicand, fs, display, measurer);
            let deg_l = degree
                .as_ref()
                .map(|d| layout_node(d, (fs * 0.62).max(8.0), false, measurer));

            let deg_w = deg_l
                .as_ref()
                .map(|d| (d.width - fs * 0.12).max(fs * 0.22))
                .unwrap_or(0.0);
            let hook_w = (fs * 0.62).max(8.5);
            let top_pad = fs * 0.18;
            let rad_left = deg_w + hook_w + fs * 0.06;
            let vinculum_y = -(rad.ascent + top_pad);
            let bottom_y = rad.descent + fs * 0.05;
            let end_x = rad_left + rad.width + fs * 0.10;

            let mut primitives = Vec::new();
            if let Some(mut d) = deg_l {
                let dx = (deg_w + hook_w * 0.25 - d.width).max(0.0);
                let dy = -fs * 0.42 - d.descent;
                translate_primitives(&mut d.primitives, dx, dy);
                primitives.extend(d.primitives);
            }

            let notch_y = (-fs * 0.26).max(vinculum_y * 0.45);
            primitives.push(MathPrimitive::Polyline {
                points: vec![
                    (deg_w + hook_w * 0.05, notch_y + fs * 0.06),
                    (deg_w + hook_w * 0.24, notch_y),
                    (deg_w + hook_w * 0.54, bottom_y),
                    (deg_w + hook_w, vinculum_y),
                    (end_x, vinculum_y),
                ],
                stroke_width: stroke_w,
            });

            translate_primitives(&mut rad.primitives, rad_left, 0.0);
            primitives.extend(rad.primitives);

            MathLayout {
                width: end_x + fs * 0.05,
                ascent: -vinculum_y + stroke_w,
                descent: bottom_y + stroke_w,
                primitives,
            }
        }
        MathNode::Scripts {
            base,
            sub,
            sup,
            limits,
        } => {
            let mut base_l = layout_node(base, fs, display, measurer);
            let script_fs = (fs * 0.70).max(8.0);
            let mut sub_l = sub
                .as_ref()
                .map(|s| layout_node(s, script_fs, false, measurer));
            let mut sup_l = sup
                .as_ref()
                .map(|s| layout_node(s, script_fs, false, measurer));

            if *limits && display {
                let mut total_w = base_l.width;
                if let Some(ref s) = sup_l {
                    total_w = total_w.max(s.width);
                }
                if let Some(ref s) = sub_l {
                    total_w = total_w.max(s.width);
                }
                let gap = fs * 0.12;
                let mut asc = base_l.ascent;
                let mut desc = base_l.descent;
                let mut primitives = Vec::new();

                let base_x = (total_w - base_l.width) * 0.5;
                translate_primitives(&mut base_l.primitives, base_x, 0.0);
                primitives.extend(base_l.primitives);

                if let Some(mut s) = sup_l {
                    let sx = (total_w - s.width) * 0.5;
                    let sy = -base_l.ascent - gap - s.descent;
                    asc = asc.max(-(sy - s.ascent));
                    translate_primitives(&mut s.primitives, sx, sy);
                    primitives.extend(s.primitives);
                }
                if let Some(mut s) = sub_l {
                    let sx = (total_w - s.width) * 0.5;
                    let sy = base_l.descent + gap + s.ascent;
                    desc = desc.max(sy + s.descent);
                    translate_primitives(&mut s.primitives, sx, sy);
                    primitives.extend(s.primitives);
                }

                return MathLayout {
                    width: total_w + fs * 0.08,
                    ascent: asc,
                    descent: desc,
                    primitives,
                };
            }

            let sx = base_l.width + fs * 0.04;
            let mut max_script_w = 0.0_f32;
            let mut asc = base_l.ascent;
            let mut desc = base_l.descent;
            let mut primitives = base_l.primitives;

            let mut sup_y = -(base_l.ascent * 0.56).max(fs * 0.36);
            let mut sub_y = (base_l.descent * 0.65 + fs * 0.24).max(fs * 0.22);

            if let (Some(ref sp), Some(ref sb)) = (&sup_l, &sub_l) {
                let sup_bot = sup_y + sp.descent;
                let sub_top = sub_y - sb.ascent;
                let min_gap = fs * 0.12;
                if sup_bot + min_gap > sub_top {
                    let push = (sup_bot + min_gap - sub_top) * 0.5;
                    sup_y -= push;
                    sub_y += push;
                }
            }

            if let Some(mut sp) = sup_l.take() {
                max_script_w = max_script_w.max(sp.width);
                asc = asc.max(-(sup_y - sp.ascent));
                translate_primitives(&mut sp.primitives, sx, sup_y);
                primitives.extend(sp.primitives);
            }
            if let Some(mut sb) = sub_l.take() {
                max_script_w = max_script_w.max(sb.width);
                desc = desc.max(sub_y + sb.descent);
                translate_primitives(&mut sb.primitives, sx, sub_y);
                primitives.extend(sb.primitives);
            }

            MathLayout {
                width: sx + max_script_w + fs * 0.04,
                ascent: asc,
                descent: desc,
                primitives,
            }
        }
        MathNode::Delimited { left, right, body } => {
            let mut inner = layout_node(body, fs, display, measurer);
            let top_y = -(inner.ascent + fs * 0.05);
            let bottom_y = inner.descent + fs * 0.05;
            let total_h = (bottom_y - top_y).max(fs);
            let mut cur_x = 0.0_f32;
            let mut primitives = Vec::new();

            if let Some(ld) = left {
                if let Some(bk) = parse_bracket_kind(ld, true) {
                    let bw = bracket_width_for_height(bk, total_h, fs);
                    primitives.push(MathPrimitive::Bracket {
                        kind: bk,
                        x: cur_x,
                        top_y,
                        bottom_y,
                        width: bw,
                        stroke_width: stroke_w,
                    });
                    cur_x += bw + fs * 0.06;
                } else {
                    let (w, _, _) = measurer.measure(ld, fs, false, false, false);
                    primitives.push(MathPrimitive::Text {
                        x: cur_x,
                        baseline_y: 0.0,
                        text: ld.clone(),
                        font_size: fs,
                        italic: false,
                        bold: false,
                        monospace: false,
                    });
                    cur_x += w + fs * 0.06;
                }
            }

            translate_primitives(&mut inner.primitives, cur_x, 0.0);
            primitives.extend(inner.primitives);
            cur_x += inner.width;

            if let Some(rd) = right {
                cur_x += fs * 0.06;
                if let Some(bk) = parse_bracket_kind(rd, false) {
                    let bw = bracket_width_for_height(bk, total_h, fs);
                    primitives.push(MathPrimitive::Bracket {
                        kind: bk,
                        x: cur_x,
                        top_y,
                        bottom_y,
                        width: bw,
                        stroke_width: stroke_w,
                    });
                    cur_x += bw;
                } else {
                    let (w, _, _) = measurer.measure(rd, fs, false, false, false);
                    primitives.push(MathPrimitive::Text {
                        x: cur_x,
                        baseline_y: 0.0,
                        text: rd.clone(),
                        font_size: fs,
                        italic: false,
                        bold: false,
                        monospace: false,
                    });
                    cur_x += w;
                }
            }

            MathLayout {
                width: cur_x + fs * 0.04,
                ascent: -top_y + stroke_w,
                descent: bottom_y + stroke_w,
                primitives,
            }
        }
        MathNode::Accent { kind, body } => {
            let mut inner = layout_node(body, fs, display, measurer);
            let w = inner.width.max(fs * 0.45);
            let mut asc = inner.ascent;
            let mut desc = inner.descent;
            let mut primitives = Vec::new();
            let offset_x = (w - inner.width) * 0.5;
            translate_primitives(&mut inner.primitives, offset_x, 0.0);
            primitives.extend(inner.primitives);

            let top_y = -(asc + fs * 0.14);
            match kind {
                AccentKind::Overline => {
                    primitives.push(MathPrimitive::Line {
                        x0: 0.0,
                        y0: top_y,
                        x1: w,
                        y1: top_y,
                        stroke_width: stroke_w,
                    });
                    asc = (-top_y + fs * 0.06).max(asc);
                }
                AccentKind::Underline => {
                    let bot_y = desc + fs * 0.12;
                    primitives.push(MathPrimitive::Line {
                        x0: 0.0,
                        y0: bot_y,
                        x1: w,
                        y1: bot_y,
                        stroke_width: stroke_w,
                    });
                    desc = (bot_y + fs * 0.06).max(desc);
                }
                AccentKind::Hat => {
                    let peak_y = top_y - fs * 0.16;
                    primitives.push(MathPrimitive::Polyline {
                        points: vec![(0.0, top_y), (w * 0.5, peak_y), (w, top_y)],
                        stroke_width: stroke_w,
                    });
                    asc = (-peak_y + fs * 0.05).max(asc);
                }
                AccentKind::Vector => {
                    let arrow_y = top_y - fs * 0.06;
                    let tip_x = w;
                    let head = fs * 0.15;
                    primitives.push(MathPrimitive::Line {
                        x0: 0.0,
                        y0: arrow_y,
                        x1: tip_x,
                        y1: arrow_y,
                        stroke_width: stroke_w,
                    });
                    primitives.push(MathPrimitive::Polyline {
                        points: vec![
                            (tip_x - head, arrow_y - head * 0.65),
                            (tip_x, arrow_y),
                            (tip_x - head, arrow_y + head * 0.65),
                        ],
                        stroke_width: stroke_w,
                    });
                    asc = (-arrow_y + head).max(asc);
                }
                AccentKind::LeftVector => {
                    let arrow_y = top_y - fs * 0.06;
                    let head = fs * 0.15;
                    primitives.push(MathPrimitive::Line {
                        x0: 0.0,
                        y0: arrow_y,
                        x1: w,
                        y1: arrow_y,
                        stroke_width: stroke_w,
                    });
                    primitives.push(MathPrimitive::Polyline {
                        points: vec![
                            (head, arrow_y - head * 0.65),
                            (0.0, arrow_y),
                            (head, arrow_y + head * 0.65),
                        ],
                        stroke_width: stroke_w,
                    });
                    asc = (-arrow_y + head).max(asc);
                }
                AccentKind::Tilde => {
                    let amp = fs * 0.07;
                    let mid_y = top_y - amp;
                    primitives.push(MathPrimitive::Polyline {
                        points: vec![
                            (0.0, mid_y + amp * 0.5),
                            (w * 0.25, mid_y - amp),
                            (w * 0.5, mid_y),
                            (w * 0.75, mid_y + amp),
                            (w, mid_y - amp * 0.5),
                        ],
                        stroke_width: stroke_w,
                    });
                    asc = (-(mid_y - amp) + fs * 0.05).max(asc);
                }
                AccentKind::Dot => {
                    let cy = top_y - fs * 0.06;
                    let r = (fs * 0.07).max(1.2);
                    primitives.push(MathPrimitive::FilledCircle { cx: w * 0.5, cy, radius: r });
                    asc = (-cy + r + 1.0).max(asc);
                }
                AccentKind::DDot => {
                    let cy = top_y - fs * 0.06;
                    let r = (fs * 0.065).max(1.1);
                    let dx = (w * 0.18).max(r * 2.0);
                    primitives.push(MathPrimitive::FilledCircle {
                        cx: w * 0.5 - dx,
                        cy,
                        radius: r,
                    });
                    primitives.push(MathPrimitive::FilledCircle {
                        cx: w * 0.5 + dx,
                        cy,
                        radius: r,
                    });
                    asc = (-cy + r + 1.0).max(asc);
                }
            }

            MathLayout {
                width: w + fs * 0.06,
                ascent: asc,
                descent: desc,
                primitives,
            }
        }
        MathNode::Boxed(body) => {
            let mut inner = layout_node(body, fs, display, measurer);
            let pad = (fs * 0.28).max(4.0);
            let top_y = -(inner.ascent + pad);
            let bot_y = inner.descent + pad;
            let total_w = inner.width + pad * 2.0;
            translate_primitives(&mut inner.primitives, pad, 0.0);
            let mut primitives = inner.primitives;
            primitives.push(MathPrimitive::Polyline {
                points: vec![
                    (0.0, top_y),
                    (total_w, top_y),
                    (total_w, bot_y),
                    (0.0, bot_y),
                    (0.0, top_y),
                ],
                stroke_width: stroke_w,
            });
            MathLayout {
                width: total_w + fs * 0.06,
                ascent: -top_y + stroke_w,
                descent: bot_y + stroke_w,
                primitives,
            }
        }
        MathNode::Matrix { env, rows } => {
            let num_rows = rows.len().max(1);
            let num_cols = rows.iter().map(|r| r.len()).max().unwrap_or(1).max(1);
            let cell_fs = if num_rows > 2 {
                (fs * 0.90).max(9.0)
            } else {
                fs
            };

            let mut laid_rows: Vec<Vec<MathLayout>> = Vec::with_capacity(num_rows);
            let mut col_widths = vec![0.0_f32; num_cols];
            let mut row_asc = vec![cell_fs * 0.72; num_rows];
            let mut row_desc = vec![cell_fs * 0.20; num_rows];

            for (r_idx, row) in rows.iter().enumerate() {
                let mut laid_cells = Vec::with_capacity(num_cols);
                for (c_idx, col_w) in col_widths.iter_mut().enumerate().take(num_cols) {
                    let cell = row.get(c_idx).unwrap_or(&MathNode::Empty);
                    let cl = layout_node(cell, cell_fs, display, measurer);
                    *col_w = col_w.max(cl.width);
                    row_asc[r_idx] = row_asc[r_idx].max(cl.ascent);
                    row_desc[r_idx] = row_desc[r_idx].max(cl.descent);
                    laid_cells.push(cl);
                }
                laid_rows.push(laid_cells);
            }

            let col_gap = if matches!(env, MatrixEnv::Aligned) {
                fs * 0.45
            } else {
                fs * 0.65
            };
            let row_gap = fs * 0.36;

            let grid_w: f32 = col_widths.iter().sum::<f32>()
                + col_gap * (num_cols.saturating_sub(1) as f32);
            let grid_h: f32 = row_asc.iter().sum::<f32>()
                + row_desc.iter().sum::<f32>()
                + row_gap * (num_rows.saturating_sub(1) as f32);

            let top_y = axis_y - grid_h * 0.5;
            let bottom_y = axis_y + grid_h * 0.5;
            let total_h = (bottom_y - top_y).max(fs);

            let (left_bk, right_bk) = match env {
                MatrixEnv::Plain | MatrixEnv::Aligned => (None, None),
                MatrixEnv::Paren => (Some(BracketKind::LeftParen), Some(BracketKind::RightParen)),
                MatrixEnv::Bracket => {
                    (Some(BracketKind::LeftBracket), Some(BracketKind::RightBracket))
                }
                MatrixEnv::Brace => (Some(BracketKind::LeftBrace), Some(BracketKind::RightBrace)),
                MatrixEnv::VBar => (Some(BracketKind::VBar), Some(BracketKind::VBar)),
                MatrixEnv::DoubleVBar => {
                    (Some(BracketKind::DoubleVBar), Some(BracketKind::DoubleVBar))
                }
                MatrixEnv::Cases => (Some(BracketKind::LeftBrace), None),
            };

            let mut cur_x = 0.0_f32;
            let mut primitives = Vec::new();
            if let Some(lb) = left_bk {
                let bw = bracket_width_for_height(lb, total_h, fs);
                primitives.push(MathPrimitive::Bracket {
                    kind: lb,
                    x: cur_x,
                    top_y: top_y - fs * 0.06,
                    bottom_y: bottom_y + fs * 0.06,
                    width: bw,
                    stroke_width: stroke_w,
                });
                cur_x += bw + fs * 0.14;
            }

            let mut cur_y = top_y;
            for (r_idx, mut row) in laid_rows.into_iter().enumerate() {
                let baseline_y = cur_y + row_asc[r_idx];
                let mut cell_x = cur_x;
                for (c_idx, mut cell) in row.drain(..).enumerate() {
                    let cw = col_widths[c_idx];
                    let align_dx = match env {
                        MatrixEnv::Cases => 0.0,
                        MatrixEnv::Aligned => {
                            if num_cols == 1 {
                                0.0
                            } else if c_idx % 2 == 0 {
                                cw - cell.width
                            } else {
                                0.0
                            }
                        }
                        _ => (cw - cell.width) * 0.5,
                    };
                    translate_primitives(&mut cell.primitives, cell_x + align_dx, baseline_y);
                    primitives.extend(cell.primitives);
                    cell_x += cw + col_gap;
                }
                cur_y += row_asc[r_idx] + row_desc[r_idx] + row_gap;
            }

            cur_x += grid_w;
            if let Some(rb) = right_bk {
                cur_x += fs * 0.14;
                let bw = bracket_width_for_height(rb, total_h, fs);
                primitives.push(MathPrimitive::Bracket {
                    kind: rb,
                    x: cur_x,
                    top_y: top_y - fs * 0.06,
                    bottom_y: bottom_y + fs * 0.06,
                    width: bw,
                    stroke_width: stroke_w,
                });
                cur_x += bw;
            }

            MathLayout {
                width: cur_x + fs * 0.06,
                ascent: (-(top_y - fs * 0.10)).max(fs * 0.75),
                descent: (bottom_y + fs * 0.10).max(fs * 0.25),
                primitives,
            }
        }
    }
}

/// Computes scalable bracket polyline points in the coordinate space of the layout.
pub(crate) fn bracket_polylines(
    kind: BracketKind,
    x: f32,
    top_y: f32,
    bottom_y: f32,
    width: f32,
) -> Vec<Vec<(f32, f32)>> {
    let h = (bottom_y - top_y).max(4.0);
    let mid_y = (top_y + bottom_y) * 0.5;
    let cubic = |p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), p3: (f32, f32), steps: usize| {
        let mut pts = Vec::with_capacity(steps + 1);
        for s in 0..=steps {
            let t = s as f32 / steps as f32;
            let u = 1.0 - t;
            let bx = u.powi(3) * p0.0
                + 3.0 * u.powi(2) * t * p1.0
                + 3.0 * u * t.powi(2) * p2.0
                + t.powi(3) * p3.0;
            let by = u.powi(3) * p0.1
                + 3.0 * u.powi(2) * t * p1.1
                + 3.0 * u * t.powi(2) * p2.1
                + t.powi(3) * p3.1;
            pts.push((bx, by));
        }
        pts
    };

    match kind {
        BracketKind::LeftParen => vec![cubic(
            (x + width * 0.88, top_y),
            (x + width * 0.10, top_y + h * 0.25),
            (x + width * 0.10, bottom_y - h * 0.25),
            (x + width * 0.88, bottom_y),
            12,
        )],
        BracketKind::RightParen => vec![cubic(
            (x + width * 0.12, top_y),
            (x + width * 0.90, top_y + h * 0.25),
            (x + width * 0.90, bottom_y - h * 0.25),
            (x + width * 0.12, bottom_y),
            12,
        )],
        BracketKind::LeftBracket => vec![vec![
            (x + width * 0.85, top_y),
            (x + width * 0.22, top_y),
            (x + width * 0.22, bottom_y),
            (x + width * 0.85, bottom_y),
        ]],
        BracketKind::RightBracket => vec![vec![
            (x + width * 0.15, top_y),
            (x + width * 0.78, top_y),
            (x + width * 0.78, bottom_y),
            (x + width * 0.15, bottom_y),
        ]],
        BracketKind::LeftBrace => {
            let mut top_half = cubic(
                (x + width * 0.90, top_y),
                (x + width * 0.30, top_y),
                (x + width * 0.70, mid_y),
                (x + width * 0.10, mid_y),
                8,
            );
            let bot_half = cubic(
                (x + width * 0.10, mid_y),
                (x + width * 0.70, mid_y),
                (x + width * 0.30, bottom_y),
                (x + width * 0.90, bottom_y),
                8,
            );
            top_half.extend(bot_half.into_iter().skip(1));
            vec![top_half]
        }
        BracketKind::RightBrace => {
            let mut top_half = cubic(
                (x + width * 0.10, top_y),
                (x + width * 0.70, top_y),
                (x + width * 0.30, mid_y),
                (x + width * 0.90, mid_y),
                8,
            );
            let bot_half = cubic(
                (x + width * 0.90, mid_y),
                (x + width * 0.30, mid_y),
                (x + width * 0.70, bottom_y),
                (x + width * 0.10, bottom_y),
                8,
            );
            top_half.extend(bot_half.into_iter().skip(1));
            vec![top_half]
        }
        BracketKind::VBar => vec![vec![(x + width * 0.5, top_y), (x + width * 0.5, bottom_y)]],
        BracketKind::DoubleVBar => vec![
            vec![(x + width * 0.32, top_y), (x + width * 0.32, bottom_y)],
            vec![(x + width * 0.68, top_y), (x + width * 0.68, bottom_y)],
        ],
        BracketKind::LeftAngle => vec![vec![
            (x + width * 0.85, top_y),
            (x + width * 0.15, mid_y),
            (x + width * 0.85, bottom_y),
        ]],
        BracketKind::RightAngle => vec![vec![
            (x + width * 0.15, top_y),
            (x + width * 0.85, mid_y),
            (x + width * 0.15, bottom_y),
        ]],
        BracketKind::LeftFloor => vec![vec![
            (x + width * 0.25, top_y),
            (x + width * 0.25, bottom_y),
            (x + width * 0.85, bottom_y),
        ]],
        BracketKind::RightFloor => vec![vec![
            (x + width * 0.75, top_y),
            (x + width * 0.75, bottom_y),
            (x + width * 0.15, bottom_y),
        ]],
        BracketKind::LeftCeil => vec![vec![
            (x + width * 0.85, top_y),
            (x + width * 0.25, top_y),
            (x + width * 0.25, bottom_y),
        ]],
        BracketKind::RightCeil => vec![vec![
            (x + width * 0.15, top_y),
            (x + width * 0.75, top_y),
            (x + width * 0.75, bottom_y),
        ]],
    }
}

/// Computes a 2D `MathLayout` using the offscreen font measurer (used in PDF/DOCX export & tests).
pub fn layout_math(node: &MathNode, base_font_size: f32, display_mode: bool) -> MathLayout {
    let Ok(guard) = offscreen_fonts_2x().lock() else {
        return MathLayout {
            width: 0.0,
            ascent: base_font_size,
            descent: base_font_size * 0.25,
            primitives: Vec::new(),
        };
    };
    let mut measurer = FontsMeasurer(&guard);
    layout_node(node, base_font_size, display_mode, &mut measurer)
}

struct EguiMeasurer<'a>(&'a egui::Context);
impl TextMeasurer for EguiMeasurer<'_> {
    fn measure(&mut self, text: &str, font_size: f32, italic: bool, bold: bool, mono: bool) -> (f32, f32, f32) {
        self.0
            .fonts(|f| measure_text_with_fonts(f, text, font_size, italic, bold, mono))
    }
}

fn hash_latex_str(latex: &str) -> u64 {
    let mut h = FxHasher::default();
    latex.hash(&mut h);
    h.finish()
}

#[derive(Clone)]
struct CachedMathLayout {
    latex: String,
    layout: Arc<MathLayout>,
    last_tick: u64,
}

#[derive(Clone, Default)]
struct MathLayoutCache {
    atlas: Weak<egui::epaint::mutex::Mutex<egui::epaint::TextureAtlas>>,
    tick: u64,
    entries: FxHashMap<(u64, u32, bool, u32), CachedMathLayout>,
}

impl MathLayoutCache {
    const MAX_ENTRIES: usize = 256;

    fn sync_atlas(&mut self, cur_atlas: &Arc<egui::epaint::mutex::Mutex<egui::epaint::TextureAtlas>>) {
        if self.atlas.as_ptr() != Arc::as_ptr(cur_atlas) {
            self.entries.clear();
            self.atlas = Arc::downgrade(cur_atlas);
        }
    }

    fn get(&mut self, key: (u64, u32, bool, u32), latex: &str) -> Option<Arc<MathLayout>> {
        self.tick = self.tick.wrapping_add(1);
        let tick = self.tick;
        let entry = self.entries.get_mut(&key)?;
        if entry.latex == latex {
            entry.last_tick = tick;
            Some(Arc::clone(&entry.layout))
        } else {
            None
        }
    }

    fn insert(&mut self, key: (u64, u32, bool, u32), latex: &str, layout: Arc<MathLayout>) {
        if self.entries.len() >= Self::MAX_ENTRIES && !self.entries.contains_key(&key) {
            let mut ticks: Vec<u64> = self.entries.values().map(|e| e.last_tick).collect();
            let mid = ticks.len() / 2;
            let (_, cutoff, _) = ticks.select_nth_unstable(mid);
            let cutoff_val = *cutoff;
            self.entries.retain(|_, v| v.last_tick > cutoff_val);
            if self.entries.len() >= Self::MAX_ENTRIES {
                self.entries.clear();
            }
        }
        self.tick = self.tick.wrapping_add(1);
        self.entries.insert(
            key,
            CachedMathLayout {
                latex: latex.to_string(),
                layout,
                last_tick: self.tick,
            },
        );
    }
}

/// Renders a LaTeX math expression in `egui` with full 2D visual typesetting.
pub fn render_math_in_ui(
    ui: &mut egui::Ui,
    latex: &str,
    base_font_size: f32,
    display_mode: bool,
    text_color: Color32,
    theme: &WordTheme,
) -> Response {
    let trimmed = latex.trim();
    if trimmed.is_empty() {
        let hint = if display_mode {
            "(empty math block)"
        } else {
            "∑"
        };
        return ui.label(egui::RichText::new(hint).size(base_font_size * 0.85).color(theme.text_muted));
    }

    let cur_atlas = ui.fonts(|f| f.texture_atlas());
    let ppp_bits = ui.ctx().pixels_per_point().to_bits();
    let cache_key = (
        hash_latex_str(trimmed),
        base_font_size.to_bits(),
        display_mode,
        ppp_bits,
    );
    let cache_id = Id::new("md_math_layout_cache_v1");
    let cached_layout = ui.ctx().data_mut(|d| {
        let cache = d.get_temp_mut_or_default::<MathLayoutCache>(cache_id);
        cache.sync_atlas(&cur_atlas);
        cache.get(cache_key, trimmed)
    });
    let layout = if let Some(hit) = cached_layout {
        hit
    } else {
        let ast = parse_latex_math(trimmed);
        let built = {
            let mut measurer = EguiMeasurer(ui.ctx());
            Arc::new(layout_node(&ast, base_font_size, display_mode, &mut measurer))
        };
        ui.ctx().data_mut(|d| {
            let cache = d.get_temp_mut_or_default::<MathLayoutCache>(cache_id);
            cache.insert(cache_key, trimmed, Arc::clone(&built));
        });
        built
    };

    let pad_x = if display_mode { 8.0 } else { 2.5 };
    let pad_y = if display_mode { 6.0 } else { 1.5 };
    let total_size = Vec2::new(
        (layout.width + pad_x * 2.0).max(8.0),
        (layout.total_height() + pad_y * 2.0).max(base_font_size),
    );
    let (rect, resp) = ui.allocate_exact_size(total_size, Sense::hover());

    if ui.is_rect_visible(rect) {
        let origin = Pos2::new(rect.left() + pad_x, rect.top() + pad_y + layout.ascent);
        let painter = ui.painter();

        for prim in &layout.primitives {
            match prim {
                MathPrimitive::Text {
                    x,
                    baseline_y,
                    text,
                    font_size,
                    italic,
                    bold,
                    monospace,
                } => {
                    let mut job = LayoutJob::default();
                    job.wrap.max_width = f32::INFINITY;
                    job.append(
                        text,
                        0.0,
                        TextFormat {
                            font_id: make_font_id(*font_size, *bold, *monospace),
                            color: text_color,
                            italics: *italic,
                            ..Default::default()
                        },
                    );
                    let galley = ui.fonts(|f| f.layout_job(job));
                    let first_ascent = galley
                        .rows
                        .first()
                        .and_then(|r| r.glyphs.first().map(|g| g.font_ascent))
                        .unwrap_or(font_size * 0.72);
                    let pos = Pos2::new(origin.x + *x, origin.y + *baseline_y - first_ascent);
                    painter.galley(pos, galley, text_color);
                }
                MathPrimitive::Line {
                    x0,
                    y0,
                    x1,
                    y1,
                    stroke_width,
                } => {
                    painter.line_segment(
                        [
                            Pos2::new(origin.x + *x0, origin.y + *y0),
                            Pos2::new(origin.x + *x1, origin.y + *y1),
                        ],
                        Stroke::new(*stroke_width, text_color),
                    );
                }
                MathPrimitive::Polyline {
                    points,
                    stroke_width,
                } => {
                    let pts: Vec<Pos2> = points
                        .iter()
                        .map(|&(px, py)| Pos2::new(origin.x + px, origin.y + py))
                        .collect();
                    painter.add(egui::Shape::line(pts, Stroke::new(*stroke_width, text_color)));
                }
                MathPrimitive::FilledCircle { cx, cy, radius } => {
                    painter.circle_filled(
                        Pos2::new(origin.x + *cx, origin.y + *cy),
                        *radius,
                        text_color,
                    );
                }
                MathPrimitive::Bracket {
                    kind,
                    x,
                    top_y,
                    bottom_y,
                    width,
                    stroke_width,
                } => {
                    for poly in bracket_polylines(*kind, *x, *top_y, *bottom_y, *width) {
                        let pts: Vec<Pos2> = poly
                            .into_iter()
                            .map(|(px, py)| Pos2::new(origin.x + px, origin.y + py))
                            .collect();
                        painter.add(egui::Shape::line(pts, Stroke::new(*stroke_width, text_color)));
                    }
                }
            }
        }
    }
    resp
}

/// Rasterizes a 2D LaTeX equation to an RGBA8 pixel buffer `(rgba_bytes, width_px, height_px)` at 2x DPI.
pub fn render_math_to_rgba(
    latex: &str,
    base_font_size: f32,
    display_mode: bool,
    fg: [u8; 4],
    bg: [u8; 4],
) -> (Vec<u8>, u32, u32) {
    let scale = 2.0_f32;
    let ast = parse_latex_math(latex.trim());
    let Ok(fonts_guard) = offscreen_fonts_2x().lock() else {
        return (vec![bg[0], bg[1], bg[2], bg[3]], 1, 1);
    };
    let layout = {
        let mut measurer = FontsMeasurer(&fonts_guard);
        layout_node(&ast, base_font_size, display_mode, &mut measurer)
    };
    let pad = if display_mode { 8.0 } else { 3.0 };
    let logical_w = (layout.width + pad * 2.0).max(8.0);
    let logical_h = (layout.total_height() + pad * 2.0).max(base_font_size);

    let mut galleys: Vec<(f32, f32, std::sync::Arc<Galley>)> = Vec::new();
    let fg_col = Color32::from_rgba_unmultiplied(fg[0], fg[1], fg[2], fg[3]);
    let origin_x = pad;
    let origin_y = pad + layout.ascent;

    for prim in &layout.primitives {
        if let MathPrimitive::Text {
            x,
            baseline_y,
            text,
            font_size,
            italic,
            bold,
            monospace,
        } = prim
        {
            let mut job = LayoutJob::default();
            job.wrap.max_width = f32::INFINITY;
            job.append(
                text,
                0.0,
                TextFormat {
                    font_id: make_font_id(*font_size, *bold, *monospace),
                    color: fg_col,
                    italics: *italic,
                    ..Default::default()
                },
            );
            let galley = fonts_guard.layout_job(job);
            let first_ascent = galley
                .rows
                .first()
                .and_then(|r| r.glyphs.first().map(|g| g.font_ascent))
                .unwrap_or(font_size * 0.72);
            galleys.push((origin_x + *x, origin_y + *baseline_y - first_ascent, galley));
        }
    }
    let font_img = fonts_guard.image();
    drop(fonts_guard);

    let mut canvas = RgbaCanvas::new(logical_w, logical_h, scale, bg);
    for (gx, gy, galley) in &galleys {
        canvas.draw_galley(galley, *gx, *gy, &font_img);
    }
    for prim in &layout.primitives {
        match prim {
            MathPrimitive::Line {
                x0,
                y0,
                x1,
                y1,
                stroke_width,
            } => {
                canvas.draw_line(
                    origin_x + *x0,
                    origin_y + *y0,
                    origin_x + *x1,
                    origin_y + *y1,
                    *stroke_width,
                    fg,
                );
            }
            MathPrimitive::Polyline {
                points,
                stroke_width,
            } => {
                let pts: Vec<(f32, f32)> = points
                    .iter()
                    .map(|&(px, py)| (origin_x + px, origin_y + py))
                    .collect();
                canvas.draw_polyline(&pts, false, *stroke_width, fg);
            }
            MathPrimitive::FilledCircle { cx, cy, radius } => {
                canvas.fill_circle(origin_x + *cx, origin_y + *cy, *radius, fg);
            }
            MathPrimitive::Bracket {
                kind,
                x,
                top_y,
                bottom_y,
                width,
                stroke_width,
            } => {
                for poly in bracket_polylines(*kind, *x, *top_y, *bottom_y, *width) {
                    let pts: Vec<(f32, f32)> = poly
                        .into_iter()
                        .map(|(px, py)| (origin_x + px, origin_y + py))
                        .collect();
                    canvas.draw_polyline(&pts, false, *stroke_width, fg);
                }
            }
            MathPrimitive::Text { .. } => {}
        }
    }

    (canvas.data, canvas.width, canvas.height)
}

/// Renders a 2D LaTeX equation to high-DPI PNG bytes `(png_bytes, pixel_w, pixel_h)`.
pub fn render_math_to_png_bytes(
    latex: &str,
    base_font_size: f32,
    display_mode: bool,
    fg: [u8; 4],
    bg: [u8; 4],
) -> Option<(Vec<u8>, u32, u32)> {
    let (rgba, w, h) = render_math_to_rgba(latex, base_font_size, display_mode, fg, bg);
    let canvas = RgbaCanvas {
        width: w,
        height: h,
        scale: 2.0,
        data: rgba,
    };
    let png = canvas.into_png_bytes()?;
    Some((png, w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fractions_roots_scripts_matrices_and_symbols() {
        let ast = parse_latex_math(r"\frac{-b \pm \sqrt{b^2 - 4ac}}{2a}");
        assert!(matches!(ast, MathNode::Frac { has_bar: true, .. }));
        let plain = math_to_plain_text(r"\frac{-b \pm \sqrt{b^2 - 4ac}}{2a}");
        assert!(plain.contains("±"));
        assert!(plain.contains("√("));

        let lim = parse_latex_math(r"\sum_{i=1}^{n} i^2 = \frac{n(n+1)(2n+1)}{6}");
        if let MathNode::Row(ref parts) = lim {
            assert!(matches!(&parts[0], MathNode::Scripts { limits: true, .. }));
        } else {
            panic!("expected Row for sum identity");
        }

        let mat = parse_latex_math(r"\begin{pmatrix} 1 & 0 \\ 0 & 1 \end{pmatrix}");
        assert!(matches!(
            mat,
            MathNode::Matrix {
                env: MatrixEnv::Paren,
                ..
            }
        ));

        let cases = parse_latex_math(r"f(x) = \begin{cases} x^2 & x \ge 0 \\ -x & x < 0 \end{cases}");
        let cases_plain = math_to_plain_text(r"f(x) = \begin{cases} x^2 & x \ge 0 \\ -x & x < 0 \end{cases}");
        assert!(cases_plain.contains("≥"));
        assert!(matches!(cases, MathNode::Row(_)));

        let bb = math_to_plain_text(r"\forall x \in \mathbb{R}, \int_0^\infty e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}");
        assert!(bb.contains("∀"));
        assert!(bb.contains("∈"));
        assert!(bb.contains("ℝ"));
        assert!(bb.contains("∫"));
        assert!(bb.contains("∞"));
        assert!(bb.contains("π"));
    }

    #[test]
    fn layout_and_rasterizer_produce_valid_2d_bounds_and_png() {
        let expr = r"x = \left( \frac{\alpha + \beta}{\sqrt[3]{\gamma_1^2 + \hat{y}}} \right) + \begin{bmatrix} a & b \\ c & d \end{bmatrix}";
        let ast = parse_latex_math(expr);
        let layout = layout_math(&ast, 18.0, true);
        assert!(layout.width > 60.0);
        assert!(layout.ascent > 14.0);
        assert!(layout.descent > 6.0);
        assert!(layout
            .primitives
            .iter()
            .any(|p| matches!(p, MathPrimitive::Bracket { .. })));
        assert!(layout
            .primitives
            .iter()
            .any(|p| matches!(p, MathPrimitive::Line { .. })));
        assert!(layout
            .primitives
            .iter()
            .any(|p| matches!(p, MathPrimitive::Polyline { .. })));

        let (png, w, h) = render_math_to_png_bytes(
            expr,
            18.0,
            true,
            [20, 24, 32, 255],
            [255, 255, 255, 255],
        )
        .expect("png rasterization succeeds");
        assert!(w > 100 && h > 40);
        assert!(png.starts_with(b"\x89PNG"));
    }

    #[test]
    fn textbook_multiline_boxed_tag_and_templates_parse_and_layout() {
        let boxed_ast = parse_latex_math(r"\boxed{E = mc^2} \tag{1}");
        let boxed_plain = math_to_plain_text(r"\boxed{E = mc^2} \tag{1}");
        assert!(boxed_plain.contains("[E = mc^2]"));
        assert!(boxed_plain.contains("(1)"));
        let boxed_lay = layout_math(&boxed_ast, 18.0, true);
        assert!(boxed_lay
            .primitives
            .iter()
            .any(|p| matches!(p, MathPrimitive::Polyline { points, .. } if points.len() == 5)));

        let aligned_ast = parse_latex_math(r"x + 1 = 3 \\ x = 2");
        assert!(matches!(
            aligned_ast,
            MathNode::Matrix {
                env: MatrixEnv::Aligned,
                ref rows
            } if rows.len() == 2
        ));

        for (title, tex) in MATH_TEXTBOOK_TEMPLATES {
            let ast = parse_latex_math(tex);
            let lay = layout_math(&ast, 17.5, true);
            assert!(lay.width > 10.0, "template {title} failed layout");
        }
    }

    #[test]
    fn math_layout_cache_hits_and_evicts_oldest_half_when_full() {
        let mut cache = MathLayoutCache::default();
        let sample = Arc::new(layout_math(&parse_latex_math("x^2 + 1"), 17.5, true));
        for i in 0..MathLayoutCache::MAX_ENTRIES {
            let expr = format!("x_{i} + 1");
            let key = (
                hash_latex_str(&expr),
                17.5_f32.to_bits(),
                true,
                1.0_f32.to_bits(),
            );
            cache.insert(key, &expr, Arc::clone(&sample));
        }
        assert_eq!(cache.entries.len(), MathLayoutCache::MAX_ENTRIES);

        let expr0 = "x_0 + 1";
        let key0 = (
            hash_latex_str(expr0),
            17.5_f32.to_bits(),
            true,
            1.0_f32.to_bits(),
        );
        assert!(cache.get(key0, expr0).is_some());

        let extra = "x_extra + 1";
        let extra_key = (
            hash_latex_str(extra),
            17.5_f32.to_bits(),
            true,
            1.0_f32.to_bits(),
        );
        cache.insert(extra_key, extra, Arc::clone(&sample));
        assert!(cache.entries.len() < MathLayoutCache::MAX_ENTRIES);
        assert!(cache.entries.len() >= MathLayoutCache::MAX_ENTRIES / 2);
        assert!(cache.get(key0, expr0).is_some());
        assert!(cache.get(extra_key, extra).is_some());
    }
}
