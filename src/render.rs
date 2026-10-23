//! Draws pane mode's labels into a shared-memory buffer: tiny-skia for shapes (label
//! boxes, borders, the backdrop), cosmic-text for finding fonts and turning text into glyph
//! pixels.

use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Weight};
use tiny_skia::{FillRule, Paint, Path, PathBuilder, PixmapMut, Rect, Stroke, Transform};

use crate::config::{PaneConfig, Rgba};
use crate::hints::Hints;

/// The target window's place on the canvas.
#[derive(Debug, Clone, Copy)]
pub struct Area {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Corner radius, matching Hyprland's window rounding.
    pub radius: f32,
}

pub struct Renderer {
    fonts: FontSystem,
    glyphs: SwashCache,
}

impl Renderer {
    /// Loads the list of installed fonts, which takes a moment, so it happens once.
    pub fn new() -> Renderer {
        Renderer {
            fonts: FontSystem::new(),
            glyphs: SwashCache::new(),
        }
    }

    pub fn has_family(&self, family: &str) -> bool {
        self.fonts.db().faces().any(|face| {
            face.families
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(family))
        })
    }

    /// Draws the backdrop and the visible hints onto a transparent `width` × `height`
    /// canvas in Wayland's ARGB8888 format. Hint positions are relative to `window`.
    pub fn draw(
        &mut self,
        canvas: &mut [u8],
        width: u32,
        height: u32,
        window: Area,
        hints: &Hints,
        style: &PaneConfig,
    ) {
        canvas.fill(0);
        let Some(mut pixmap) = PixmapMut::from_bytes(canvas, width, height) else {
            return;
        };

        if style.backdrop.a > 0
            && let Some(path) = rounded_rect(
                window.x,
                window.y,
                window.width,
                window.height,
                window.radius,
            )
        {
            pixmap.fill_path(
                &path,
                &paint(style.backdrop),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }

        let label = &style.label;
        let (padding_y, padding_x) = label.padding;
        let border = label.border_width;

        let family = match &style.font.family {
            Some(name) => Family::Name(name),
            None => Family::Monospace,
        };
        let attrs = Attrs::new()
            .family(family)
            .weight(Weight(style.font.weight));
        let metrics = Metrics::new(style.font.size, style.font.size * 1.2);
        let typed_attrs = attrs.clone().color(color(label.typed_color));
        let rest_attrs = attrs.clone().color(color(label.text_color));
        let background = paint(label.background);
        let border_paint = paint(label.border_color);
        let stroke = Stroke {
            width: border,
            ..Stroke::default()
        };

        let typed: String = hints.typed().iter().collect();
        for hint in hints.visible() {
            let rest: String = hint.label[typed.chars().count()..].iter().collect();
            let mut text = Buffer::new(&mut self.fonts, metrics);
            text.set_rich_text(
                [
                    (typed.as_str(), typed_attrs.clone()),
                    (rest.as_str(), rest_attrs.clone()),
                ],
                &attrs,
                Shaping::Advanced,
                None,
            );
            text.shape_until_scroll(&mut self.fonts, false);
            let text_width = text.layout_runs().map(|run| run.line_w).fold(0.0, f32::max);
            let text_height = metrics.line_height;

            // The box: text, padding around it, then the border around that.
            let box_width = text_width + 2.0 * (padding_x + border);
            let box_height = text_height + 2.0 * (padding_y + border);
            let box_left = (window.x + hint.x as f32 - box_width / 2.0).round();
            let box_top = (window.y + hint.y as f32 - box_height / 2.0).round();
            if let Some(path) = rounded_rect(box_left, box_top, box_width, box_height, label.radius)
            {
                pixmap.fill_path(
                    &path,
                    &background,
                    FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }
            // A stroke is centred on its path, so the path runs half a border width inside
            // the box's edge to keep the whole border within the box.
            if border > 0.0
                && let Some(path) = rounded_rect(
                    box_left + border / 2.0,
                    box_top + border / 2.0,
                    box_width - border,
                    box_height - border,
                    label.radius - border / 2.0,
                )
            {
                pixmap.stroke_path(&path, &border_paint, &stroke, Transform::identity(), None);
            }
            let left = box_left + border + padding_x;
            let top = box_top + border + padding_y;

            let pixels = pixmap.data_mut();
            text.draw(
                &mut self.fonts,
                &mut self.glyphs,
                color(label.text_color),
                |x, y, w, h, color| {
                    blend(
                        pixels,
                        (width, height),
                        (left as i32 + x, top as i32 + y),
                        (w, h),
                        color,
                    );
                },
            );
        }

        // tiny-skia stores pixels as R, G, B, A bytes; Wayland's ARGB8888 is a
        // little-endian 32-bit value, so in memory it's B, G, R, A.
        for pixel in pixmap.data_mut().as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
    }
}

fn color(Rgba { r, g, b, a }: Rgba) -> Color {
    Color::rgba(r, g, b, a)
}

fn paint(Rgba { r, g, b, a }: Rgba) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(r, g, b, a);
    paint
}

/// A rectangle with circular corners of `radius`, clamped so that a large radius gives
/// a pill shape instead of overlapping corners.
fn rounded_rect(x: f32, y: f32, width: f32, height: f32, radius: f32) -> Option<Path> {
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let r = radius.clamp(0.0, width.min(height) / 2.0);
    if r == 0.0 {
        return Some(PathBuilder::from_rect(Rect::from_xywh(
            x, y, width, height,
        )?));
    }
    // Distance of the control points from each corner's ends: the standard cubic
    // approximation of a quarter circle.
    let k = 0.552_284_8 * r;
    let (right, bottom) = (x + width, y + height);
    let mut path = PathBuilder::new();
    path.move_to(x + r, y);
    path.line_to(right - r, y);
    path.cubic_to(right - r + k, y, right, y + r - k, right, y + r);
    path.line_to(right, bottom - r);
    path.cubic_to(
        right,
        bottom - r + k,
        right - r + k,
        bottom,
        right - r,
        bottom,
    );
    path.line_to(x + r, bottom);
    path.cubic_to(x + r - k, bottom, x, bottom - r + k, x, bottom - r);
    path.line_to(x, y + r);
    path.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    path.close();
    path.finish()
}

/// Paints a `size` rectangle at `at` in `color` over premultiplied RGBA `pixels`, clipped
/// to the canvas. cosmic-text calls this for each run of glyph pixels, with the glyph's
/// coverage already folded into the colour's alpha.
fn blend(pixels: &mut [u8], canvas: (u32, u32), at: (i32, i32), size: (u32, u32), color: Color) {
    let alpha = u32::from(color.a());
    if alpha == 0 {
        return;
    }
    let source = [color.r(), color.g(), color.b()].map(|c| u32::from(c) * alpha / 255);
    let x_range = at.0.max(0)..(at.0 + size.0 as i32).min(canvas.0 as i32);
    let y_range = at.1.max(0)..(at.1 + size.1 as i32).min(canvas.1 as i32);
    for y in y_range {
        for x in x_range.clone() {
            let i = (y as usize * canvas.0 as usize + x as usize) * 4;
            let pixel = &mut pixels[i..i + 4];
            // Source-over: the new colour, plus whatever shows through it.
            let keep = 255 - alpha;
            for (channel, source) in pixel[..3].iter_mut().zip(source) {
                *channel = (source + u32::from(*channel) * keep / 255) as u8;
            }
            pixel[3] = (alpha + u32::from(pixel[3]) * keep / 255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounded_rects_keep_their_bounds() {
        for radius in [0.0, 4.0, 1000.0] {
            let path = rounded_rect(10.0, 20.0, 30.0, 12.0, radius).unwrap();
            assert_eq!(
                path.bounds(),
                Rect::from_xywh(10.0, 20.0, 30.0, 12.0).unwrap(),
                "radius {radius}"
            );
        }
    }

    #[test]
    fn empty_rects_draw_nothing() {
        assert!(rounded_rect(0.0, 0.0, 0.0, 10.0, 2.0).is_none());
    }

    #[test]
    fn opaque_colour_replaces_the_pixel() {
        let mut pixels = [0u8; 4];
        blend(
            &mut pixels,
            (1, 1),
            (0, 0),
            (1, 1),
            Color::rgba(10, 20, 30, 255),
        );
        assert_eq!(pixels, [10, 20, 30, 255]);
    }

    #[test]
    fn half_transparent_colour_mixes_with_the_pixel() {
        let mut pixels = [200, 200, 200, 255];
        blend(
            &mut pixels,
            (1, 1),
            (0, 0),
            (1, 1),
            Color::rgba(0, 0, 0, 128),
        );
        assert_eq!(pixels, [99, 99, 99, 255]);
    }

    #[test]
    fn blending_is_clipped_to_the_canvas() {
        let mut pixels = [0u8; 2 * 2 * 4];
        blend(
            &mut pixels,
            (2, 2),
            (1, -1),
            (5, 2),
            Color::rgba(255, 255, 255, 255),
        );
        // Only the top-right pixel is inside both the canvas and the rectangle.
        assert_eq!(&pixels[4..8], [255, 255, 255, 255]);
        assert_eq!(pixels.iter().filter(|&&b| b != 0).count(), 4);
    }
}
