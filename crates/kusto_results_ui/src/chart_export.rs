//! Copying a chart as a picture (CXP-1): the scene is written as SVG, drawn by resvg and put on
//! the clipboard as a PNG.

use std::sync::{Arc, LazyLock};

use anyhow::{Context as _, Result, bail};
use gpui::{App, Hsla, Rgba};
use image::ImageEncoder as _;
use kusto_results::chart_scene::SvgStyle;
use resvg::{tiny_skia, usvg};
use ui::prelude::*;

use crate::actor_colours::theme_actor_palette;

/// The picture has this many pixels for each pixel the chart takes on the screen, so that it
/// stays sharp when it is pasted into a document.
pub(crate) const SCALE: f32 = 2.0;

/// The most pixels along a side, so that a huge chart cannot ask for gigabytes.
const MAXIMUM_SIDE: u32 = 8192;

static FONTS: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
    let mut database = usvg::fontdb::Database::new();
    database.load_system_fonts();
    Arc::new(database)
});

/// Draws an SVG and encodes it as a PNG.
pub(crate) fn render_png(svg: &str, scale: f32) -> Result<Vec<u8>> {
    let options = usvg::Options {
        fontdb: FONTS.clone(),
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(svg, &options).context("the chart picture is not valid")?;
    let size = tree.size();
    let width = (size.width() * scale).ceil() as u32;
    let height = (size.height() * scale).ceil() as u32;
    if width == 0 || height == 0 || width > MAXIMUM_SIDE || height > MAXIMUM_SIDE {
        bail!("a chart of {width} by {height} pixels is too large to copy");
    }
    let mut pixmap = tiny_skia::Pixmap::new(width, height).context("no room for the picture")?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let mut pixels = Vec::with_capacity(pixmap.pixels().len() * 4);
    for pixel in pixmap.pixels() {
        let straight = pixel.demultiply();
        pixels.extend([
            straight.red(),
            straight.green(),
            straight.blue(),
            straight.alpha(),
        ]);
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&pixels, width, height, image::ExtendedColorType::Rgba8)
        .context("could not encode the picture")?;
    Ok(png)
}

fn hex(color: Hsla) -> String {
    let color = Rgba::from(color);
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        channel(color.r),
        channel(color.g),
        channel(color.b)
    )
}

/// The colours of the current theme, with the series coloured as they are on the screen.
pub(crate) fn themed_style(series_count: usize, cx: &App) -> SvgStyle {
    let colors = cx.theme().colors();
    let mut style = SvgStyle::light();
    style.background = Some(hex(colors.editor_background));
    style.text = hex(colors.text);
    style.muted = hex(colors.text_muted);
    style.grid = hex(colors.border_variant);
    style.axis = hex(colors.border);
    style.series = theme_actor_palette(series_count.max(1), cx)
        .into_iter()
        .map(hex)
        .collect();
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQUARE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="6" viewBox="0 0 10 6">
        <rect width="10" height="6" fill="#ffffff"/><rect x="0" y="0" width="5" height="6" fill="#ff0000"/></svg>"##;

    #[test]
    fn a_picture_is_drawn_at_the_scale_asked_for() {
        let png = render_png(SQUARE, 2.0).unwrap();
        let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (20, 12));
        assert_eq!(decoded.get_pixel(2, 2).0, [255, 0, 0, 255], "the red half");
        assert_eq!(
            decoded.get_pixel(17, 9).0,
            [255, 255, 255, 255],
            "the white half"
        );
    }

    #[test]
    fn something_that_is_not_a_picture_says_so() {
        let error = render_png("not svg", 1.0).unwrap_err();
        assert!(format!("{error:#}").contains("not valid"), "{error:#}");
    }

    #[test]
    fn a_picture_too_large_to_hold_is_refused() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="20000" height="10"/>"#;
        let error = render_png(svg, 1.0).unwrap_err();
        assert!(format!("{error:#}").contains("too large"), "{error:#}");
    }

    #[test]
    fn colours_are_written_as_hex() {
        assert_eq!(hex(gpui::hsla(0.0, 1.0, 0.5, 1.0)), "#ff0000");
        assert_eq!(hex(gpui::hsla(0.0, 0.0, 1.0, 1.0)), "#ffffff");
    }
}
