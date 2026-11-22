use eframe::egui;
use egui::{Color32, CursorIcon, Id, Key, LayerId, Order, PointerButton, Pos2, Rect, Shape, Stroke, Vec2};

use crate::ui::theme::WordTheme;

const DEADZONE_PX: f32 = 12.0;
const HOLD_RELEASE_THRESHOLD_SECS: f64 = 0.25;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutoScrollTarget {
    #[default]
    Editor,
    Reader,
}

/// State machine & renderer for middle-mouse-button two-arrow fast auto-scrolling.
#[derive(Debug, Default)]
pub struct AutoScrollState {
    pub active: bool,
    pub target: AutoScrollTarget,
    pub anchor: Pos2,
    pub suppress_pane_clicks_this_frame: bool,
    pressed_at: f64,
    dragged_beyond_deadzone: bool,
}

impl AutoScrollState {
    pub fn cancel(&mut self) {
        self.active = false;
        self.dragged_beyond_deadzone = false;
        self.suppress_pane_clicks_this_frame = false;
    }

    /// Updates middle-click auto-scroll state, paints the two-arrow indicator on the foreground
    /// layer when active, and returns `Some((target, delta_y_px))` if active.
    pub fn update_and_paint(
        &mut self,
        ctx: &egui::Context,
        theme: &WordTheme,
        modal_open: bool,
        editor_rect: Option<Rect>,
        reader_rect: Option<Rect>,
    ) -> Option<(AutoScrollTarget, f32)> {
        self.suppress_pane_clicks_this_frame = false;
        if modal_open {
            self.active = false;
            return None;
        }

        let (
            pointer_pos,
            now,
            dt,
            middle_pressed,
            middle_released,
            any_button_pressed,
            raw_wheel_y,
            esc_pressed,
        ) = ctx.input(|i| {
            let pos = i.pointer.latest_pos().or_else(|| i.pointer.interact_pos());
            let middle_p = i.pointer.button_pressed(PointerButton::Middle);
            let primary_p = i.pointer.button_pressed(PointerButton::Primary);
            let secondary_p = i.pointer.button_pressed(PointerButton::Secondary);
            (
                pos,
                i.time,
                i.stable_dt.clamp(0.001, 0.05),
                middle_p,
                i.pointer.button_released(PointerButton::Middle),
                middle_p || primary_p || secondary_p,
                i.raw_scroll_delta.y,
                i.key_pressed(Key::Escape),
            )
        });

        if self.active {
            let should_exit = esc_pressed
                || any_button_pressed
                || (middle_released
                    && (self.dragged_beyond_deadzone
                        || (now - self.pressed_at) > HOLD_RELEASE_THRESHOLD_SECS))
                || raw_wheel_y.abs() > 0.1;

            if should_exit {
                if esc_pressed {
                    ctx.input_mut(|i| {
                        i.consume_key(egui::Modifiers::NONE, Key::Escape);
                    });
                }
                self.active = false;
                self.dragged_beyond_deadzone = false;
                self.suppress_pane_clicks_this_frame = any_button_pressed;
                return None;
            }

            self.suppress_pane_clicks_this_frame = true;
            let cur_pos = pointer_pos.unwrap_or(self.anchor);
            let dy = cur_pos.y - self.anchor.y;
            if dy.abs() > DEADZONE_PX {
                self.dragged_beyond_deadzone = true;
            }
            let eff = (dy.abs() - DEADZONE_PX).max(0.0);
            let speed_px_per_sec = if eff > 0.0 {
                (eff * 6.0 + eff * eff * 0.085).min(18_000.0)
            } else {
                0.0
            };
            let scroll_delta_y = if eff > 0.0 {
                dy.signum() * speed_px_per_sec * dt
            } else {
                0.0
            };

            let (cursor, dir_sign) = if eff > 0.0 && dy < 0.0 {
                (CursorIcon::ResizeNorth, -1)
            } else if eff > 0.0 && dy > 0.0 {
                (CursorIcon::ResizeSouth, 1)
            } else {
                (CursorIcon::ResizeVertical, 0)
            };
            ctx.set_cursor_icon(cursor);
            paint_two_arrow_indicator(ctx, self.anchor, theme, dir_sign);

            if eff > 0.0 {
                ctx.request_repaint();
            }
            return Some((self.target, scroll_delta_y));
        }

        if middle_pressed {
            if let Some(pos) = pointer_pos {
                let target = if editor_rect.is_some_and(|r| r.contains(pos)) {
                    Some(AutoScrollTarget::Editor)
                } else if reader_rect.is_some_and(|r| r.contains(pos)) {
                    Some(AutoScrollTarget::Reader)
                } else {
                    None
                };
                if let Some(target) = target {
                    self.active = true;
                    self.target = target;
                    self.anchor = pos;
                    self.pressed_at = now;
                    self.dragged_beyond_deadzone = false;
                    self.suppress_pane_clicks_this_frame = true;
                    ctx.set_cursor_icon(CursorIcon::ResizeVertical);
                    paint_two_arrow_indicator(ctx, pos, theme, 0);
                    return Some((target, 0.0));
                }
            }
        }

        None
    }
}

fn paint_two_arrow_indicator(
    ctx: &egui::Context,
    anchor: Pos2,
    theme: &WordTheme,
    dir_sign: i8,
) {
    let painter = ctx.layer_painter(LayerId::new(
        Order::Foreground,
        Id::new("autoscroll_indicator"),
    ));

    let bg = Color32::from_rgba_unmultiplied(
        theme.paper_bg.r(),
        theme.paper_bg.g(),
        theme.paper_bg.b(),
        242,
    );
    let border_col = if dir_sign != 0 {
        theme.accent
    } else {
        theme.border
    };
    let dot_col = if dir_sign != 0 {
        theme.accent
    } else {
        theme.text_muted
    };
    let up_col = if dir_sign < 0 {
        theme.accent
    } else {
        theme.text_primary
    };
    let down_col = if dir_sign > 0 {
        theme.accent
    } else {
        theme.text_primary
    };

    painter.circle_filled(
        anchor + Vec2::new(0.0, 1.0),
        16.5,
        Color32::from_black_alpha(38),
    );
    painter.circle(anchor, 15.0, bg, Stroke::new(1.5_f32, border_col));
    painter.circle_filled(anchor, 2.2, dot_col);

    // Upward arrow triangle
    painter.add(Shape::convex_polygon(
        vec![
            anchor + Vec2::new(0.0, -10.5),
            anchor + Vec2::new(-4.5, -5.0),
            anchor + Vec2::new(4.5, -5.0),
        ],
        up_col,
        Stroke::NONE,
    ));

    // Downward arrow triangle
    painter.add(Shape::convex_polygon(
        vec![
            anchor + Vec2::new(0.0, 10.5),
            anchor + Vec2::new(-4.5, 5.0),
            anchor + Vec2::new(4.5, 5.0),
        ],
        down_col,
        Stroke::NONE,
    ));
}
