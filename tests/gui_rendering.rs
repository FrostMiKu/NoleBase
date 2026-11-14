//! Run on the main thread so AppKit and Metal can render production views.
fn main() -> anyhow::Result<()> {
    let workspace = tempfile::tempdir()?;
    let output = std::env::var_os("NOLE_REVIEW_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("nole-kit-review"));
    nole::gui::review::capture(
        nole::core::storage::Storage::new(workspace.path())?,
        &output,
    )?;
    println!("Rendered desktop review: {}", output.display());
    Ok(())
}
