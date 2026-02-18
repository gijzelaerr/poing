use nih_plug_vizia::vizia::prelude::*;
use nih_plug_vizia::vizia::vg::{Paint, Path, Color as VgColor};
use std::sync::Arc;

use crate::model::{PoingEvent, PoingModel};

pub struct WaveformView {
    is_dragging: bool,
    drag_start_x: f32,
    drag_start_y: f32,
}

impl WaveformView {
    pub fn new(cx: &mut Context) -> Handle<Self> {
        Self {
            is_dragging: false,
            drag_start_x: 0.0,
            drag_start_y: 0.0,
        }
        .build(cx, |_| {})
    }
}

impl View for WaveformView {
    fn element(&self) -> Option<&'static str> {
        Some("waveform")
    }

    fn event(&mut self, cx: &mut EventContext, event: &mut Event) {
        event.map(|window_event, _| match window_event {
            WindowEvent::MouseDown(button) if *button == MouseButton::Left => {
                self.is_dragging = true;
                self.drag_start_x = cx.mouse().left.pos_down.0;
                self.drag_start_y = cx.mouse().left.pos_down.1;
                cx.capture();
            }
            WindowEvent::MouseUp(button) if *button == MouseButton::Left => {
                self.is_dragging = false;
                cx.release();
            }
            WindowEvent::MouseMove(x, y) => {
                if self.is_dragging {
                    let dx = *x - self.drag_start_x;
                    let dy = *y - self.drag_start_y;
                    let distance = (dx * dx + dy * dy).sqrt();
                    if distance >= 10.0 {
                        self.is_dragging = false;
                        cx.release();
                        cx.emit(PoingEvent::StartDrag);
                    }
                }
            }
            _ => {}
        });
    }

    fn draw(&self, cx: &mut DrawContext, canvas: &mut Canvas) {
        let bounds = cx.bounds();
        if bounds.w == 0.0 || bounds.h == 0.0 {
            return;
        }

        let bg_color = VgColor::rgb(15, 15, 23); // #0f0f17
        let center_color = VgColor::rgb(45, 45, 64); // #2d2d40
        let wave_color = VgColor::rgb(61, 122, 209); // #3d7ad1

        // Draw background
        let mut path = Path::new();
        path.rect(bounds.x, bounds.y, bounds.w, bounds.h);
        canvas.fill_path(&path, &Paint::color(bg_color));

        // Draw center line
        let center_y = bounds.y + bounds.h * 0.5;
        let mut path = Path::new();
        path.move_to(bounds.x, center_y);
        path.line_to(bounds.x + bounds.w, center_y);
        let mut paint = Paint::color(center_color);
        paint.set_line_width(1.0);
        canvas.stroke_path(&path, &paint);

        // Get waveform data from model
        let waveform_data: Arc<Vec<(f32, f32)>> =
            PoingModel::waveform_data.get(cx);

        if waveform_data.is_empty() {
            return;
        }

        let num_cols = waveform_data.len();
        let col_width = bounds.w / num_cols as f32;

        // Draw waveform bars
        for (i, (min_val, max_val)) in waveform_data.iter().enumerate() {
            if *min_val == 0.0 && *max_val == 0.0 {
                continue;
            }

            let x = bounds.x + i as f32 * col_width;
            // Map sample values (-1..1) to pixel coordinates
            let y_top = center_y - max_val * bounds.h * 0.45;
            let y_bottom = center_y - min_val * bounds.h * 0.45;
            let bar_height = (y_bottom - y_top).max(1.0);

            let mut path = Path::new();
            path.rect(x, y_top, col_width.max(1.0), bar_height);
            canvas.fill_path(&path, &Paint::color(wave_color));
        }
    }
}
