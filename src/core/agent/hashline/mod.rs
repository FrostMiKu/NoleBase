//! Hashline patch language: parsing, block resolution, lowering, rebase.
//!
//! Native implementation of the upstream
//! [`oh-my-pi` hashline patch language](https://github.com/can1357/oh-my-pi/tree/main/packages/hashline)
//! (MIT), adapted to `nole`'s versioned snapshot file-edit path. Modules are
//! split by stage: `parser` turns patch text into a `Patch`, `block` resolves
//! syntactic spans, `plan` lowers resolved hunks into `LineEdit`s, `rebase`
//! reconciles plans against drifted on-disk files, and `registers` holds the
//! session-scoped paste/cut registers shared across `edit` calls.

pub(crate) mod block;
pub(crate) mod parser;
pub(crate) mod plan;
pub(crate) mod rebase;
pub(crate) mod registers;

pub(crate) use block::syntax_for_path;
pub(crate) use parser::parse_patch;
pub(crate) use plan::{plan_section, FileOp, PlannedFile};
pub(crate) use rebase::rebase_edits;
pub(crate) use registers::RegisterBank;

/// One lowered line-range edit against ORIGINAL 0-based line numbering.
/// `start_line` is the 0-based index of the first replaced line;
/// `end_line_exclusive` is the 0-based exclusive end (== `start_line` for a
/// pure insertion). `anchor_line` is the 1-based line the model named, for
/// diagnostics only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LineEdit {
    pub(crate) start_line: usize,
    pub(crate) end_line_exclusive: usize,
    pub(crate) lines: Vec<String>,
    pub(crate) insertion: bool,
    pub(crate) anchor_line: usize,
}

/// A parsed patch: one or more `[PATH#TAG]` sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Patch {
    pub(crate) sections: Vec<Section>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Section {
    pub(crate) path: String,
    pub(crate) tag: String,     // canonical UPPERCASE 4-hex
    pub(crate) line_num: usize, // 1-based line of the header inside the patch text
    pub(crate) hunks: Vec<Hunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hunk {
    pub(crate) line_num: usize,
    pub(crate) op: Op,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Op {
    Put {
        locator: PutLocator,
        payload: Payload,
    },
    Cut {
        locator: SpanLocator,
        register: Option<String>,
    },
    Rem,
    Mv {
        dest: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PutLocator {
    Span(SpanLocator),
    Gap(GapLocator),
}

/// 1-based inclusive span locators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpanLocator {
    Range { start: usize, end: usize },
    Block(usize),
}

/// 1-based gap locators. `Eof` is `>$`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GapLocator {
    Before(usize),
    After(usize),
    AfterBlock(usize),
    Eof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Payload {
    Body(Vec<String>),
    Register(Option<String>),
}
