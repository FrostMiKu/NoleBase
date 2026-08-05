//! Pinned, vendored KaTeX runtime for the standalone HTML export.
//!
//! KaTeX 0.18.1 (MIT, © Khan Academy; see `assets/katex/LICENSE`) ships in
//! the binary and is inlined into exports that contain math: the stylesheet
//! with every referenced font embedded as a `data:font/woff2` URI, the
//! engine, and a bootstrap that renders each `.math[data-math]` element.
//! Formulas render fully offline (no CDN) with `throwOnError:false`,
//! `strict:"warn"`, and `trust:false`, so invalid or hostile TeX never throws
//! and cannot forge links or markup, and the raw escaped source stays visible
//! whenever scripting is unavailable.

use std::sync::LazyLock;

use base64::Engine;

/// Minified KaTeX engine (UMD, defines `window.katex`). Verified to contain
/// no `</script` sequence, so it can be inlined verbatim between script tags.
pub(crate) const KATEX_JS: &str = include_str!("../../assets/katex/katex.min.js");
pub(crate) const KATEX_LICENSE: &str = include_str!("../../assets/katex/LICENSE");

const KATEX_CSS: &str = include_str!("../../assets/katex/katex.min.css");

/// `font-family` name → woff2 bytes, mirroring the `@font-face` blocks of
/// [`KATEX_CSS`]. Only woff2 is embedded — it is supported by every browser
/// since 2016 — and the woff/ttf fallback entries are dropped from the
/// inlined stylesheet so exports stay small.
const KATEX_FONTS: [(&str, &[u8]); 20] = [
    (
        "KaTeX_AMS-Regular",
        include_bytes!("../../assets/katex/fonts/KaTeX_AMS-Regular.woff2"),
    ),
    (
        "KaTeX_Caligraphic-Bold",
        include_bytes!("../../assets/katex/fonts/KaTeX_Caligraphic-Bold.woff2"),
    ),
    (
        "KaTeX_Caligraphic-Regular",
        include_bytes!("../../assets/katex/fonts/KaTeX_Caligraphic-Regular.woff2"),
    ),
    (
        "KaTeX_Fraktur-Bold",
        include_bytes!("../../assets/katex/fonts/KaTeX_Fraktur-Bold.woff2"),
    ),
    (
        "KaTeX_Fraktur-Regular",
        include_bytes!("../../assets/katex/fonts/KaTeX_Fraktur-Regular.woff2"),
    ),
    (
        "KaTeX_Main-Bold",
        include_bytes!("../../assets/katex/fonts/KaTeX_Main-Bold.woff2"),
    ),
    (
        "KaTeX_Main-BoldItalic",
        include_bytes!("../../assets/katex/fonts/KaTeX_Main-BoldItalic.woff2"),
    ),
    (
        "KaTeX_Main-Italic",
        include_bytes!("../../assets/katex/fonts/KaTeX_Main-Italic.woff2"),
    ),
    (
        "KaTeX_Main-Regular",
        include_bytes!("../../assets/katex/fonts/KaTeX_Main-Regular.woff2"),
    ),
    (
        "KaTeX_Math-BoldItalic",
        include_bytes!("../../assets/katex/fonts/KaTeX_Math-BoldItalic.woff2"),
    ),
    (
        "KaTeX_Math-Italic",
        include_bytes!("../../assets/katex/fonts/KaTeX_Math-Italic.woff2"),
    ),
    (
        "KaTeX_SansSerif-Bold",
        include_bytes!("../../assets/katex/fonts/KaTeX_SansSerif-Bold.woff2"),
    ),
    (
        "KaTeX_SansSerif-Italic",
        include_bytes!("../../assets/katex/fonts/KaTeX_SansSerif-Italic.woff2"),
    ),
    (
        "KaTeX_SansSerif-Regular",
        include_bytes!("../../assets/katex/fonts/KaTeX_SansSerif-Regular.woff2"),
    ),
    (
        "KaTeX_Script-Regular",
        include_bytes!("../../assets/katex/fonts/KaTeX_Script-Regular.woff2"),
    ),
    (
        "KaTeX_Size1-Regular",
        include_bytes!("../../assets/katex/fonts/KaTeX_Size1-Regular.woff2"),
    ),
    (
        "KaTeX_Size2-Regular",
        include_bytes!("../../assets/katex/fonts/KaTeX_Size2-Regular.woff2"),
    ),
    (
        "KaTeX_Size3-Regular",
        include_bytes!("../../assets/katex/fonts/KaTeX_Size3-Regular.woff2"),
    ),
    (
        "KaTeX_Size4-Regular",
        include_bytes!("../../assets/katex/fonts/KaTeX_Size4-Regular.woff2"),
    ),
    (
        "KaTeX_Typewriter-Regular",
        include_bytes!("../../assets/katex/fonts/KaTeX_Typewriter-Regular.woff2"),
    ),
];

/// Runs once the DOM is ready and renders every `.math[data-math]` element
/// with the pinned KaTeX build. `throwOnError:false` keeps invalid formulas
/// visible as red source text instead of failing the script; `strict:"warn"`
/// reports non-LaTeX extension use; `trust:false` disables
/// `\href`/`\url`/`\includegraphics`/`\html*` so formulas cannot
/// forge links or markup. If KaTeX itself is missing (script blocked or
/// failed to parse), the raw source text remains untouched in the container.
pub(crate) const KATEX_INIT_JS: &str = r#"/* KaTeX v0.18.1 (MIT, (c) Khan Academy) math rendering */
(function () {
  "use strict";
  function renderMath() {
    if (typeof katex === "undefined") { return; }
    var nodes = document.querySelectorAll(".math[data-math]");
    for (var i = 0; i < nodes.length; i += 1) {
      var node = nodes[i];
      var display = node.getAttribute("data-math") === "display";
      try {
        katex.render(node.textContent.trim(), node, {
          displayMode: display,
          throwOnError: false,
          strict: "warn",
          trust: false
        });
      } catch (error) {
        /* katex.render only throws for non-parse failures; the raw TeX
           source stays visible in the container either way. */
      }
    }
  }
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", renderMath);
  } else {
    renderMath();
  }
}());
"#;

/// [`KATEX_CSS`] with every `url(fonts/…)` reference replaced by an embedded
/// `data:font/woff2;base64,…` URI and the woff/ttf fallbacks dropped, so the
/// stylesheet is fully self-contained. Computed once per process.
static EMBEDDED_CSS: LazyLock<String> = LazyLock::new(|| {
    let mut css = KATEX_CSS.to_string();
    for (name, bytes) in KATEX_FONTS {
        let source = format!(
            "url(fonts/{name}.woff2) format(\"woff2\"),url(fonts/{name}.woff) format(\"woff\"),url(fonts/{name}.ttf) format(\"truetype\")"
        );
        assert!(
            css.contains(&source),
            "vendored KaTeX stylesheet has no font source for {name}"
        );
        let embedded = format!(
            "url(data:font/woff2;base64,{}) format(\"woff2\")",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        );
        css = css.replace(&source, &embedded);
    }
    css
});

pub(crate) fn embedded_css() -> &'static str {
    EMBEDDED_CSS.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_css_inlines_every_font_and_drops_fallbacks() {
        let css = embedded_css();
        assert_eq!(css.matches("data:font/woff2;base64,").count(), 20);
        assert!(!css.contains("url(fonts/"));
        assert!(!css.contains(".woff)"));
        assert!(!css.contains(".ttf)"));
        // Every family referenced by the vendored stylesheet stays declared.
        let mut families: Vec<&str> = KATEX_FONTS
            .iter()
            .map(|(name, _)| name.split('-').next().expect("family prefix"))
            .collect();
        families.sort_unstable();
        families.dedup();
        assert_eq!(families.len(), 12);
        for family in families {
            assert!(
                css.contains(&format!("font-family:{family};")),
                "missing font-family:{family}"
            );
        }
        // The layout rules the rendered math relies on survive the transform.
        assert!(css.contains(".katex-display"));
    }

    #[test]
    fn pinned_version_matches_the_vendored_engine() {
        assert!(KATEX_JS.contains("version:\"0.18.1\""));
        assert!(!KATEX_JS.contains("</script"));
        assert!(!embedded_css().contains("</style"));
    }

    #[test]
    fn bootstrap_uses_locked_down_render_options() {
        assert!(KATEX_INIT_JS.contains("katex.render("));
        assert!(KATEX_INIT_JS.contains("throwOnError: false"));
        assert!(KATEX_INIT_JS.contains("strict: \"warn\""));
        assert!(KATEX_INIT_JS.contains("trust: false"));
        assert!(KATEX_INIT_JS.contains("displayMode: display"));
        assert!(KATEX_INIT_JS.contains(".math[data-math]"));
        // The bootstrap must never terminate its own script tag.
        assert!(!KATEX_INIT_JS.contains("</script"));
    }
}
