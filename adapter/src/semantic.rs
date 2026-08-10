use std::{
    collections::{BTreeMap, HashSet},
    fs,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

pub const TOKEN_TYPES: [&str; 6] = [
    "rustowlDefinitelyLive",
    "rustowlMaybeInitialized",
    "rustowlImmutableBorrow",
    "rustowlMutableBorrow",
    "rustowlMoveOrCall",
    "rustowlOutlive",
];

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct Decoration {
    #[serde(rename = "type")]
    pub kind: String,
    pub range: Range,
    #[serde(default)]
    pub hover_text: Option<String>,
    #[serde(default)]
    pub overlapped: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct DecorationPresentation {
    pub title: &'static str,
    pub summary: &'static str,
    pub facts: &'static [(&'static str, &'static str)],
    pub inlay_label: Option<&'static str>,
    pub priority: u8,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CursorResponse {
    #[serde(default)]
    pub is_analyzed: bool,
    #[serde(default)]
    pub status: Option<Value>,
    #[serde(default)]
    pub decorations: Vec<Decoration>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Token {
    line: u32,
    start: u32,
    length: u32,
    token_type: u32,
}

#[derive(Debug)]
struct HintCandidate {
    position: Position,
    label: String,
    tooltip: Option<String>,
    priority: u8,
}

#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
struct FlowEvent {
    position: Position,
    label: &'static str,
}

pub fn semantic_tokens(uri: &str, decorations: &[Decoration]) -> Vec<u32> {
    let source = source_for_uri(uri);
    let mut tokens = HashSet::new();
    for decoration in decorations
        .iter()
        .filter(|decoration| !decoration.overlapped)
    {
        let Some(token_type) = token_type(&decoration.kind) else {
            continue;
        };
        for (line, start, length) in split_range(decoration.range, source.as_deref()) {
            if length > 0 {
                tokens.insert(Token {
                    line,
                    start,
                    length,
                    token_type,
                });
            }
        }
    }

    let mut tokens: Vec<_> = tokens.into_iter().collect();
    tokens.sort_unstable();
    let mut encoded = Vec::with_capacity(tokens.len() * 5);
    let mut previous_line = 0;
    let mut previous_start = 0;
    for token in tokens {
        let delta_line = token.line - previous_line;
        let delta_start = if delta_line == 0 {
            token.start - previous_start
        } else {
            token.start
        };
        encoded.extend([delta_line, delta_start, token.length, token.token_type, 0]);
        previous_line = token.line;
        previous_start = token.start;
    }
    encoded
}

pub fn inlay_hints(uri: &str, requested_range: Range, decorations: &[Decoration]) -> Vec<Value> {
    let source = source_for_uri(uri);
    let mut per_line = BTreeMap::new();
    for decoration in decorations
        .iter()
        .filter(|decoration| !decoration.overlapped)
    {
        let Some(presentation) = decoration_presentation(&decoration.kind) else {
            continue;
        };
        let Some(label) = presentation.inlay_label else {
            continue;
        };
        let position = decoration.range.end;
        if position < requested_range.start || requested_range.end < position {
            continue;
        }

        upsert_hint(
            &mut per_line,
            HintCandidate {
                position,
                label: label.into(),
                tooltip: decoration_markdown(decoration),
                priority: presentation.priority,
            },
        );
    }

    if let Some(source) = source.as_deref() {
        for await_position in await_positions_in_source(source) {
            if await_position < requested_range.start || requested_range.end < await_position {
                continue;
            }
            if let Some(candidate) = async_hint(await_position, decorations) {
                upsert_hint(&mut per_line, candidate);
            }
        }
    }

    let mut hints = Vec::new();
    for (line, candidate) in per_line {
        let line_end = source
            .as_deref()
            .and_then(|source| source.lines().nth(line as usize))
            .map(|line| line.encode_utf16().count() as u32)
            .map(|character| Position { line, character });
        let position = line_end
            .filter(|position| *position <= requested_range.end)
            .unwrap_or(candidate.position);

        let mut hint = serde_json::Map::from_iter([
            ("position".into(), serde_json::to_value(position).unwrap()),
            ("label".into(), Value::String(candidate.label)),
            ("paddingLeft".into(), Value::Bool(true)),
        ]);
        if let Some(tooltip) = candidate.tooltip {
            hint.insert(
                "tooltip".into(),
                serde_json::json!({"kind": "markdown", "value": tooltip}),
            );
        }
        hints.push(Value::Object(hint));
    }
    hints
}

fn upsert_hint(per_line: &mut BTreeMap<u32, HintCandidate>, candidate: HintCandidate) {
    match per_line.entry(candidate.position.line) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(candidate);
        }
        std::collections::btree_map::Entry::Occupied(mut entry) => {
            if candidate.priority > entry.get().priority {
                entry.insert(candidate);
            }
        }
    }
}

pub fn identifier_positions(uri: &str, requested_range: Range) -> Vec<Position> {
    source_for_uri(uri)
        .map(|source| identifier_positions_in_source(&source, requested_range))
        .unwrap_or_default()
}

fn identifier_positions_in_source(source: &str, requested_range: Range) -> Vec<Position> {
    let mut positions = Vec::new();
    for (line_number, line) in source.lines().enumerate() {
        let line_number = line_number as u32;
        if line_number < requested_range.start.line || line_number > requested_range.end.line {
            continue;
        }

        let mut characters = line.chars().peekable();
        let mut utf16_column = 0_u32;
        while let Some(character) = characters.next() {
            let start = utf16_column;
            utf16_column += character.len_utf16() as u32;
            if !is_identifier_start(character) {
                continue;
            }

            let mut identifier = String::from(character);
            while let Some(next) = characters.peek().copied() {
                if !is_identifier_continue(next) {
                    break;
                }
                characters.next();
                identifier.push(next);
                utf16_column += next.len_utf16() as u32;
            }

            let position = Position {
                line: line_number,
                character: start,
            };
            if requested_range.start <= position
                && position <= requested_range.end
                && !is_rust_keyword(&identifier)
            {
                positions.push(position);
            }
        }
    }
    positions
}

fn is_identifier_start(character: char) -> bool {
    character == '_' || character.is_alphabetic()
}

fn is_identifier_continue(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}

fn is_rust_keyword(identifier: &str) -> bool {
    matches!(
        identifier,
        "as" | "async"
            | "await"
            | "break"
            | "const"
            | "continue"
            | "crate"
            | "dyn"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "union"
            | "unsafe"
            | "use"
            | "where"
            | "while"
            | "yield"
    )
}

fn await_positions_in_source(source: &str) -> Vec<Position> {
    let mut positions = Vec::new();
    for (line_number, line) in source.lines().enumerate() {
        let mut offset = 0;
        while let Some(relative) = line[offset..].find(".await") {
            let byte_index = offset + relative;
            let after = byte_index + ".await".len();
            if line[after..]
                .chars()
                .next()
                .is_some_and(is_identifier_continue)
            {
                offset = after;
                continue;
            }
            positions.push(Position {
                line: line_number as u32,
                character: line[..byte_index + 1].encode_utf16().count() as u32,
            });
            offset = after;
        }
    }
    positions
}

fn async_hint(position: Position, decorations: &[Decoration]) -> Option<HintCandidate> {
    let touches_tracked_state = decorations.iter().any(|decoration| {
        !decoration.overlapped
            && decoration.range.start.line <= position.line
            && position.line <= decoration.range.end.line
            && matches!(
                decoration.kind.as_str(),
                "lifetime"
                    | "definitely_live"
                    | "maybe_initialized"
                    | "imm_borrow"
                    | "mut_borrow"
                    | "outlive"
            )
    });
    if !touches_tracked_state {
        return None;
    }

    Some(HintCandidate {
        position,
        label: "← live across .await · stored in future".into(),
        tooltip: Some(
            "### RustOwl · State crosses `.await`\n\nOne or more RustOwl-tracked values remain live while this future may be suspended.\n\n- **State machine** · live values and references are retained inside the generated future\n- **Borrowing** · owners and exclusivity must remain valid until the relevant uses finish\n- **Cancellation** · dropping the future also drops state it retains\n- **Spawn check** · detached tasks may additionally require the future to be `Send + 'static`"
                .into(),
        ),
        priority: 120,
    })
}

pub fn ownership_flow_markdown(uri: &str, decorations: &[Decoration]) -> Option<String> {
    let mut events: Vec<_> = decorations
        .iter()
        .filter(|decoration| !decoration.overlapped)
        .filter_map(|decoration| {
            let label = match decoration.kind.as_str() {
                "call" => "call / value produced",
                "imm_borrow" => "shared borrow",
                "mut_borrow" => "exclusive borrow",
                "move" => "move / call",
                "outlive" => "must outlive",
                "shared_mut" => "borrow conflict",
                _ => return None,
            };
            Some(FlowEvent {
                position: decoration.range.start,
                label,
            })
        })
        .collect();

    if let Some(source) = source_for_uri(uri) {
        events.extend(
            await_positions_in_source(&source)
                .into_iter()
                .filter(|position| async_hint(*position, decorations).is_some())
                .map(|position| FlowEvent {
                    position,
                    label: "await / suspend",
                }),
        );
    }

    events.sort_unstable();
    events.dedup();
    let mut nodes: Vec<_> = events
        .into_iter()
        .map(|event| format!("`L{} {}`", event.position.line + 1, event.label))
        .collect();
    if nodes.is_empty() {
        return None;
    }
    if nodes.len() > 8 {
        let tail = nodes.split_off(nodes.len() - 3);
        nodes.truncate(4);
        nodes.push("`…`".into());
        nodes.extend(tail);
    }
    Some(format!("**Flow** · {}", nodes.join(" → ")))
}

pub fn position_for_rustowl(uri: &str, position: Position) -> Position {
    let Some(source) = source_for_uri(uri) else {
        return position;
    };
    let Some(line) = source.lines().nth(position.line as usize) else {
        return position;
    };
    Position {
        line: position.line,
        character: utf16_to_char_column(line, position.character),
    }
}

pub fn range_for_lsp(uri: &str, range: Range) -> Range {
    let Some(source) = source_for_uri(uri) else {
        return range;
    };
    Range {
        start: char_position_to_utf16(&source, range.start),
        end: char_position_to_utf16(&source, range.end),
    }
}

pub fn contains(range: Range, position: Position) -> bool {
    range.start <= position && position < range.end
}

fn token_type(kind: &str) -> Option<u32> {
    match kind {
        "lifetime" | "definitely_live" => Some(0),
        "maybe_initialized" => Some(1),
        "imm_borrow" => Some(2),
        "mut_borrow" => Some(3),
        "move" | "call" => Some(4),
        "outlive" | "shared_mut" => Some(5),
        _ => None,
    }
}

pub fn decoration_presentation(kind: &str) -> Option<DecorationPresentation> {
    match kind {
        "lifetime" => Some(DecorationPresentation {
            title: "Lifetime region",
            summary: "RustOwl is tracing the source region where this value remains available.",
            facts: &[
                (
                    "Scope",
                    "inferred from MIR; this is not a `'static` annotation",
                ),
                (
                    "Meaning",
                    "storage and uses stay connected throughout this region",
                ),
            ],
            inlay_label: None,
            priority: 10,
        }),
        "definitely_live" => Some(DecorationPresentation {
            title: "Definitely live",
            summary: "The value is initialized on every analyzed control-flow path here.",
            facts: &[
                (
                    "Guarantee",
                    "no move or drop has made the value unavailable",
                ),
                (
                    "Access",
                    "its storage contains a value; active borrows may still restrict use",
                ),
            ],
            inlay_label: None,
            priority: 20,
        }),
        "maybe_initialized" => Some(DecorationPresentation {
            title: "Maybe live",
            summary: "The value is available on some analyzed paths, but not guaranteed on all of them.",
            facts: &[
                (
                    "Risk",
                    "a branch may leave the value uninitialized or moved",
                ),
                (
                    "Check",
                    "make every path establish ownership before the next use",
                ),
            ],
            inlay_label: Some("← maybe live · path-dependent"),
            priority: 30,
        }),
        "call" => Some(DecorationPresentation {
            title: "Value-producing call",
            summary: "This function call creates or assigns the selected value.",
            facts: &[
                (
                    "Result",
                    "the returned value becomes the selected binding's next state",
                ),
                (
                    "Contract",
                    "the callee's signature determines returned ownership and lifetimes",
                ),
            ],
            inlay_label: Some("← call result · value created"),
            priority: 40,
        }),
        "imm_borrow" => Some(DecorationPresentation {
            title: "Shared borrow",
            summary: "`&T` grants read-only access without transferring ownership.",
            facts: &[
                ("Ownership", "the source keeps the value"),
                ("Aliasing", "multiple shared readers may coexist"),
                ("Mutation", "blocked until the last shared use ends"),
            ],
            inlay_label: Some("← shared borrow · read-only"),
            priority: 90,
        }),
        "mut_borrow" => Some(DecorationPresentation {
            title: "Exclusive borrow",
            summary: "`&mut T` grants temporary write access without transferring ownership.",
            facts: &[
                (
                    "Aliasing",
                    "this must be the only active reference to the borrowed place",
                ),
                (
                    "Access",
                    "competing reads and writes wait until the borrow ends",
                ),
                (
                    "Duration",
                    "normally ends at its last use under non-lexical lifetimes",
                ),
            ],
            inlay_label: Some("← exclusive borrow · writable"),
            priority: 100,
        }),
        "move" => Some(DecorationPresentation {
            title: "Move or call",
            summary: "RustOwl marks this as an ownership-sensitive move or call site.",
            facts: &[
                (
                    "By value",
                    "a non-Copy argument transfers ownership to the callee",
                ),
                (
                    "By reference",
                    "the owner remains available after the shared or exclusive borrow ends",
                ),
                ("Check", "the callee signature decides which case applies"),
            ],
            inlay_label: Some("← move / call · check ownership"),
            priority: 80,
        }),
        "outlive" => Some(DecorationPresentation {
            title: "Required lifetime",
            summary: "A later dependency requires this value to remain valid here.",
            facts: &[
                (
                    "Constraint",
                    "the source must outlive every reference or use depending on it",
                ),
                (
                    "Typical fix",
                    "extend the source scope or shorten the dependent lifetime",
                ),
            ],
            inlay_label: Some("← lifetime required · must stay live"),
            priority: 105,
        }),
        "shared_mut" => Some(DecorationPresentation {
            title: "Borrow conflict",
            summary: "Shared and exclusive borrows overlap at this point.",
            facts: &[
                (
                    "Rule",
                    "`&mut T` cannot overlap `&T` or another `&mut T` to the same place",
                ),
                (
                    "Typical fix",
                    "end the earlier borrow, shorten its scope, or reorder operations",
                ),
            ],
            inlay_label: Some("← borrow conflict · shared + mutable"),
            priority: 110,
        }),
        _ => None,
    }
}

pub fn decoration_markdown(decoration: &Decoration) -> Option<String> {
    let presentation = decoration_presentation(&decoration.kind)?;
    let mut markdown = format!(
        "### RustOwl · {}\n\n{}\n\n",
        presentation.title, presentation.summary
    );
    for (label, value) in presentation.facts {
        markdown.push_str(&format!("- **{label}** · {value}\n"));
    }
    if let Some(report) = decoration
        .hover_text
        .as_deref()
        .filter(|report| !report.is_empty())
    {
        markdown.push_str(&format!("\n> **RustOwl report** · {report}"));
    }
    Some(markdown)
}

fn split_range(range: Range, source: Option<&str>) -> Vec<(u32, u32, u32)> {
    if range.start.line == range.end.line {
        return vec![(
            range.start.line,
            range.start.character,
            range.end.character.saturating_sub(range.start.character),
        )];
    }

    let Some(source) = source else {
        return Vec::new();
    };
    let lines: Vec<_> = source.lines().collect();
    let mut result = Vec::new();
    for line_number in range.start.line..=range.end.line {
        let Some(line) = lines.get(line_number as usize) else {
            continue;
        };
        let start = if line_number == range.start.line {
            range.start.character
        } else {
            0
        };
        let end = if line_number == range.end.line {
            range.end.character
        } else {
            line.encode_utf16().count() as u32
        };
        result.push((line_number, start, end.saturating_sub(start)));
    }
    result
}

fn source_for_uri(uri: &str) -> Option<String> {
    let path = Url::parse(uri).ok()?.to_file_path().ok()?;
    fs::read_to_string(path).ok()
}

fn char_position_to_utf16(source: &str, position: Position) -> Position {
    let character = source
        .lines()
        .nth(position.line as usize)
        .map(|line| char_to_utf16_column(line, position.character))
        .unwrap_or(position.character);
    Position {
        line: position.line,
        character,
    }
}

fn char_to_utf16_column(line: &str, character: u32) -> u32 {
    line.chars()
        .take(character as usize)
        .map(char::len_utf16)
        .sum::<usize>() as u32
}

fn utf16_to_char_column(line: &str, utf16_column: u32) -> u32 {
    let mut utf16_seen = 0_u32;
    let mut characters = 0_u32;
    for character in line.chars() {
        let next = utf16_seen + character.len_utf16() as u32;
        if next > utf16_column {
            break;
        }
        utf16_seen = next;
        characters += 1;
    }
    characters
}

#[cfg(test)]
mod tests {
    use super::{
        Decoration, Position, Range, async_hint, await_positions_in_source, decoration_markdown,
        decoration_presentation, identifier_positions_in_source, inlay_hints,
        ownership_flow_markdown, semantic_tokens,
    };

    #[test]
    fn encodes_sorted_single_line_tokens() {
        let decorations = vec![
            Decoration {
                kind: "move".into(),
                range: Range {
                    start: Position {
                        line: 3,
                        character: 8,
                    },
                    end: Position {
                        line: 3,
                        character: 11,
                    },
                },
                hover_text: None,
                overlapped: false,
            },
            Decoration {
                kind: "imm_borrow".into(),
                range: Range {
                    start: Position {
                        line: 3,
                        character: 2,
                    },
                    end: Position {
                        line: 3,
                        character: 4,
                    },
                },
                hover_text: None,
                overlapped: false,
            },
        ];
        assert_eq!(
            semantic_tokens("not-a-file-uri", &decorations),
            vec![3, 2, 2, 2, 0, 0, 6, 3, 4, 0]
        );
    }

    #[test]
    fn ignores_overlapped_decorations() {
        let decorations = vec![Decoration {
            kind: "mut_borrow".into(),
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 4,
                },
            },
            hover_text: None,
            overlapped: true,
        }];
        assert!(semantic_tokens("not-a-file-uri", &decorations).is_empty());
    }

    #[test]
    fn creates_inline_helpers_for_ownership_events() {
        let decorations = vec![Decoration {
            kind: "move".into(),
            range: Range {
                start: Position {
                    line: 4,
                    character: 8,
                },
                end: Position {
                    line: 4,
                    character: 13,
                },
            },
            hover_text: Some("variable moved".into()),
            overlapped: false,
        }];
        let hints = inlay_hints(
            "not-a-file-uri",
            Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 10,
                    character: 0,
                },
            },
            &decorations,
        );
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0]["label"], "← move / call · check ownership");
        let tooltip = hints[0]["tooltip"]["value"].as_str().unwrap();
        assert!(tooltip.contains("### RustOwl · Move or call"));
        assert!(tooltip.contains("callee signature decides"));
        assert!(tooltip.contains("> **RustOwl report** · variable moved"));
    }

    #[test]
    fn keeps_the_highest_value_inline_helper_per_line() {
        let range = Range {
            start: Position {
                line: 4,
                character: 8,
            },
            end: Position {
                line: 4,
                character: 13,
            },
        };
        let hints = inlay_hints(
            "not-a-file-uri",
            Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 10,
                    character: 0,
                },
            },
            &[
                Decoration {
                    kind: "call".into(),
                    range,
                    hover_text: None,
                    overlapped: false,
                },
                Decoration {
                    kind: "move".into(),
                    range,
                    hover_text: None,
                    overlapped: false,
                },
            ],
        );

        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0]["label"], "← move / call · check ownership");
    }

    #[test]
    fn finds_visible_identifiers_without_waiting_for_hover() {
        let source = "fn main() {\n    let token = String::from(\"signed token\");\n    consume(token);\n}\n";
        let positions = identifier_positions_in_source(
            source,
            Range {
                start: Position {
                    line: 1,
                    character: 0,
                },
                end: Position {
                    line: 2,
                    character: u32::MAX,
                },
            },
        );

        assert!(positions.contains(&Position {
            line: 1,
            character: 8,
        }));
        assert!(positions.contains(&Position {
            line: 2,
            character: 4,
        }));
        assert!(positions.contains(&Position {
            line: 2,
            character: 12,
        }));
        assert!(!positions.contains(&Position {
            line: 1,
            character: 4,
        }));
    }

    #[test]
    fn explains_a_borrow_that_crosses_an_async_suspension_point() {
        let candidate = async_hint(
            Position {
                line: 4,
                character: 18,
            },
            &[Decoration {
                kind: "imm_borrow".into(),
                range: Range {
                    start: Position {
                        line: 3,
                        character: 8,
                    },
                    end: Position {
                        line: 6,
                        character: 20,
                    },
                },
                hover_text: Some("immutable borrow".into()),
                overlapped: false,
            }],
        )
        .unwrap();

        assert_eq!(candidate.label, "← live across .await · stored in future");
        let tooltip = candidate.tooltip.unwrap();
        assert!(tooltip.contains("State crosses `.await`"));
        assert!(tooltip.contains("future may be suspended"));
        assert!(tooltip.contains("`Send + 'static`"));
    }

    #[test]
    fn builds_a_compact_multi_call_ownership_flow() {
        let point = |line: u32, kind: &str| Decoration {
            kind: kind.into(),
            range: Range {
                start: Position { line, character: 4 },
                end: Position {
                    line,
                    character: 12,
                },
            },
            hover_text: None,
            overlapped: false,
        };
        let flow = ownership_flow_markdown(
            "not-a-file-uri",
            &[point(1, "call"), point(3, "imm_borrow"), point(8, "move")],
        )
        .unwrap();

        assert_eq!(
            flow,
            "**Flow** · `L2 call / value produced` → `L4 shared borrow` → `L9 move / call`"
        );
    }

    #[test]
    fn recognizes_real_await_tokens_only() {
        let positions = await_positions_in_source(
            "let output = future.await;\nlet method = value.awaiting();\n",
        );
        assert_eq!(
            positions,
            vec![Position {
                line: 0,
                character: 20,
            }]
        );
    }

    #[test]
    fn explains_lifetime_regions_even_without_an_inline_hint() {
        let decoration = Decoration {
            kind: "lifetime".into(),
            range: Range {
                start: Position {
                    line: 1,
                    character: 4,
                },
                end: Position {
                    line: 4,
                    character: 9,
                },
            },
            hover_text: Some("lifetime of variable `message`".into()),
            overlapped: false,
        };
        let markdown = decoration_markdown(&decoration).unwrap();
        assert!(markdown.contains("### RustOwl · Lifetime region"));
        assert!(markdown.contains("this is not a `'static` annotation"));
        assert!(markdown.contains("lifetime of variable `message`"));
    }

    #[test]
    fn presents_every_upstream_decoration_kind() {
        let cases = [
            ("lifetime", "Lifetime region", None),
            ("definitely_live", "Definitely live", None),
            (
                "maybe_initialized",
                "Maybe live",
                Some("← maybe live · path-dependent"),
            ),
            (
                "imm_borrow",
                "Shared borrow",
                Some("← shared borrow · read-only"),
            ),
            (
                "mut_borrow",
                "Exclusive borrow",
                Some("← exclusive borrow · writable"),
            ),
            (
                "move",
                "Move or call",
                Some("← move / call · check ownership"),
            ),
            (
                "call",
                "Value-producing call",
                Some("← call result · value created"),
            ),
            (
                "outlive",
                "Required lifetime",
                Some("← lifetime required · must stay live"),
            ),
            (
                "shared_mut",
                "Borrow conflict",
                Some("← borrow conflict · shared + mutable"),
            ),
        ];
        for (kind, title, label) in cases {
            let presentation = decoration_presentation(kind).unwrap();
            assert_eq!(presentation.title, title);
            assert_eq!(presentation.inlay_label, label);
        }
    }
}
