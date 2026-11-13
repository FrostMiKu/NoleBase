use super::*;

impl Storage {
    pub fn prepare_export(
        &self,
        source: impl AsRef<Path>,
        destination: impl AsRef<Path>,
        format: ExportFormat,
        policy: ExportDestinationPolicy,
    ) -> Result<PreparedExport> {
        let canonical_root = fs::canonicalize(&self.root).context("canonicalizing Nole root")?;
        let source_input = source.as_ref();
        let source_candidate = if source_input.is_absolute() {
            normalize_lexical(source_input.to_path_buf())?
        } else {
            normalize_lexical(self.root.join(source_input))?
        };
        reject_symlink_components_below(&self.root, &source_candidate)?;
        let canonical_source = fs::canonicalize(&source_candidate)
            .with_context(|| format!("resolving export source {}", source_candidate.display()))?;
        if !canonical_source.starts_with(&canonical_root) {
            bail!("export source must be inside the Nole root");
        }
        let relative = canonical_source.strip_prefix(&canonical_root)?;
        if relative.starts_with(CONFIG_DIR) || relative.starts_with(ATTACHMENTS_DIR) {
            bail!("config and attachment internals cannot be exported");
        }
        let metadata = fs::metadata(&canonical_source)?;
        if !metadata.is_file() {
            bail!("export source must be a regular file");
        }
        let (rendered_source, content_sha256) = if format != ExportFormat::Original {
            let extension = canonical_source
                .extension()
                .and_then(|value| value.to_str());
            if !extension.is_some_and(|value| value.eq_ignore_ascii_case("md")) {
                bail!("{} export requires a UTF-8 .md source", format.label());
            }
            let bytes = fs::read(&canonical_source)
                .with_context(|| format!("reading {}", canonical_source.display()))?;
            let size = u64::try_from(bytes.len()).context("export source is too large")?;
            if size > crate::core::export::MAX_RENDER_SOURCE_BYTES {
                bail!(
                    "{} export source exceeds the {}-byte limit",
                    format.label(),
                    crate::core::export::MAX_RENDER_SOURCE_BYTES
                );
            }
            let content_sha256 = sha256_bytes(&bytes);
            (
                Some(String::from_utf8(bytes).context("rendered export source is not UTF-8")?),
                content_sha256,
            )
        } else {
            (None, sha256_file(&canonical_source)?)
        };
        let destination = self.resolve_export_destination(destination.as_ref(), &canonical_root)?;
        format.validate_destination(&destination)?;
        // Open the validated parent directory and bind every publication
        // operation to it, so a later rename or symlink swap of the parent
        // path cannot redirect the publish. Destination policy checks run
        // against this handle too, keeping the prepare-time validation
        // race-free.
        let parent = destination
            .parent()
            .context("export destination has no parent directory")?;
        let file_name = destination
            .file_name()
            .context("export destination needs a file name")?
            .to_os_string();
        let parent_dir =
            cap_std::fs::Dir::open_ambient_dir(parent, cap_std::ambient_authority())
                .with_context(|| format!("opening export directory {}", parent.display()))?;
        match policy {
            ExportDestinationPolicy::CreateNew => {
                if destination_entry_exists(&parent_dir, &file_name)? {
                    bail!(
                        "export destination already exists: {}",
                        destination.display()
                    );
                }
            }
            ExportDestinationPolicy::ReplaceExisting => {
                validate_replaceable_destination(&parent_dir, &file_name, &destination)?;
            }
        }
        Ok(PreparedExport {
            source: canonical_source.clone(),
            destination,
            file_name,
            parent_dir,
            format,
            policy,
            identity: ExportSourceIdentity {
                canonical_path: canonical_source,
                length: metadata.len(),
                modified: metadata
                    .modified()
                    .context("reading export source modification time")?,
                content_sha256,
            },
            rendered_source,
        })
    }

    pub fn publish_export(&self, prepared: &PreparedExport) -> Result<ExportOutcome> {
        self.revalidate_export(prepared)?;
        let (rendered_bytes, rendered_diagnostics) = match prepared.format {
            ExportFormat::Original => (None, Vec::new()),
            ExportFormat::Html => {
                let source = prepared
                    .rendered_source
                    .as_deref()
                    .context("rendered export is missing its prepared source")?;
                let title = prepared
                    .source
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("Nole export");
                let attachments =
                    crate::core::attachment::AttachmentStore::new(self.attachments_dir.clone());
                let rendered = crate::core::export::render_html(
                    source,
                    title,
                    &self.root,
                    &prepared.source,
                    &attachments,
                )?;
                (Some(rendered.bytes), rendered.diagnostics)
            }
        };
        let mut original_source = if prepared.format == ExportFormat::Original {
            let source = File::open(&prepared.source)
                .with_context(|| format!("opening {}", prepared.source.display()))?;
            validate_open_export_source(&source, prepared)?;
            Some(BufReader::with_capacity(64 * 1024, source))
        } else {
            None
        };
        // Write into a same-directory temp file and publish atomically, so a
        // failed write, flush, render, or race never leaves a partial final
        // target behind. The temp file is created relative to the parent
        // directory handle bound at prepare time and removed on every failure
        // path.
        let (mut temp_file, temp_name) = create_export_temp(
            &prepared.parent_dir,
            &prepared.file_name,
            &prepared.destination,
        )?;
        let publication = (|| -> Result<u64> {
            let bytes = if let Some(bytes) = &rendered_bytes {
                temp_file.write_all(bytes)?;
                u64::try_from(bytes.len()).context("export is too large")?
            } else {
                let source = original_source
                    .as_mut()
                    .context("missing Original source handle")?;
                let (bytes, copied_sha256) = copy_and_hash(source, &mut temp_file)?;
                if copied_sha256 != prepared.identity.content_sha256 {
                    bail!("export source content changed after preparation");
                }
                bytes
            };
            temp_file.flush()?;
            // Re-check the source and destination after the write, so a
            // change racing the export is caught before anything is linked
            // into place.
            if let Some(source) = original_source.as_ref() {
                validate_open_export_source(source.get_ref(), prepared)?;
            }
            self.revalidate_export_source(prepared)?;
            verify_export_source_content(prepared)?;
            // The destination is checked again right before publication, all
            // relative to the directory handle bound at prepare time, so a
            // change racing the export is caught before anything is linked
            // into place and a swapped parent path cannot redirect the
            // publish. CreateNew refuses any existing destination;
            // ReplaceExisting re-validates that the target is still an
            // existing regular non-symlink file before the atomic swap.
            match prepared.policy {
                ExportDestinationPolicy::CreateNew => {
                    if destination_entry_exists(&prepared.parent_dir, &prepared.file_name)? {
                        bail!(
                            "export destination already exists: {}",
                            prepared.destination.display()
                        );
                    }
                    publish_no_overwrite(
                        &prepared.parent_dir,
                        &temp_name,
                        &prepared.file_name,
                        &prepared.destination,
                    )?;
                }
                ExportDestinationPolicy::ReplaceExisting => {
                    validate_replaceable_destination(
                        &prepared.parent_dir,
                        &prepared.file_name,
                        &prepared.destination,
                    )?;
                    prepared
                        .parent_dir
                        .rename(&temp_name, &prepared.parent_dir, &prepared.file_name)
                        .with_context(|| {
                            format!(
                                "atomically replacing export destination {}",
                                prepared.destination.display()
                            )
                        })?;
                }
            }
            Ok(bytes)
        })();
        drop(temp_file);
        match publication {
            Ok(bytes) => {
                let _ = prepared.parent_dir.remove_file(&temp_name);
                Ok(ExportOutcome {
                    destination: prepared.destination.clone(),
                    bytes,
                    diagnostics: rendered_diagnostics,
                })
            }
            Err(error) => {
                let _ = prepared.parent_dir.remove_file(&temp_name);
                Err(error).context("publishing export")
            }
        }
    }

    fn resolve_export_destination(&self, input: &Path, canonical_root: &Path) -> Result<PathBuf> {
        let expanded = match expand_leading_home(input)? {
            Some(expanded) => expanded,
            None if input.is_absolute() => input.to_path_buf(),
            None => self
                .root
                .parent()
                .context("Nole root has no parent directory")?
                .join(input),
        };
        let normalized = normalize_lexical(expanded)?;
        if normalized.starts_with(canonical_root) {
            bail!("export destination must be outside the Nole root");
        }
        let parent = normalized
            .parent()
            .context("export destination has no parent")?;
        if let Some(base) = self.root.parent().filter(|base| parent.starts_with(base)) {
            reject_symlink_components_below(base, parent)?;
        } else {
            reject_existing_symlink_components(parent)?;
        }
        let canonical_parent = fs::canonicalize(parent).with_context(|| {
            format!(
                "export destination parent does not exist: {}",
                parent.display()
            )
        })?;
        if !fs::metadata(&canonical_parent)?.is_dir() {
            bail!("export destination parent is not a directory");
        }
        if canonical_parent.starts_with(canonical_root) {
            bail!("export destination must be outside the Nole root");
        }
        let name = normalized
            .file_name()
            .context("export destination needs a file name")?;
        Ok(canonical_parent.join(name))
    }

    /// Resolve `destination` exactly as [`Self::prepare_export`] would and
    /// report whether it currently names an existing regular file that is not
    /// a symlink — the only destination an explicit overwrite may replace.
    /// Directories, symlinks, special files, and missing paths report `false`;
    /// a resolution failure is returned so the caller can surface the real
    /// export error instead.
    pub fn export_destination_is_overwritable(
        &self,
        destination: impl AsRef<Path>,
    ) -> Result<bool> {
        let canonical_root = fs::canonicalize(&self.root).context("canonicalizing Nole root")?;
        let resolved = self.resolve_export_destination(destination.as_ref(), &canonical_root)?;
        let Ok(symlink_metadata) = fs::symlink_metadata(&resolved) else {
            return Ok(false);
        };
        if symlink_metadata.file_type().is_symlink() {
            return Ok(false);
        }
        Ok(fs::metadata(&resolved).is_ok_and(|metadata| metadata.is_file()))
    }

    fn revalidate_export_source(&self, prepared: &PreparedExport) -> Result<()> {
        let canonical_root = fs::canonicalize(&self.root)?;
        reject_symlink_components_below(&canonical_root, &prepared.source)?;
        let canonical = fs::canonicalize(&prepared.source)?;
        let metadata = fs::metadata(&canonical)?;
        if canonical != prepared.identity.canonical_path
            || !metadata.is_file()
            || metadata.len() != prepared.identity.length
            || metadata.modified()? != prepared.identity.modified
        {
            bail!("export source changed after preparation");
        }
        Ok(())
    }

    fn revalidate_export(&self, prepared: &PreparedExport) -> Result<()> {
        self.revalidate_export_source(prepared)?;
        // The destination is display-only for the actual publication, which
        // runs against the directory handle bound at prepare time. Re-resolve
        // the stored absolute path purely as a consistency check so a moved
        // parent surfaces a clear error instead of silently publishing
        // somewhere the user is no longer looking.
        let canonical_root = fs::canonicalize(&self.root)?;
        let resolved = self.resolve_export_destination(&prepared.destination, &canonical_root)?;
        if resolved != prepared.destination {
            bail!("export destination changed after preparation");
        }
        Ok(())
    }
}

/// Create a unique hidden temp file in the destination's directory, relative
/// to the parent directory handle bound at prepare time. The temp lives next
/// to the final target so the atomic publish below stays on one filesystem;
/// `create_new` guarantees no two publishers collide.
pub(super) fn create_export_temp(
    parent: &cap_std::fs::Dir,
    file_name: &OsStr,
    destination: &Path,
) -> Result<(cap_std::fs::File, OsString)> {
    let name = Path::new(file_name)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("export");
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    for attempt in 0..16u32 {
        let temp_name = format!(
            ".{name}.nole-export-{}-{nonce}-{attempt}.tmp",
            std::process::id()
        );
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        match parent.open_with(&temp_name, &options) {
            Ok(file) => return Ok((file, temp_name.into())),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "creating export temp file next to {}",
                        destination.display()
                    )
                })
            }
        }
    }
    bail!(
        "could not allocate a unique export temp file next to {}",
        destination.display()
    )
}

/// Report whether a directory entry exists without following a symlink.
/// `Dir::exists` follows links on some platforms, which can report false for
/// dangling links even though CreateNew must reject their occupied names.
pub(super) fn destination_entry_exists(
    parent: &cap_std::fs::Dir,
    file_name: &OsStr,
) -> io::Result<bool> {
    match parent.symlink_metadata(file_name) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Verify the destination — a single file name relative to the bound parent
/// directory — is an existing regular file that is not a symlink: the only
/// target [`ExportDestinationPolicy::ReplaceExisting`] may replace.
/// Directories, symlinks, special files, and missing paths are all rejected so
/// an explicit overwrite can never clobber anything but a plain file.
pub(super) fn validate_replaceable_destination(
    parent: &cap_std::fs::Dir,
    file_name: &OsStr,
    destination: &Path,
) -> Result<()> {
    let symlink_metadata = match parent.symlink_metadata(file_name) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!(
                "export destination does not exist: {}",
                destination.display()
            )
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!("inspecting export destination {}", destination.display())
            })
        }
    };
    if symlink_metadata.is_symlink() {
        bail!(
            "export destination is a symlink and cannot be replaced: {}",
            destination.display()
        );
    }
    let metadata = parent
        .metadata(file_name)
        .with_context(|| format!("inspecting export destination {}", destination.display()))?;
    if !metadata.is_file() {
        bail!(
            "export destination is not a regular file: {}",
            destination.display()
        );
    }
    Ok(())
}

/// Publish `temp_name` as `file_name`, both relative to the bound parent
/// directory, without ever overwriting an existing file.
///
/// A hard link is atomic and fails when the destination already exists, which
/// preserves the no-overwrite guarantee across the final publication race.
/// Filesystems that cannot create hard links fail explicitly; an
/// exists-then-rename fallback would be racy because rename may overwrite a
/// destination created between the check and the rename.
pub(super) fn publish_no_overwrite(
    parent: &cap_std::fs::Dir,
    temp_name: &OsStr,
    file_name: &OsStr,
    destination: &Path,
) -> Result<()> {
    match parent.hard_link(temp_name, parent, file_name) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            bail!(
                "export destination already exists: {}",
                destination.display()
            )
        }
        Err(error) => Err(error).with_context(|| {
            format!(
                "atomically publishing export {}; the destination filesystem must support hard links",
                destination.display()
            )
        }),
    }
}

pub(super) fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub(super) fn sha256_file(path: &Path) -> Result<[u8; 32]> {
    let file =
        File::open(path).with_context(|| format!("opening {} for hashing", path.display()))?;
    sha256_reader(BufReader::with_capacity(64 * 1024, file))
}

pub(super) fn sha256_reader(mut reader: impl Read) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

pub(super) fn copy_and_hash(
    reader: &mut impl Read,
    writer: &mut impl Write,
) -> Result<(u64, [u8; 32])> {
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        writer.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        total = total
            .checked_add(u64::try_from(read).context("export is too large")?)
            .context("export is too large")?;
    }
    Ok((total, hasher.finalize().into()))
}

pub(super) fn verify_export_source_content(prepared: &PreparedExport) -> Result<()> {
    if sha256_file(&prepared.source)? != prepared.identity.content_sha256 {
        bail!("export source content changed after preparation");
    }
    Ok(())
}

pub(super) fn validate_open_export_source(source: &File, prepared: &PreparedExport) -> Result<()> {
    let metadata = source
        .metadata()
        .context("reading opened export source metadata")?;
    if !metadata.is_file()
        || metadata.len() != prepared.identity.length
        || metadata.modified()? != prepared.identity.modified
    {
        bail!("export source changed after preparation");
    }
    Ok(())
}
