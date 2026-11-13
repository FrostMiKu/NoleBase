/// The diagram engine accounts for terminal cell widths, including CJK.
/// Keep its spacing intact so mixed-language labels retain their spaces.
pub(super) fn render(source: &str, width: usize) -> Result<String, mermaid_text::Error> {
    mermaid_text::render_with_options(
        source,
        &mermaid_text::RenderOptions {
            max_width: Some(width),
            max_width_strict: true,
            gaps_override: Some((2, 1)),
            ..Default::default()
        },
    )
}
