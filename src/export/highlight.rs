//! Pinned, vendored highlight.js runtime for the standalone HTML export.
//!
//! highlight.js 11.11.1 (BSD-3-Clause; see `assets/highlight/LICENSE`) ships
//! in the binary and is inlined into exports that contain plain fenced code
//! blocks: the theme stylesheet in the head, the engine, and a bootstrap that
//! highlights every `pre code[class^="language-"]` element whose language the
//! pinned build knows. Highlighting is fully offline (no CDN); blocks with an
//! unknown or missing language keep their plain escaped source, and any
//! bootstrap failure leaves the source untouched.

/// Minified highlight.js "common" browser build (v11.11.1, git 08cb242e7d) —
/// defines `window.hljs` and registers ~40 common languages (rust, python,
/// javascript, typescript, c/c++/c#, go, …) including the aliases the
/// exporter's language allowlist admits (`c++`, `c#`, `objective-c`,
/// `python-repl`). Verified to contain no `</script` sequence, so it can be
/// inlined verbatim between script tags.
pub(crate) const HIGHLIGHT_JS: &str = include_str!("../../assets/highlight/highlight.min.js");
pub(crate) const HIGHLIGHT_LICENSE: &str = include_str!("../../assets/highlight/LICENSE");

/// GitHub light theme. The base `.hljs` rule is inert here because the
/// bootstrap never adds the `hljs` class; only the standalone token selectors
/// (`.hljs-keyword`, …) apply, so highlighted and plain blocks keep the
/// export's own `pre`/`pre code` layout.
pub(crate) const HIGHLIGHT_THEME_CSS: &str = include_str!("../../assets/highlight/github.min.css");

/// Runs after the engine, highlighting every fenced code block that carries a
/// `language-*` class the exporter emitted. Only languages the pinned build
/// knows are touched: unknown or missing languages keep their plain escaped
/// source, and a missing engine aborts without touching anything. The raw
/// source is read back from the DOM (the server-side escaping is already
/// decoded there) and the engine re-escapes it before emitting token spans,
/// so hostile code can never execute; any failure leaves the source
/// untouched.
pub(crate) const HIGHLIGHT_INIT_JS: &str = r#"(function(){try{if(typeof hljs==='undefined')return;var nodes=document.querySelectorAll('pre code[class^="language-"]');for(var i=0;i<nodes.length;i++){var el=nodes[i];var match=/^language-([A-Za-z0-9_+#-]+)$/.exec(el.className);if(!match)continue;var name=match[1];if(!hljs.getLanguage(name))continue;el.innerHTML=hljs.highlight(name,el.textContent,true).value;}}catch(error){}})();"#;

/// The language token of a fenced code info string: its first
/// whitespace-delimited word, accepted only when it consists of ASCII
/// alphanumerics plus `_`, `+`, `#`, `-` (the shapes the pinned build
/// registers and aliases — `rust`, `c++`, `c#`, `objective-c`,
/// `python-repl`). Anything else (quotes, angle brackets, colons, dots,
/// whitespace) yields `None` so no `language-*` class is ever emitted for an
/// untrusted token. Kept in sync with the `/^language-…$/` class check in
/// [`HIGHLIGHT_INIT_JS`].
pub(crate) fn language_token(info: &str) -> Option<&str> {
    let token = info.split_whitespace().next()?;
    token
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '+' | '#' | '-'))
        .then_some(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_token_admits_safe_alias_shapes() {
        for info in [
            "rust",
            "rust extra-token",
            "c++",
            "c#",
            "objective-c",
            "python-repl",
            "Rust",
            "jsx",
            "c99",
        ] {
            assert_eq!(
                language_token(info),
                Some(info.split_whitespace().next().unwrap()),
                "info: {info}"
            );
        }
    }

    #[test]
    fn language_token_rejects_unsafe_or_absent_tokens() {
        for info in [
            "",
            "   ",
            "rust\" onmouseover=\"alert(1)",
            "javascript:alert(1)",
            "<script>alert(1)</script>",
            "a.b",
            "x=y",
            "x,y",
            "x|y",
            "a/b",
            "日本語",
            "`backtick`",
        ] {
            assert_eq!(language_token(info), None, "info: {info}");
        }
    }
}
