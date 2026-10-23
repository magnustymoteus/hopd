//! Draws pane mode's labels into a shared-memory buffer: tiny-skia for the label
//! backgrounds, cosmic-text for finding fonts and turning text into glyph pixels.

use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Weight};
use tiny_skia::{Paint, PixmapMut, Rect, Transform};

use crate::config::{PaneConfig, Rgba};
use crate::hints::Hints;

/// Space between a label's text and the edge of its background, in pixels.
const PADDING_X: f32 = 4.0;
const PADDING_Y: f32 = 1.0;

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

    /// Draws the visible hints onto a transparent `width` × `height` canvas in Wayland's
    /// ARGB8888 format. `origin` is where the window's top-left corner is on the canvas.
    pub fn draw(
        &mut self,
        canvas: &mut [u8],
        width: u32,
        height: u32,
        origin: (f64, f64),
        hints: &Hints,
        style: &PaneConfig,
    ) {
        canvas.fill(0);
        let Some(mut pixmap) = PixmapMut::from_bytes(canvas, width, height) else {
            return;
        };

        let family = match &style.font.family {
            Some(name) => Family::Name(name),
            None => Family::Monospace,
        };
        let attrs = Attrs::new()
            .family(family)
            .weight(Weight(style.font.weight));
        let metrics = Metrics::new(style.font.size, style.font.size * 1.2);
        let typed_attrs = attrs.clone().color(color(style.typed_color));
        let rest_attrs = attrs.clone().color(color(style.text_color));
        let mut background = Paint::default();
        let Rgba { r, g, b, a } = style.background;
        background.set_color_rgba8(r, g, b, a);

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

            let center_x = (origin.0 + hint.x) as f32;
            let center_y = (origin.1 + hint.y) as f32;
            let left = (center_x - text_width / 2.0).round();
            let top = (center_y - text_height / 2.0).round();
            if let Some(rect) = Rect::from_xywh(
                left - PADDING_X,
                top - PADDING_Y,
                text_width + 2.0 * PADDING_X,
                text_height + 2.0 * PADDING_Y,
            ) {
                pixmap.fill_rect(rect, &background, Transform::identity(), None);
            }

            let pixels = pixmap.data_mut();
            text.draw(
                &mut self.fonts,
                &mut self.glyphs,
                color(style.text_color),
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
