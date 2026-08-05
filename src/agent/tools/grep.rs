//! Bounded-output, ripgrep-style local text search.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use regex::bytes::{Regex, RegexBuilder};
use serde_json::{json, Value};

use super::util::{
    display_path, range_schema, required_string, truncate_chars, RangeSelector, MAX_SEARCH_RESULTS,
    MAX_SEARCH_SNIPPET_CHARS,
};
use crate::agent::{canonical_root, Tool, ToolExecutionPolicy};

pub struct Grep {
    root: PathBuf,
}

impl Grep {
    pub fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
        })
    }
}

#[async_trait::async_trait]
impl Tool for Grep {
    fn name(&self) -> &'static str {
        "grep"
    }

    fn execution_policy(&self) -> ToolExecutionPolicy {
        ToolExecutionPolicy::LocalRead
    }

    fn description(&self) -> &'static str {
        "Ripgrep-style regex search in a local file or directory. Searches files of any size line by line, respects ignore files, never follows symlinks, and returns matching lines with one-based line and byte-column positions using range pagination."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Rust regular expression to search for, or literal text when fixed_strings is true"
                },
                "path": {
                    "type": "string",
                    "minLength": 1,
                    "description": "File or directory to search, absolute or relative to the Nole root; defaults to the Nole root"
                },
                "case_sensitive": {
                    "type": "boolean",
                    "description": "Whether matching is case-sensitive; defaults to true"
                },
                "fixed_strings": {
                    "type": "boolean",
                    "description": "Treat pattern as literal text instead of a regular expression; defaults to false"
                },
                "include": {
                    "type": "array",
                    "items": { "type": "string", "minLength": 1 },
                    "maxItems": 32,
                    "description": "Optional glob patterns limiting searched files, for example [\"*.rs\", \"src/**\"]"
                },
                "range": range_schema(MAX_SEARCH_RESULTS)
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let pattern = required_string(input, "pattern")?;
        if pattern.is_empty() {
            bail!("pattern must not be empty");
        }
        let requested = input.get("path").and_then(Value::as_str).unwrap_or(".");
        let case_sensitive = optional_bool(input, "case_sensitive", true)?;
        let fixed_strings = optional_bool(input, "fixed_strings", false)?;
        let selector = RangeSelector::from_input(input, MAX_SEARCH_RESULTS)?;
        let includes = include_patterns(input)?;
        let root = self.root.clone();
        let requested = requested.to_string();
        let pattern = pattern.to_string();

        tokio::task::spawn_blocking(move || {
            execute_search(
                &root,
                &requested,
                &pattern,
                case_sensitive,
                fixed_strings,
                &includes,
                selector,
            )
        })
        .await
        .context("joining grep search")?
    }
}

fn optional_bool(input: &Value, name: &str, default: bool) -> Result<bool> {
    match input.get(name) {
        Some(value) => value
            .as_bool()
            .with_context(|| format!("field {name} must be a boolean")),
        None => Ok(default),
    }
}

fn include_patterns(input: &Value) -> Result<Vec<String>> {
    let Some(value) = input.get("include") else {
        return Ok(Vec::new());
    };
    let values = value.as_array().context("field include must be an array")?;
    values
        .iter()
        .map(|value| {
            let pattern = value
                .as_str()
                .context("field include items must be strings")?;
            if pattern.is_empty() {
                bail!("field include items must not be empty");
            }
            Ok(pattern.to_string())
        })
        .collect()
}

fn execute_search(
    root: &Path,
    requested: &str,
    pattern: &str,
    case_sensitive: bool,
    fixed_strings: bool,
    includes: &[String],
    selector: RangeSelector,
) -> Result<String> {
    let unresolved = Path::new(requested);
    let unresolved = if unresolved.is_absolute() {
        unresolved.to_path_buf()
    } else {
        root.join(unresolved)
    };
    let metadata = std::fs::symlink_metadata(&unresolved)
        .with_context(|| format!("checking grep path {}", unresolved.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("grep path cannot be a symlink: {}", unresolved.display());
    }
    if !metadata.is_file() && !metadata.is_dir() {
        bail!(
            "grep path must be a regular file or directory: {}",
            unresolved.display()
        );
    }
    let search_path = std::fs::canonicalize(&unresolved)
        .with_context(|| format!("resolving grep path {}", unresolved.display()))?;
    let expression = if fixed_strings {
        regex::escape(pattern)
    } else {
        pattern.to_string()
    };
    let regex = RegexBuilder::new(&expression)
        .case_insensitive(!case_sensitive)
        .build()
        .with_context(|| format!("invalid grep pattern {pattern:?}"))?;
    let include_set = build_include_set(includes)?;
    let search_base = if metadata.is_dir() {
        search_path.as_path()
    } else {
        search_path.parent().unwrap_or(root)
    };

    let mut state = SearchState {
        root,
        search_base,
        regex: &regex,
        include_set: include_set.as_ref(),
        selector,
        total: 0,
        items: Vec::new(),
    };
    if metadata.is_file() {
        state.search_file(&search_path)?;
    } else {
        let walker = WalkBuilder::new(&search_path)
            .follow_links(false)
            .standard_filters(true)
            .sort_by_file_path(|left, right| left.cmp(right))
            .build();
        for entry in walker {
            let entry = entry.with_context(|| format!("walking {}", search_path.display()))?;
            if entry.file_type().is_some_and(|kind| kind.is_file()) {
                state.search_file(entry.path())?;
            }
        }
    }

    let page = selector.window(state.total);
    let mut result = json!({
        "pattern": pattern,
        "path": display_path(root, &search_path),
        "case_sensitive": case_sensitive,
        "fixed_strings": fixed_strings,
        "range": selector.as_string(),
        "returned": state.items.len(),
        "total": state.total,
        "has_more": page.has_more(),
        "items": state.items,
    });
    if !includes.is_empty() {
        result["include"] = json!(includes);
    }
    if let Some(next) = page.next() {
        result["next"] = json!(next);
    }
    serde_json::to_string_pretty(&result).context("encoding grep results")
}

fn build_include_set(patterns: &[String]) -> Result<Option<GlobSet>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = GlobBuilder::new(pattern)
            .literal_separator(false)
            .build()
            .with_context(|| format!("invalid include glob {pattern:?}"))?;
        builder.add(glob);
    }
    builder.build().context("building include globs").map(Some)
}

struct SearchState<'a> {
    root: &'a Path,
    search_base: &'a Path,
    regex: &'a Regex,
    include_set: Option<&'a GlobSet>,
    selector: RangeSelector,
    total: usize,
    items: Vec<Value>,
}

impl SearchState<'_> {
    fn search_file(&mut self, path: &Path) -> Result<()> {
        let relative = path.strip_prefix(self.search_base).unwrap_or(path);
        if self
            .include_set
            .is_some_and(|patterns| !patterns.is_match(relative))
        {
            return Ok(());
        }
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut reader = BufReader::new(file);
        let mut bytes = Vec::new();
        let mut line = 0usize;
        loop {
            bytes.clear();
            if reader
                .read_until(b'\n', &mut bytes)
                .with_context(|| format!("searching {}", path.display()))?
                == 0
            {
                break;
            }
            line += 1;
            while matches!(bytes.last(), Some(b'\n' | b'\r')) {
                bytes.pop();
            }
            let Some(found) = self.regex.find(&bytes) else {
                continue;
            };
            self.total = self.total.saturating_add(1);
            if self.total < self.selector.start || self.total > self.selector.end {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            self.items.push(json!({
                "path": display_path(self.root, path),
                "line": line,
                "column": found.start() + 1,
                "snippet": truncate_chars(&text, MAX_SEARCH_SNIPPET_CHARS),
            }));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::Value;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn searches_large_files_without_loading_or_size_rejection() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("large.txt");
        let mut source = vec![b'x'; 2_000_000];
        source.extend_from_slice(b"\nNeedle at the end\n");
        fs::write(&path, source).unwrap();

        let result: Value = serde_json::from_str(
            &execute_search(
                directory.path(),
                path.to_str().unwrap(),
                "Needle",
                true,
                false,
                &[],
                RangeSelector::parse("1-10", MAX_SEARCH_RESULTS).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(result["total"], 1);
        assert_eq!(result["items"][0]["line"], 2);
    }

    #[test]
    fn regex_case_and_include_filters_are_observable() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("one.rs"), "Alpha 42\nalpha 7\n").unwrap();
        fs::write(directory.path().join("two.md"), "Alpha 99\n").unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();

        let result: Value = serde_json::from_str(
            &execute_search(
                &root,
                ".",
                r"alpha \d+",
                false,
                false,
                &["*.rs".to_string()],
                RangeSelector::parse("1-1", MAX_SEARCH_RESULTS).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(result["returned"], 1);
        assert_eq!(result["total"], 2);
        assert_eq!(result["has_more"], true);
        assert_eq!(result["next"], "2-2");
        assert_eq!(result["items"][0]["path"], "one.rs");
        assert_eq!(result["items"][0]["column"], 1);
    }

    #[cfg(unix)]
    #[test]
    fn directory_walk_never_follows_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "needle\n").unwrap();
        symlink(outside.path(), directory.path().join("linked")).unwrap();

        let result: Value = serde_json::from_str(
            &execute_search(
                directory.path(),
                ".",
                "needle",
                true,
                true,
                &[],
                RangeSelector::parse("1-10", MAX_SEARCH_RESULTS).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(result["total"], 0);
    }
}
