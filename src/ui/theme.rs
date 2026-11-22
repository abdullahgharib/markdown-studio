use egui::{Color32, CornerRadius, Margin, Shadow, Stroke, Visuals};

/// Theme palette mode for UI chrome and document paper surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum ThemeMode {
    #[default]
    Light,
    Sepia,
    Dark,
}

/// Word-inspired visual theme palette for Light ("Classic Paper"), Sepia ("Warm Paper"), and Dark ("Midnight Studio") modes.
#[derive(Debug, Clone)]
pub struct WordTheme {
    pub mode: ThemeMode,
    pub dark: bool,
    pub desk_bg: Color32,
    pub paper_bg: Color32,
    pub paper_shadow: Color32,
    pub ribbon_bg: Color32,
    pub titlebar_bg: Color32,
    pub pill_bg: Color32,
    pub border: Color32,
    pub border_subtle: Color32,
    pub text_primary: Color32,
    pub text_muted: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub accent_soft: Color32,
    pub selection_bg: Color32,
    pub active_line_bg: Color32,
    pub code_bg: Color32,
    pub code_border: Color32,
    pub quote_bg: Color32,
    pub quote_bar: Color32,
    pub table_header_bg: Color32,
    pub table_alt_bg: Color32,
    pub heading_color: Color32,
    pub link_color: Color32,
    pub warning_amber: Color32,
    pub success_green: Color32,
    pub syn_keyword: Color32,
    pub syn_string: Color32,
    pub syn_comment: Color32,
    pub syn_number: Color32,
    pub syn_type: Color32,
}

impl WordTheme {
    pub fn from_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Light => Self::light(),
            ThemeMode::Sepia => Self::sepia(),
            ThemeMode::Dark => Self::dark(),
        }
    }

    pub fn light() -> Self {
        Self {
            mode: ThemeMode::Light,
            dark: false,
            desk_bg: Color32::from_rgb(238, 242, 247),
            paper_bg: Color32::from_rgb(255, 255, 255),
            paper_shadow: Color32::from_rgba_unmultiplied(15, 23, 42, 18),
            ribbon_bg: Color32::from_rgb(249, 250, 252),
            titlebar_bg: Color32::from_rgb(255, 255, 255),
            pill_bg: Color32::from_rgb(241, 245, 249),
            border: Color32::from_rgb(213, 221, 232),
            border_subtle: Color32::from_rgb(229, 235, 243),
            text_primary: Color32::from_rgb(24, 32, 46),
            text_muted: Color32::from_rgb(100, 116, 139),
            accent: Color32::from_rgb(37, 99, 235),
            accent_hover: Color32::from_rgb(29, 78, 216),
            accent_soft: Color32::from_rgb(224, 236, 255),
            selection_bg: Color32::from_rgba_unmultiplied(59, 130, 246, 55),
            active_line_bg: Color32::from_rgb(243, 247, 253),
            code_bg: Color32::from_rgb(245, 247, 251),
            code_border: Color32::from_rgb(223, 230, 240),
            quote_bg: Color32::from_rgb(247, 250, 255),
            quote_bar: Color32::from_rgb(59, 130, 246),
            table_header_bg: Color32::from_rgb(236, 242, 252),
            table_alt_bg: Color32::from_rgb(249, 251, 254),
            heading_color: Color32::from_rgb(18, 42, 78),
            link_color: Color32::from_rgb(29, 78, 216),
            warning_amber: Color32::from_rgb(217, 119, 6),
            success_green: Color32::from_rgb(22, 163, 74),
            syn_keyword: Color32::from_rgb(147, 51, 234),
            syn_string: Color32::from_rgb(21, 128, 61),
            syn_comment: Color32::from_rgb(100, 116, 139),
            syn_number: Color32::from_rgb(180, 83, 9),
            syn_type: Color32::from_rgb(14, 116, 144),
        }
    }

    pub fn sepia() -> Self {
        Self {
            mode: ThemeMode::Sepia,
            dark: false,
            desk_bg: Color32::from_rgb(239, 231, 214),
            paper_bg: Color32::from_rgb(250, 245, 232),
            paper_shadow: Color32::from_rgba_unmultiplied(60, 42, 20, 22),
            ribbon_bg: Color32::from_rgb(245, 239, 224),
            titlebar_bg: Color32::from_rgb(251, 247, 236),
            pill_bg: Color32::from_rgb(237, 228, 208),
            border: Color32::from_rgb(216, 204, 180),
            border_subtle: Color32::from_rgb(230, 220, 199),
            text_primary: Color32::from_rgb(51, 40, 30),
            text_muted: Color32::from_rgb(120, 103, 84),
            accent: Color32::from_rgb(180, 83, 9),
            accent_hover: Color32::from_rgb(146, 64, 14),
            accent_soft: Color32::from_rgb(243, 225, 196),
            selection_bg: Color32::from_rgba_unmultiplied(180, 83, 9, 50),
            active_line_bg: Color32::from_rgb(245, 236, 219),
            code_bg: Color32::from_rgb(242, 233, 213),
            code_border: Color32::from_rgb(221, 208, 182),
            quote_bg: Color32::from_rgb(245, 236, 219),
            quote_bar: Color32::from_rgb(180, 83, 9),
            table_header_bg: Color32::from_rgb(237, 226, 204),
            table_alt_bg: Color32::from_rgb(246, 239, 224),
            heading_color: Color32::from_rgb(74, 46, 20),
            link_color: Color32::from_rgb(154, 67, 10),
            warning_amber: Color32::from_rgb(180, 83, 9),
            success_green: Color32::from_rgb(21, 128, 61),
            syn_keyword: Color32::from_rgb(159, 43, 43),
            syn_string: Color32::from_rgb(47, 111, 56),
            syn_comment: Color32::from_rgb(126, 116, 88),
            syn_number: Color32::from_rgb(180, 83, 9),
            syn_type: Color32::from_rgb(109, 40, 118),
        }
    }

    pub fn dark() -> Self {
        Self {
            mode: ThemeMode::Dark,
            dark: true,
            desk_bg: Color32::from_rgb(14, 18, 27),
            paper_bg: Color32::from_rgb(22, 27, 38),
            paper_shadow: Color32::from_rgba_unmultiplied(0, 0, 0, 70),
            ribbon_bg: Color32::from_rgb(26, 32, 44),
            titlebar_bg: Color32::from_rgb(21, 26, 36),
            pill_bg: Color32::from_rgb(34, 42, 58),
            border: Color32::from_rgb(48, 58, 76),
            border_subtle: Color32::from_rgb(38, 46, 61),
            text_primary: Color32::from_rgb(230, 235, 245),
            text_muted: Color32::from_rgb(148, 163, 184),
            accent: Color32::from_rgb(96, 165, 250),
            accent_hover: Color32::from_rgb(59, 130, 246),
            accent_soft: Color32::from_rgb(33, 53, 88),
            selection_bg: Color32::from_rgba_unmultiplied(96, 165, 250, 65),
            active_line_bg: Color32::from_rgb(29, 36, 50),
            code_bg: Color32::from_rgb(17, 21, 30),
            code_border: Color32::from_rgb(44, 53, 70),
            quote_bg: Color32::from_rgb(27, 34, 49),
            quote_bar: Color32::from_rgb(96, 165, 250),
            table_header_bg: Color32::from_rgb(35, 45, 64),
            table_alt_bg: Color32::from_rgb(26, 32, 45),
            heading_color: Color32::from_rgb(191, 219, 254),
            link_color: Color32::from_rgb(125, 180, 255),
            warning_amber: Color32::from_rgb(251, 191, 36),
            success_green: Color32::from_rgb(74, 222, 128),
            syn_keyword: Color32::from_rgb(192, 132, 252),
            syn_string: Color32::from_rgb(134, 239, 172),
            syn_comment: Color32::from_rgb(120, 136, 160),
            syn_number: Color32::from_rgb(253, 186, 116),
            syn_type: Color32::from_rgb(103, 232, 249),
        }
    }

    pub fn paper_shadow(&self) -> Shadow {
        Shadow {
            offset: [0, 4],
            blur: 16,
            spread: 1,
            color: self.paper_shadow,
        }
    }

    pub fn apply_to_ctx(&self, ctx: &egui::Context) {
        let mut visuals = if self.dark {
            Visuals::dark()
        } else {
            Visuals::light()
        };
        visuals.panel_fill = self.desk_bg;
        visuals.window_fill = self.paper_bg;
        visuals.window_corner_radius = CornerRadius::same(8);
        visuals.window_shadow = Shadow {
            offset: [0, 8],
            blur: 24,
            spread: 2,
            color: self.paper_shadow,
        };
        visuals.window_stroke = Stroke::new(1.0_f32, self.border);

        visuals.widgets.noninteractive.bg_fill = self.ribbon_bg;
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, self.border_subtle);
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, self.text_primary);
        visuals.widgets.noninteractive.corner_radius = CornerRadius::same(6);

        visuals.widgets.inactive.bg_fill = self.pill_bg;
        visuals.widgets.inactive.weak_bg_fill = self.pill_bg;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, self.border_subtle);
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, self.text_primary);
        visuals.widgets.inactive.corner_radius = CornerRadius::same(6);

        visuals.widgets.hovered.bg_fill = self.accent_soft;
        visuals.widgets.hovered.weak_bg_fill = self.accent_soft;
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, self.accent);
        visuals.widgets.hovered.fg_stroke = Stroke::new(1.2_f32, self.text_primary);
        visuals.widgets.hovered.corner_radius = CornerRadius::same(6);

        visuals.widgets.active.bg_fill = self.accent_soft;
        visuals.widgets.active.weak_bg_fill = self.accent_soft;
        visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, self.accent);
        visuals.widgets.active.fg_stroke = Stroke::new(1.5_f32, self.text_primary);
        visuals.widgets.active.corner_radius = CornerRadius::same(6);

        visuals.selection.bg_fill = self.accent_soft;
        visuals.selection.stroke = Stroke::new(1.0_f32, self.accent);
        ctx.set_visuals(visuals);

        let mut style = (*ctx.style()).clone();
        style.spacing.button_padding = egui::vec2(9.0, 4.5);
        style.spacing.item_spacing = egui::vec2(6.0, 5.0);
        style.spacing.window_margin = Margin::same(12);
        style.spacing.scroll.bar_width = 8.0;
        style.spacing.scroll.bar_inner_margin = 3.0;
        ctx.set_style(style);
    }
}
