//! Pure renderers: called while Markdown nodes are parsed, never each frame.
use crate::core::theme::{Color, Theme};
use gpui_kit::{Image, ImageFormat};
use std::sync::Arc;

pub(super) struct Media {
    pub image: Arc<Image>,
    pub width: f32,
    pub height: f32,
    pub baseline: f32,
}

pub(super) fn hex(value: Color) -> String {
    let rgba = gpui_kit::Rgba::from(super::super::theme::color(value));
    format!(
        "#{:02x}{:02x}{:02x}",
        (rgba.r * 255.).round() as u8,
        (rgba.g * 255.).round() as u8,
        (rgba.b * 255.).round() as u8
    )
}

pub(super) fn diagram(source: &str, tokens: Theme) -> Result<Media, String> {
    if source.len() > 64 * 1024 {
        return Err("图表内容较多，请拆分后预览。".into());
    }
    let parsed =
        mermaid_rs_renderer::parse_mermaid_strict(source).map_err(|error| error.to_string())?;
    if parsed.graph.nodes.len() > 512 || parsed.graph.edges.len() > 2048 {
        return Err("图表内容较多，请拆分后预览。".into());
    }
    let mut theme = mermaid_rs_renderer::Theme::modern();
    theme.primary_color = hex(tokens.surface_raised);
    theme.primary_text_color = hex(tokens.text_primary);
    theme.primary_border_color = hex(tokens.ui_border);
    theme.secondary_color = hex(tokens.surface_sunken);
    theme.tertiary_color = hex(tokens.surface_raised);
    theme.background = hex(tokens.surface_canvas);
    theme.text_color = hex(tokens.text_primary);
    theme.line_color = hex(tokens.text_secondary);
    theme.edge_label_background = hex(tokens.surface_canvas);
    theme.cluster_background = hex(tokens.surface_sunken);
    theme.cluster_border = hex(tokens.ui_border_subtle);
    theme.sequence_actor_fill = hex(tokens.surface_raised);
    theme.sequence_actor_border = hex(tokens.ui_border);
    theme.sequence_actor_line = hex(tokens.text_muted);
    theme.sequence_note_fill = hex(tokens.surface_sunken);
    theme.sequence_note_border = hex(tokens.ui_border);
    let config = mermaid_rs_renderer::LayoutConfig::default();
    let layout = mermaid_rs_renderer::compute_layout(&parsed.graph, &theme, &config);
    let dimensions = mermaid_rs_renderer::measure_svg_dimensions(&layout, &config, None);
    let svg = mermaid_rs_renderer::render_svg(&layout, &theme, &config);
    Ok(Media {
        image: Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes())),
        width: dimensions.width,
        height: dimensions.height,
        baseline: dimensions.height,
    })
}

pub(super) fn formula(source: &str, display: bool, tokens: Theme) -> Result<Media, String> {
    if source.len() > 16 * 1024 {
        return Err("公式内容较多，请拆分后预览。".into());
    }
    let mut depth = 0usize;
    for byte in source.bytes() {
        match byte {
            b'{' => {
                depth += 1;
                if depth > 128 {
                    return Err("公式嵌套较深，请拆分后预览。".into());
                }
            }
            b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    let nodes = ratex_parser::parser::parse(source).map_err(|error| error.to_string())?;
    let size = if display { 20. } else { 16. };
    let rgba = gpui_kit::Rgba::from(super::super::theme::color(tokens.text_primary));
    let options = ratex_layout::LayoutOptions::default()
        .with_style(if display {
            ratex_types::math_style::MathStyle::Display
        } else {
            ratex_types::math_style::MathStyle::Text
        })
        .with_color(ratex_types::color::Color::rgb(rgba.r, rgba.g, rgba.b));
    let layout = ratex_layout::layout(&nodes, &options);
    ratex_layout::validate_layout(&layout)?;
    let list = ratex_layout::to_display_list(&layout);
    let padding = 2.;
    let svg = ratex_svg::render_to_svg_with_color_syntax(
        &list,
        &ratex_svg::SvgOptions {
            font_size: size,
            padding,
            embed_glyphs: true,
            ..Default::default()
        },
        ratex_svg::SvgColorSyntax::Rgb,
    );
    Ok(Media {
        image: Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes())),
        width: (list.width * size + padding * 2.) as f32,
        height: ((list.height + list.depth) * size + padding * 2.) as f32,
        baseline: (list.height * size + padding) as f32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn formulas_and_diagrams_produce_sized_native_images() {
        let theme = Theme::default();
        let fraction = formula(r"\frac{a^2+b^2}{\sqrt{c}}", true, theme).unwrap();
        assert!(fraction.width > 20. && fraction.height > 35.);
        assert!(fraction.baseline < fraction.height);
        let svg = std::str::from_utf8(fraction.image.bytes()).unwrap();
        assert!(svg.contains("<path"));
        assert!(!svg.contains("<text"));
        let chart = diagram("flowchart LR\n A[开始] --> B[完成]", theme).unwrap();
        assert!(chart.width > 100. && chart.height > 30.);
    }
    #[test]
    fn malformed_or_excessive_content_returns_a_render_error() {
        let theme = Theme::default();
        assert!(formula(r"\frac{", true, theme).is_err());
        assert!(diagram("not a diagram", theme).is_err());
        assert!(formula(&"{".repeat(129), true, theme).is_err());
        assert!(diagram(&"a".repeat(64 * 1024 + 1), theme).is_err());
    }
}
