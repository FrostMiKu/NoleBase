//! Build script: turn on `cfg(embed_web_ui)` when a production build of the
//! React UI exists at `web/dist`, so release binaries (and any build made
//! after `pnpm --dir web run build`) carry the frontend while a fresh
//! checkout without one still compiles in API-only mode.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(embed_web_ui)");
    if std::path::Path::new("web/dist/index.html").is_file() {
        println!("cargo::rustc-cfg=embed_web_ui");
        // Re-evaluate whenever the built frontend changes; dropping the
        // directory flips the flag back off on the next build.
        println!("cargo::rerun-if-changed=web/dist");
    }
}
