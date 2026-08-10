use std::{collections::HashSet, fs};

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

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Decoration {
    #[serde(rename = "type")]
    pub kind: String,
    pub range: Range,
    #[serde(default)]
    pub hover_text: Option<String>,
    #[serde(default)]
    pub overlapped: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CursorResponse {
    #[allow(dead_code)]
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

pub fn inlay_hints(requested_range: Range, decorations: &[Decoration]) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut hints = Vec::new();
    for decoration in decorations
        .iter()
        .filter(|decoration| !decoration.overlapped)
    {
        let Some(label) = inlay_label(&decoration.kind) else {
            continue;
        };
        let position = decoration.range.end;
        if position < requested_range.start || requested_range.end < position {
            continue;
        }
        let identity = (position, label);
        if !seen.insert(identity) {
            continue;
        }

        let mut hint = serde_json::Map::from_iter([
            ("position".into(), serde_json::to_value(position).unwrap()),
            ("label".into(), Value::String(label.into())),
            ("paddingLeft".into(), Value::Bool(true)),
        ]);
        if let Some(tooltip) = decoration
            .hover_text
            .as_deref()
            .filter(|tooltip| !tooltip.is_empty())
        {
            hint.insert(
                "tooltip".into(),
                serde_json::json!({"kind": "markdown", "value": tooltip}),
            );
        }
        hints.push(Value::Object(hint));
    }
    hints
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

fn inlay_label(kind: &str) -> Option<&'static str> {
    match kind {
        "maybe_initialized" => Some("← maybe initialized"),
        "imm_borrow" => Some("← immutable borrow"),
        "mut_borrow" => Some("← mutable borrow"),
        "move" => Some("← moved"),
        "call" => Some("← call"),
        "outlive" => Some("← must outlive"),
        "shared_mut" => Some("← conflicting borrows"),
        "lifetime" | "definitely_live" => None,
        _ => None,
    }
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
    use super::{Decoration, Position, Range, inlay_hints, semantic_tokens};

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
        assert_eq!(hints[0]["label"], "← moved");
        assert_eq!(hints[0]["tooltip"]["value"], "variable moved");
    }
}
