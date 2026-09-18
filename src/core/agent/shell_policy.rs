//! Hard safety checks applied before Agent shell command approval.

use std::io::Cursor;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use brush_parser::ast::{
    AssignmentValue, Command, CommandPrefixOrSuffixItem, CompoundCommand, CompoundList,
    IoFileRedirectTarget, IoRedirect, RedirectList, SimpleCommand, Word,
};
use brush_parser::word::{Parameter, ParameterExpr, TildeExpr, WordPiece, WordPieceWithSource};
use brush_parser::{Parser, ParserOptions};

const MAX_NESTED_COMMAND_DEPTH: usize = 16;

pub(crate) fn validate_shell_command(command: &str, cwd: &Path, nole_root: &Path) -> Result<()> {
    validate_script(command, cwd, nole_root, 0)
}

fn validate_script(script: &str, cwd: &Path, nole_root: &Path, depth: usize) -> Result<()> {
    if depth > MAX_NESTED_COMMAND_DEPTH {
        bail!("environment safety policy rejected deeply nested shell command");
    }
    let mut parser = Parser::new(Cursor::new(script), &ParserOptions::default());
    let program = parser
        .parse_program()
        .context("environment safety policy could not parse shell command")?;
    for command in &program.complete_commands {
        inspect_list(command, cwd, nole_root, depth)?;
    }
    Ok(())
}

fn inspect_list(list: &CompoundList, cwd: &Path, nole_root: &Path, depth: usize) -> Result<()> {
    for item in &list.0 {
        for (_, pipeline) in item.0.iter() {
            for command in &pipeline.seq {
                inspect_command(command, cwd, nole_root, depth)?;
            }
        }
    }
    Ok(())
}

fn inspect_command(command: &Command, cwd: &Path, nole_root: &Path, depth: usize) -> Result<()> {
    match command {
        Command::Simple(simple) => inspect_simple(simple, cwd, nole_root, depth),
        Command::Compound(compound, redirects) => {
            inspect_compound(compound, cwd, nole_root, depth)?;
            inspect_redirects(redirects.as_ref(), cwd, nole_root, depth)
        }
        Command::Function(function) => {
            inspect_compound(&function.body.0, cwd, nole_root, depth)?;
            inspect_redirects(function.body.1.as_ref(), cwd, nole_root, depth)
        }
        Command::ExtendedTest(_, redirects) => {
            inspect_redirects(redirects.as_ref(), cwd, nole_root, depth)
        }
    }
}

fn inspect_compound(
    command: &CompoundCommand,
    cwd: &Path,
    nole_root: &Path,
    depth: usize,
) -> Result<()> {
    match command {
        CompoundCommand::Arithmetic(_) => Ok(()),
        CompoundCommand::ArithmeticForClause(command) => {
            inspect_list(&command.body.list, cwd, nole_root, depth)
        }
        CompoundCommand::BraceGroup(command) => inspect_list(&command.list, cwd, nole_root, depth),
        CompoundCommand::Subshell(command) => inspect_list(&command.list, cwd, nole_root, depth),
        CompoundCommand::ForClause(command) => {
            inspect_list(&command.body.list, cwd, nole_root, depth)
        }
        CompoundCommand::CaseClause(command) => {
            for case in &command.cases {
                if let Some(list) = &case.cmd {
                    inspect_list(list, cwd, nole_root, depth)?;
                }
            }
            Ok(())
        }
        CompoundCommand::IfClause(command) => {
            inspect_list(&command.condition, cwd, nole_root, depth)?;
            inspect_list(&command.then, cwd, nole_root, depth)?;
            if let Some(elses) = &command.elses {
                for branch in elses {
                    if let Some(condition) = &branch.condition {
                        inspect_list(condition, cwd, nole_root, depth)?;
                    }
                    inspect_list(&branch.body, cwd, nole_root, depth)?;
                }
            }
            Ok(())
        }
        CompoundCommand::WhileClause(command) | CompoundCommand::UntilClause(command) => {
            inspect_list(&command.0, cwd, nole_root, depth)?;
            inspect_list(&command.1.list, cwd, nole_root, depth)
        }
        CompoundCommand::Coprocess(command) => {
            inspect_command(&command.body, cwd, nole_root, depth)
        }
    }
}

fn inspect_simple(
    simple: &SimpleCommand,
    cwd: &Path,
    nole_root: &Path,
    depth: usize,
) -> Result<()> {
    let mut words = Vec::new();
    if let Some(prefix) = &simple.prefix {
        for item in &prefix.0 {
            inspect_command_item(item, None, cwd, nole_root, depth)?;
        }
    }
    if let Some(name) = &simple.word_or_name {
        words.push(name);
    }
    if let Some(suffix) = &simple.suffix {
        for item in &suffix.0 {
            inspect_command_item(item, Some(&mut words), cwd, nole_root, depth)?;
        }
    }

    for word in &words {
        inspect_word_substitutions(word, cwd, nole_root, depth)?;
    }
    let Some(command_index) = effective_command_index(&words) else {
        return Ok(());
    };
    let command_name = literal_word(words[command_index]).unwrap_or_default();
    let basename = Path::new(&command_name)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&command_name);
    let args = &words[command_index + 1..];

    if basename == "rm" {
        inspect_rm(args, cwd, nole_root)?;
    } else if matches!(basename, "sh" | "bash" | "zsh" | "brush") {
        inspect_shell_c(args, cwd, nole_root, depth)?;
    } else if basename == "eval" {
        let nested = args
            .iter()
            .map(|word| literal_word(word))
            .collect::<Option<Vec<_>>>();
        if let Some(nested) = nested {
            validate_script(&nested.join(" "), cwd, nole_root, depth + 1)?;
        }
    }
    Ok(())
}

fn inspect_command_item<'a>(
    item: &'a CommandPrefixOrSuffixItem,
    words: Option<&mut Vec<&'a Word>>,
    cwd: &Path,
    nole_root: &Path,
    depth: usize,
) -> Result<()> {
    match item {
        CommandPrefixOrSuffixItem::IoRedirect(redirect) => {
            inspect_redirect(redirect, cwd, nole_root, depth)
        }
        CommandPrefixOrSuffixItem::Word(word) => {
            inspect_word_substitutions(word, cwd, nole_root, depth)?;
            if let Some(words) = words {
                words.push(word);
            }
            Ok(())
        }
        CommandPrefixOrSuffixItem::AssignmentWord(assignment, word) => {
            inspect_word_substitutions(word, cwd, nole_root, depth)?;
            match &assignment.value {
                AssignmentValue::Scalar(value) => {
                    inspect_word_substitutions(value, cwd, nole_root, depth)
                }
                AssignmentValue::Array(values) => {
                    for (key, value) in values {
                        if let Some(key) = key {
                            inspect_word_substitutions(key, cwd, nole_root, depth)?;
                        }
                        inspect_word_substitutions(value, cwd, nole_root, depth)?;
                    }
                    Ok(())
                }
            }
        }
        CommandPrefixOrSuffixItem::ProcessSubstitution(_, command) => {
            inspect_list(&command.list, cwd, nole_root, depth)
        }
    }
}

fn inspect_redirects(
    redirects: Option<&RedirectList>,
    cwd: &Path,
    nole_root: &Path,
    depth: usize,
) -> Result<()> {
    if let Some(redirects) = redirects {
        for redirect in &redirects.0 {
            inspect_redirect(redirect, cwd, nole_root, depth)?;
        }
    }
    Ok(())
}

fn inspect_redirect(
    redirect: &IoRedirect,
    cwd: &Path,
    nole_root: &Path,
    depth: usize,
) -> Result<()> {
    match redirect {
        IoRedirect::File(_, _, target) => match target {
            IoFileRedirectTarget::Filename(word) | IoFileRedirectTarget::Duplicate(word) => {
                inspect_word_substitutions(word, cwd, nole_root, depth)
            }
            IoFileRedirectTarget::ProcessSubstitution(_, command) => {
                inspect_list(&command.list, cwd, nole_root, depth)
            }
            IoFileRedirectTarget::Fd(_) => Ok(()),
        },
        IoRedirect::HereDocument(_, document) => {
            if document.requires_expansion {
                inspect_word_substitutions(&document.doc, cwd, nole_root, depth)?;
            }
            Ok(())
        }
        IoRedirect::HereString(_, word) | IoRedirect::OutputAndError(word, _) => {
            inspect_word_substitutions(word, cwd, nole_root, depth)
        }
    }
}

fn effective_command_index(words: &[&Word]) -> Option<usize> {
    let mut index = 0;
    loop {
        let word = literal_word(*words.get(index)?)?;
        let basename = Path::new(&word)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&word);
        if !matches!(basename, "command" | "env" | "sudo") {
            return Some(index);
        }
        index += 1;
        while let Some(argument) = words.get(index).and_then(|word| literal_word(word)) {
            if argument == "--" {
                index += 1;
                break;
            }
            let takes_value = match basename {
                "sudo" => matches!(
                    argument.as_str(),
                    "-C" | "-D"
                        | "-g"
                        | "-h"
                        | "-p"
                        | "-R"
                        | "-T"
                        | "-u"
                        | "--chdir"
                        | "--close-from"
                        | "--group"
                        | "--host"
                        | "--prompt"
                        | "--role"
                        | "--type"
                        | "--user"
                ),
                "env" => matches!(argument.as_str(), "-u" | "--unset" | "-C" | "--chdir"),
                _ => false,
            };
            if argument.starts_with('-') || (basename == "env" && argument.contains('=')) {
                index += 1;
                if takes_value {
                    index += 1;
                }
                continue;
            }
            break;
        }
    }
}

fn inspect_shell_c(args: &[&Word], cwd: &Path, nole_root: &Path, depth: usize) -> Result<()> {
    for pair in args.windows(2) {
        if literal_word(pair[0]).as_deref() == Some("-c") {
            if let Some(script) = literal_word(pair[1]) {
                validate_script(&script, cwd, nole_root, depth + 1)?;
            }
            break;
        }
    }
    Ok(())
}

fn inspect_rm(args: &[&Word], cwd: &Path, nole_root: &Path) -> Result<()> {
    let mut recursive = false;
    let mut force = false;
    let mut options = true;
    let mut targets = Vec::new();
    for word in args {
        let Some(value) = literal_word(word) else {
            targets.push(*word);
            continue;
        };
        if options && value == "--" {
            options = false;
        } else if options && value.starts_with("--") {
            recursive |= value == "--recursive";
            force |= value == "--force";
        } else if options && value.starts_with('-') {
            recursive |= value[1..].chars().any(|flag| matches!(flag, 'r' | 'R'));
            force |= value[1..].contains('f');
        } else {
            targets.push(*word);
        }
    }
    if !(recursive && force) {
        return Ok(());
    }

    let home = dirs::home_dir().map(|path| lexical_normalize(&path));
    let nole_root = lexical_normalize(nole_root);
    for target in targets {
        let value = known_path_word(target, cwd, home.as_deref()).ok_or_else(|| {
            anyhow::anyhow!(
                "environment safety policy rejected recursive forced deletion with unresolved target {:?}; resolve it to an explicit narrow path first",
                target.value
            )
        })?;
        if targets_protected_path(&value, cwd, home.as_deref(), &nole_root) {
            bail!(
                "environment safety policy rejected recursive forced deletion of protected target {value:?}; use a narrower target or a recoverable trash operation"
            );
        }
    }
    Ok(())
}

fn targets_protected_path(value: &str, cwd: &Path, home: Option<&Path>, nole_root: &Path) -> bool {
    let prefix = value
        .find(['*', '?', '[', '{'])
        .map_or(value, |index| &value[..index]);
    let trimmed = prefix.trim_end_matches(['/', '\\']);
    let prefix = if trimmed.is_empty() && Path::new(prefix).is_absolute() {
        std::path::MAIN_SEPARATOR_STR
    } else {
        trimmed
    };
    let target = if prefix.is_empty() {
        lexical_normalize(cwd)
    } else {
        let path = Path::new(prefix);
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        };
        lexical_normalize(&absolute)
    };
    let cwd = lexical_normalize(cwd);
    target == Path::new("/")
        || cwd.starts_with(&target)
        || home.is_some_and(|home| home.starts_with(&target))
        || nole_root.starts_with(&target)
}

fn known_path_word(word: &Word, cwd: &Path, home: Option<&Path>) -> Option<String> {
    let pieces = brush_parser::word::parse(&word.value, &ParserOptions::default()).ok()?;
    known_pieces(&pieces, cwd, home)
}

fn literal_word(word: &Word) -> Option<String> {
    let pieces = brush_parser::word::parse(&word.value, &ParserOptions::default()).ok()?;
    literal_pieces(&pieces)
}

fn literal_pieces(pieces: &[WordPieceWithSource]) -> Option<String> {
    let mut value = String::new();
    for piece in pieces {
        match &piece.piece {
            WordPiece::Text(text)
            | WordPiece::SingleQuotedText(text)
            | WordPiece::AnsiCQuotedText(text)
            | WordPiece::EscapeSequence(text) => value.push_str(text),
            WordPiece::DoubleQuotedSequence(nested)
            | WordPiece::GettextDoubleQuotedSequence(nested) => {
                value.push_str(&literal_pieces(nested)?);
            }
            _ => return None,
        }
    }
    Some(value)
}

fn known_pieces(pieces: &[WordPieceWithSource], cwd: &Path, home: Option<&Path>) -> Option<String> {
    let mut value = String::new();
    for piece in pieces {
        match &piece.piece {
            WordPiece::Text(text)
            | WordPiece::SingleQuotedText(text)
            | WordPiece::AnsiCQuotedText(text)
            | WordPiece::EscapeSequence(text) => value.push_str(text),
            WordPiece::DoubleQuotedSequence(nested)
            | WordPiece::GettextDoubleQuotedSequence(nested) => {
                value.push_str(&known_pieces(nested, cwd, home)?);
            }
            WordPiece::TildeExpansion(TildeExpr::Home) => {
                value.push_str(home?.to_str()?);
            }
            WordPiece::TildeExpansion(TildeExpr::WorkingDir) => {
                value.push_str(cwd.to_str()?);
            }
            WordPiece::ParameterExpansion(ParameterExpr::Parameter {
                parameter: Parameter::Named(name),
                indirect: false,
            }) if name == "HOME" => value.push_str(home?.to_str()?),
            _ => return None,
        }
    }
    Some(value)
}

fn inspect_word_substitutions(
    word: &Word,
    cwd: &Path,
    nole_root: &Path,
    depth: usize,
) -> Result<()> {
    let pieces = brush_parser::word::parse(&word.value, &ParserOptions::default())
        .context("environment safety policy could not parse shell word")?;
    inspect_piece_substitutions(&pieces, cwd, nole_root, depth)
}

fn inspect_piece_substitutions(
    pieces: &[WordPieceWithSource],
    cwd: &Path,
    nole_root: &Path,
    depth: usize,
) -> Result<()> {
    for piece in pieces {
        match &piece.piece {
            WordPiece::CommandSubstitution(script)
            | WordPiece::BackquotedCommandSubstitution(script) => {
                validate_script(script, cwd, nole_root, depth + 1)?;
            }
            WordPiece::DoubleQuotedSequence(nested)
            | WordPiece::GettextDoubleQuotedSequence(nested) => {
                inspect_piece_substitutions(nested, cwd, nole_root, depth)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(command: &str) -> Result<()> {
        validate_shell_command(
            command,
            Path::new("/Users/test/notes"),
            Path::new("/Users/test/notes"),
        )
    }

    #[test]
    fn rejects_recursive_forced_deletion_of_broad_targets() {
        for command in [
            "rm -rf /",
            "rm -fr .",
            "rm --recursive --force ..",
            "rm -rf \"$UNKNOWN_TARGET\"",
            "rm -rf ~",
            "rm -rf \"$HOME\"",
            "command rm -Rf /Users/test/notes",
            "env FOO=bar rm -rf /Users/test/*",
            "sudo rm -rf /Users",
            "sudo -u root rm -rf /Users",
            "true && rm -rf /",
            "if true; then rm -rf /; fi",
            "sh -c 'rm -rf /'",
            "printf '%s' \"$(rm -rf /)\"",
            ": > \"$(rm -rf /)\"",
        ] {
            assert!(policy(command).is_err(), "allowed {command:?}");
        }
    }

    #[test]
    fn allows_narrow_or_non_recursive_removal_and_quoted_text() {
        for command in [
            "rm -rf build/tmp",
            "rm -r /Users/test/notes",
            "rm -f /Users/test/notes/file",
            "printf '%s' 'rm -rf /'",
            "echo rm -rf /",
            "sudo echo rm -rf /",
            "rm -rf '$HOME'",
        ] {
            assert!(policy(command).is_ok(), "rejected {command:?}");
        }
    }
}
