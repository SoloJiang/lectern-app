//! Edit mode's parse (docs/design/file_reader.md「编辑」): one parser and
//! the last tree of an edit session's buffer, reparsed incrementally.
//!
//! A round takes the buffer's text and the edit from the last round's text
//! to it: the last tree, edited to match (`Tree::edit`), is reused by the
//! parse, and the highlight query runs again only on the lines that the edit
//! and the tree's changed ranges (`Tree::changed_ranges`) touch. The other
//! lines keep the last round's tokens, moved by the edit
//! ([`WindowSpans::shifted`]). Every capture that colours a byte outside
//! those lines belongs to a node whose structure and text did not change,
//! so the result is the tokens a whole parse of the text would give. A
//! round is sliced like [`FullParse`](super::FullParse): the parser and the
//! query keep their progress between steps. The first round (and a round
//! whose edit does not describe the texts) parses and queries the whole
//! text.

use std::ops::Range;
use std::sync::Arc;

use tree_sitter::{InputEdit, Parser, Point, Tree};

use super::{Budget, Halt, Input, Outcome, Querying, Source, Watch, new_parser, parse_from};
use crate::change::{self, TextEdit};
use crate::health::{self, Health};
use crate::lang::{Lang, LangHandle};
use crate::spans::WindowSpans;

/// A buffer's text as a parse source.
struct Buffer(Arc<str>);

impl Source for Buffer {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }

    fn chunk(&self, at: u64) -> Option<&[u8]> {
        usize::try_from(at)
            .ok()
            .and_then(|at| self.0.as_bytes().get(at..))
    }
}

/// An edit session's parser, its last tree and that tree's spans.
pub(crate) struct EditParse {
    lang: LangHandle,
    health: Health,
    /// None once the grammar or its query is unusable.
    parser: Option<Parser>,
    last: Option<Last>,
    round: Option<Round>,
}

/// The last round: the text, its tree and its tokens.
struct Last {
    text: Arc<str>,
    tree: Tree,
    spans: Arc<WindowSpans>,
}

struct Round {
    src: Arc<Buffer>,
    /// The last tree edited to this text, and the edit; None: the whole
    /// text is parsed.
    old: Option<(Tree, TextEdit)>,
    stage: Stage,
}

enum Stage {
    Parsing,
    Querying(Box<Requery>),
}

/// The query of a round's new tree.
struct Requery {
    query: Querying,
    /// The lines queried again (line starts, or the text's end).
    region: Range<u64>,
    /// The last round's tokens moved to this text.
    outer: Option<WindowSpans>,
}

impl EditParse {
    /// Compiles the language's query on first use (blocking). Background
    /// threads only.
    pub(crate) fn new(lang: &Lang) -> EditParse {
        EditParse::new_in(health::global().handle(), lang)
    }

    pub(crate) fn new_in(health: Health, lang: &Lang) -> EditParse {
        let lang = lang.handle();
        let parser = new_parser(&lang);
        EditParse {
            lang,
            health,
            parser,
            last: None,
            round: None,
        }
    }

    /// Starts a round on `text`; `edit` takes the last round's text to it
    /// (None when unknown). An edit that does not describe the two texts is
    /// replaced by their difference, so the last tree is still reused.
    /// Linear in the text (the check and tree-sitter's points); call it on
    /// the thread that steps the round. A round under way is dropped.
    pub(crate) fn begin(&mut self, text: Arc<str>, edit: Option<TextEdit>) {
        if let Some(parser) = self.parser.as_mut() {
            // A halted parse of other input must not resume on this one.
            parser.reset();
        }
        let old = self.last.as_ref().map(|last| {
            let (old, new) = (last.text.as_bytes(), text.as_bytes());
            let edit = edit
                .filter(|e| change::describes(*e, old, new))
                .or_else(|| change::diff(old, new))
                // The same text: an empty edit at its end.
                .unwrap_or(TextEdit {
                    start: new.len(),
                    old_end: new.len(),
                    new_end: new.len(),
                });
            let mut tree = last.tree.clone();
            tree.edit(&input_edit(edit, old, new));
            (tree, edit)
        });
        self.round = Some(Round {
            src: Arc::new(Buffer(text)),
            old,
            stage: Stage::Parsing,
        });
    }

    /// One slice of the round: at most the budget's parse plus one query
    /// chunk. `Done` gives the round's text's tokens (exact), which become
    /// the base of the next round. `Unavailable` when no round is under way,
    /// or for good once the grammar version is disabled or the query does
    /// not compile.
    pub(crate) fn step(&mut self, budget: Budget<'_>) -> Outcome<Arc<WindowSpans>> {
        let EditParse {
            lang,
            health,
            parser,
            last,
            round,
        } = self;
        let (Some(compiled), Some(active), Some(stepping)) =
            (lang.compiled.clone(), parser.as_mut(), round.as_mut())
        else {
            return Outcome::Unavailable;
        };
        let src = stepping.src.clone();
        let len = src.len();
        let input = Input::of(&*src, 0, len);
        let watch = Watch::new(&budget);
        let Some(_guard) = health.enter(lang.name, lang.grammar_version, budget.budget) else {
            *parser = None;
            *round = None;
            return Outcome::Unavailable;
        };
        if let Stage::Parsing = stepping.stage {
            let old = stepping.old.as_ref().map(|(tree, _)| tree);
            let tree = match parse_from(active, input, old, &watch) {
                Ok(tree) => tree,
                Err(halt) => {
                    if halt == Halt::Unavailable {
                        *parser = None;
                        *round = None;
                    }
                    return halt.outcome();
                }
            };
            let text = src.0.as_bytes();
            let (region, outer) = match (&stepping.old, last.as_ref()) {
                (Some((old, edit)), Some(last)) => {
                    // The lines the edit touches (the moved spans dropped the
                    // tokens crossing its ends, on the line holding its new
                    // end too), and those whose structure changed.
                    let mut from = edit.start;
                    let mut to = change::line_end(text, edit.new_end);
                    for changed in old.changed_ranges(&tree) {
                        from = from.min(changed.start_byte);
                        if changed.end_byte > changed.start_byte {
                            to = to.max(change::line_end(text, changed.end_byte - 1));
                        }
                    }
                    let from = change::line_start(text, from);
                    let outer = last.spans.shifted(*edit, last.text.as_bytes(), text);
                    (from as u64..to as u64, Some(outer))
                }
                _ => (0..len, None),
            };
            let (from, to) = (region.start as usize, region.end as usize);
            let query = Querying::new(tree, input, from, to - from, to, region.start);
            stepping.stage = Stage::Querying(Box::new(Requery {
                query,
                region,
                outer,
            }));
        }
        let Stage::Querying(requery) = &mut stepping.stage else {
            return Outcome::Unavailable;
        };
        if let Err(halt) = requery
            .query
            .run(lang.precedence, &compiled, input, &watch, true)
        {
            return halt.outcome();
        }
        let Some(Round {
            src,
            stage: Stage::Querying(requery),
            ..
        }) = round.take()
        else {
            return Outcome::Unavailable;
        };
        let Requery {
            query,
            region,
            outer,
        } = *requery;
        let inner = query.builder.finish(true);
        let spans = Arc::new(WindowSpans::splice(outer.as_ref(), &inner, region, len));
        *last = Some(Last {
            text: src.0.clone(),
            tree: query.tree,
            spans: spans.clone(),
        });
        Outcome::Done(spans)
    }
}

#[cfg(test)]
impl EditParse {
    /// The last round's tree: what its spliced spans are checked against.
    pub(crate) fn tree(&self) -> Option<&Tree> {
        self.last.as_ref().map(|last| &last.tree)
    }
}

/// tree-sitter's description of `edit` from `old` to `new`.
pub(crate) fn input_edit(edit: TextEdit, old: &[u8], new: &[u8]) -> InputEdit {
    let point = |text: &[u8], at: usize| {
        let (row, column) = change::point(text, at);
        Point { row, column }
    };
    InputEdit {
        start_byte: edit.start,
        old_end_byte: edit.old_end,
        new_end_byte: edit.new_end,
        start_position: point(old, edit.start),
        old_end_position: point(old, edit.old_end),
        new_end_position: point(new, edit.new_end),
    }
}
