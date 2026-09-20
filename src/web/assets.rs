//! Serving the compiled React UI out of the binary. The build script turns
//! on `cfg(embed_web_ui)` when `web/dist` holds a production build; release
//! CI always builds the frontend first, so shipped binaries answer `/` with
//! the real UI, while a fresh checkout without a frontend build never
//! compiles this module at all.

use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

/// The production build produced by `pnpm --dir web run build`. Embedding
/// builds bake the bytes in at compile time; debug builds read the same
/// folder from disk at runtime (rust-embed's default), so frontend changes
/// show up without recompiling Rust. The folder resolves against the crate
/// root, not the server's working directory.
#[derive(RustEmbed)]
#[folder = "web/dist/"]
pub(crate) struct Assets;

/// The fallback for non-API GETs when the frontend is embedded: the asset
/// at the request path, the SPA shell for client-side routes, or a plain
/// 404 if the embedded build were somehow empty.
pub async fn serve_embedded(uri: Uri) -> Response {
    let requested = uri.path().trim_start_matches('/');
    let requested = if requested.is_empty() {
        "index.html"
    } else {
        requested
    };
    serve_asset(requested)
        .or_else(|| serve_asset("index.html"))
        .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
}

/// One embedded file with its guessed content type. Hashed `assets/` files
/// are content-addressed and cache forever; the SPA shell and everything
/// else revalidates so a replaced build is picked up.
fn serve_asset(path: &str) -> Option<Response> {
    let file = Assets::get(path)?;
    let cache_control = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    Some(
        (
            [
                (
                    header::CONTENT_TYPE,
                    mime_guess::from_path(path).first_or_octet_stream().as_ref(),
                ),
                (header::CACHE_CONTROL, cache_control),
            ],
            file.data.into_owned(),
        )
            .into_response(),
    )
}
