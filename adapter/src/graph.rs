use std::collections::{BTreeMap, HashMap};
use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use url::Url;

use crate::semantic::{Decoration, range_from_scalar_span};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphQueryResponse {
    #[serde(default)]
    pub is_analyzed: bool,
    #[serde(default)]
    pub status: Option<Value>,
    #[serde(default)]
    pub result: Option<GraphSlice>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GraphSlice {
    pub schema_version: u32,
    #[allow(dead_code)]
    pub revision_id: String,
    #[allow(dead_code)]
    pub revision_sequence: u64,
    #[allow(dead_code)]
    pub source_fingerprint: String,
    #[allow(dead_code)]
    pub requested_document_version: Option<i64>,
    #[allow(dead_code)]
    pub analyzed_document_version: Option<i64>,
    pub fresh: bool,
    pub truncated: bool,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub location: Option<SourceLocation>,
    pub certainty: String,
    #[serde(default)]
    pub properties: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GraphEdge {
    pub kind: String,
    pub source: String,
    pub target: String,
    pub location: Option<SourceLocation>,
    pub order: Option<u32>,
    pub certainty: String,
    #[allow(dead_code)]
    pub explanation: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SourceLocation {
    pub file: String,
    pub span: GraphSpan,
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct GraphSpan {
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Copy)]
struct InsightPresentation {
    kind: &'static str,
    title: &'static str,
    summary: &'static str,
}

pub fn graph_decorations(uri: &str, slice: &GraphSlice) -> Result<Vec<Decoration>> {
    let (source_path, source) =
        source_for_uri(uri).context("could not read source for graph locations")?;
    let nodes: HashMap<_, _> = slice
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let mut result = Vec::new();
    for node in &slice.nodes {
        let Some(presentation) = insight_presentation(node) else {
            continue;
        };
        let Some(location) = node.location.as_ref() else {
            continue;
        };
        if !same_source_file(&source_path, &location.file) {
            continue;
        }
        let Some(range) = range_from_scalar_span(&source, location.span.start, location.span.end)
        else {
            continue;
        };
        if signature_artifact(&source, range, presentation.kind) {
            continue;
        }
        let markdown = insight_markdown(node, presentation, slice, &nodes);
        result.push(Decoration {
            kind: presentation.kind.to_owned(),
            range,
            hover_text: Some(markdown),
            overlapped: false,
        });
    }
    for edge in &slice.edges {
        let Some(presentation) = edge_presentation(edge) else {
            continue;
        };
        let Some(location) = edge.location.as_ref() else {
            continue;
        };
        if !same_source_file(&source_path, &location.file) {
            continue;
        }
        let Some(range) = range_from_scalar_span(&source, location.span.start, location.span.end)
        else {
            continue;
        };
        if signature_artifact(&source, range, presentation.kind) {
            continue;
        }
        let Some(source_node) = nodes.get(edge.source.as_str()) else {
            continue;
        };
        let Some(target_node) = nodes.get(edge.target.as_str()) else {
            continue;
        };
        if result
            .iter()
            .any(|decoration| decoration.kind == presentation.kind && decoration.range == range)
        {
            continue;
        }
        result.push(Decoration {
            kind: presentation.kind.to_owned(),
            range,
            hover_text: Some(edge_markdown(
                edge,
                presentation,
                source_node,
                target_node,
                slice,
                &nodes,
                &source,
                range,
            )),
            overlapped: false,
        });
    }
    result.sort_by_key(|decoration| (decoration.range, decoration.kind.clone()));
    result.dedup();
    Ok(result)
}

fn edge_presentation(edge: &GraphEdge) -> Option<InsightPresentation> {
    match edge.kind.as_str() {
        "calls" => Some(InsightPresentation {
            kind: "call",
            title: "Call target found",
            summary: "RustOwl connected this call to the function that will receive it.",
        }),
        "borrows_shared" => Some(InsightPresentation {
            kind: "imm_borrow",
            title: "Shared borrow",
            summary: "This creates a read-only view; the original value keeps ownership.",
        }),
        "borrows_mut" => Some(InsightPresentation {
            kind: "mut_borrow",
            title: "Exclusive borrow",
            summary: "This creates a writable view that must be the only active access.",
        }),
        "moves_to" => Some(InsightPresentation {
            kind: "move",
            title: "Ownership moved",
            summary: "This value is transferred rather than copied.",
        }),
        "copies_to" => Some(InsightPresentation {
            kind: "copy",
            title: "Value copied",
            summary: "This value is duplicated, so the original remains usable.",
        }),
        "mutates_through" => Some(InsightPresentation {
            kind: "mutation",
            title: "Value updated",
            summary: "This operation changes the value in place.",
        }),
        "returns_as" => Some(InsightPresentation {
            kind: "return",
            title: "Call result",
            summary: "The result of this call is stored here.",
        }),
        "drops_at" | "cancellation_drops_at" => Some(InsightPresentation {
            kind: "drop",
            title: "Value dropped",
            summary: "This value is cleaned up and cannot be used afterward.",
        }),
        "live_across_await" => Some(InsightPresentation {
            kind: "async_suspend",
            title: "Saved across await",
            summary: "This value is stored inside the future while execution is paused.",
        }),
        "blocks_send" => Some(InsightPresentation {
            kind: "async_suspend",
            title: "Future is not Send",
            summary: "This retained value is the compiler-proven reason the generated future cannot move between threads.",
        }),
        "blocks_static" => Some(InsightPresentation {
            kind: "async_suspend",
            title: "Future borrows from its caller",
            summary: "This retained reference carries a non-'static region into the generated future.",
        }),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn edge_markdown(
    edge: &GraphEdge,
    presentation: InsightPresentation,
    source: &GraphNode,
    target: &GraphNode,
    slice: &GraphSlice,
    nodes: &HashMap<&str, &GraphNode>,
    document: &str,
    range: crate::semantic::Range,
) -> String {
    let graph_source_label = developer_label(source, slice, nodes);
    let graph_target_label = developer_label(target, slice, nodes);
    let (source_label, target_label) = source_edge_labels(
        document,
        range,
        &edge.kind,
        source,
        target,
        &graph_source_label,
        &graph_target_label,
    );
    // Assignment edges encode the relation "reference borrows owner" as
    // reference -> owner. Present ownership flow in the direction developers
    // read it: owner -> resulting reference. Borrow-region event edges retain
    // their native place -> event direction.
    let reverse_borrow_assignment = borrow_assignment_edge(&edge.kind, source, target);
    let (source_label, target_label) = if reverse_borrow_assignment {
        (target_label, source_label)
    } else {
        (source_label, target_label)
    };
    let expression = source_expression(document, range);
    let consequence = human_edge_consequence(
        &edge.kind,
        &source_label,
        &target_label,
        expression.as_deref(),
    );
    // Edge hovers compete with Zed's own signature/documentation hover. Keep
    // the developer-facing card to one explanation, one consequence, and one
    // flow instead of repeating the same event in a summary paragraph.
    let mut markdown = format!("### RustOwl · {}\n\n{}", presentation.title, consequence);
    if let Some((label, guidance)) = human_edge_guidance(&edge.kind, &source_label) {
        markdown.push_str(&format!("\n\n**{label}** · {guidance}"));
    }
    markdown.push_str(&format!(
        "\n\n**Flow** · {}",
        human_edge_flow(
            &edge.kind,
            &source_label,
            &target_label,
            expression.as_deref(),
        )
    ));
    append_human_provenance(&mut markdown, slice, &edge.certainty);
    markdown
}

fn source_expression(document: &str, range: crate::semantic::Range) -> Option<String> {
    let line = document.lines().nth(range.start.line as usize)?;
    let code = line.split("//").next()?.trim();
    let expression = let_assignment(code)
        .map(|(_, expression)| expression)
        .unwrap_or(code)
        .trim()
        .trim_end_matches(';')
        .trim();
    (!expression.is_empty()).then(|| expression.to_owned())
}

fn human_edge_consequence(
    kind: &str,
    source: &str,
    target: &str,
    expression: Option<&str>,
) -> String {
    let expression = expression.filter(|value| value.len() <= 100);
    match kind {
        "calls" => expression
            .map(|call| format!("`{call}` enters `{target}`."))
            .unwrap_or_else(|| format!("This call enters `{target}`.")),
        "borrows_shared" => format!(
            "`{target}` is a read-only view of `{source}` and does not take ownership. `{source}` remains the owner."
        ),
        "borrows_mut" => format!(
            "`{target}` can modify `{source}`. While this borrow is active, no other code may read or write `{source}`."
        ),
        "moves_to" => expression
            .map(|call| match call_name(call) {
                Some(callee) => format!(
                    "Calling `{call}` moves `{source}` into the `{target}` parameter of `{callee}`. `{source}` cannot be used again after this line."
                ),
                None => format!(
                    "`{call}` takes ownership of `{source}`. Inside the called function it becomes parameter `{target}`."
                ),
            })
            .unwrap_or_else(|| {
                format!("`{source}` transfers ownership to `{target}` rather than being copied.")
            }),
        "copies_to" => format!(
            "`{target}` receives a copy of `{source}`; both values remain usable."
        ),
        "mutates_through" => format!("This operation changes `{target}` in place."),
        "returns_as" => expression
            .map(|call| format!("The result of `{call}` is stored as `{target}`."))
            .unwrap_or_else(|| format!("The returned value is stored as `{target}`.")),
        "drops_at" | "cancellation_drops_at" => {
            format!("`{source}` is cleaned up here and is no longer available.")
        }
        "live_across_await" => format!(
            "`{source}` is needed after this `.await`, so the generated future keeps it while the task is paused."
        ),
        "blocks_send" => format!(
            "Retaining `{source}` makes this future non-`Send`, so it cannot be moved to another thread."
        ),
        "blocks_static" => format!(
            "Retaining `{source}` keeps a caller-owned borrow inside this future."
        ),
        _ => format!("Ownership state flows from `{source}` to `{target}`."),
    }
}

fn human_edge_guidance(kind: &str, source: &str) -> Option<(String, String)> {
    match kind {
        "borrows_shared" => Some((
            "Why it matters".to_owned(),
            format!(
                "`{source}` cannot be mutably borrowed until the final use of the shared reference."
            ),
        )),
        "borrows_mut" => Some((
            "Why it matters".to_owned(),
            "the exclusive access ends after the reference's final use—or when a containing future is dropped."
                .to_owned(),
        )),
        "moves_to" => Some((
            format!("Keep using `{source}`"),
            format!(
                "pass `&{source}` if the function only needs to read it, or use `{source}.clone()` when a deliberate duplicate is appropriate."
            ),
        )),
        "returns_as" => Some((
            "Why it matters".to_owned(),
            "if the result is a reference, its source must stay valid for as long as that reference is used."
                .to_owned(),
        )),
        "live_across_await" => Some((
            "Why it matters".to_owned(),
            "saved values affect the future's size, cancellation cleanup, and whether it can satisfy `Send` or `'static`."
                .to_owned(),
        )),
        "blocks_send" => Some((
            "Why it matters".to_owned(),
            "multi-thread executors such as `tokio::spawn` require the spawned future to be `Send`."
                .to_owned(),
        )),
        "blocks_static" => Some((
            "Why it matters".to_owned(),
            "detached task spawners usually require `'static`; await the task locally or move owned data into it."
                .to_owned(),
        )),
        _ => None,
    }
}

fn call_name(expression: &str) -> Option<&str> {
    let prefix = expression.split_once('(')?.0.trim();
    let name = prefix.rsplit("::").next()?.trim();
    valid_identifier(name).then_some(name)
}

fn human_edge_flow(kind: &str, source: &str, target: &str, expression: Option<&str>) -> String {
    match kind {
        "moves_to" => expression
            .map(|call| match call_name(call) {
                Some(callee) => {
                    format!("caller: `{source}` → `{callee}` → callee: `{target}`")
                }
                None => format!("`{source}` → `{call}` → parameter `{target}`"),
            })
            .unwrap_or_else(|| format!("`{source}` → `{target}`")),
        "returns_as" => expression
            .map(|call| format!("`{call}` → `{target}`"))
            .unwrap_or_else(|| format!("return → `{target}`")),
        "borrows_shared" => format!("`{source}` ── shared view ─→ `{target}`"),
        "borrows_mut" => format!("`{source}` ── exclusive view ─→ `{target}`"),
        "copies_to" => format!("`{source}` ── copy ─→ `{target}`"),
        "mutates_through" => format!("operation ── writes ─→ `{target}`"),
        "live_across_await" => format!("`{source}` → future storage → resume"),
        "blocks_send" => format!("`{source}` → retained field → future is not `Send`"),
        "blocks_static" => format!("`{source}` → retained borrow → future is not `'static`"),
        _ => format!("`{source}` → `{target}`"),
    }
}

#[allow(clippy::too_many_arguments)]
fn source_edge_labels(
    document: &str,
    range: crate::semantic::Range,
    kind: &str,
    source: &GraphNode,
    target: &GraphNode,
    graph_source: &str,
    graph_target: &str,
) -> (String, String) {
    let reverse_borrow_assignment = borrow_assignment_edge(kind, source, target);
    let mut readable_source = if internal_place_label(&source.label) {
        if reverse_borrow_assignment {
            "the resulting reference".to_owned()
        } else {
            source_role(kind).to_owned()
        }
    } else {
        learner_place_label(graph_source)
    };
    let mut readable_target = if internal_place_label(&target.label) {
        if reverse_borrow_assignment {
            "the borrowed value".to_owned()
        } else {
            target_role(kind).to_owned()
        }
    } else {
        learner_place_label(graph_target)
    };
    let Some(line) = document.lines().nth(range.start.line as usize) else {
        return (readable_source, readable_target);
    };
    if let Some((binding, expression)) = let_assignment(line) {
        if reverse_borrow_assignment {
            if internal_place_label(&source.label) {
                readable_source = binding;
            }
            if internal_place_label(&target.label)
                && let Some(subject) = expression_subject(expression, kind)
            {
                readable_target = subject;
            }
        } else {
            if internal_place_label(&target.label) {
                readable_target = binding;
            }
            if internal_place_label(&source.label)
                && let Some(subject) = expression_subject(expression, kind)
            {
                readable_source = subject;
            }
        }
    } else if internal_place_label(&source.label)
        && let Some(subject) = expression_subject(line, kind)
    {
        readable_source = subject;
    }
    (readable_source, readable_target)
}

fn borrow_assignment_edge(kind: &str, source: &GraphNode, target: &GraphNode) -> bool {
    matches!(kind, "borrows_shared" | "borrows_mut")
        && matches!(source.kind.as_str(), "place" | "binding")
        && matches!(target.kind.as_str(), "place" | "binding")
}

fn let_assignment(line: &str) -> Option<(String, &str)> {
    let line = line.split("//").next()?.trim();
    let rest = line.strip_prefix("let ")?.trim_start();
    let rest = rest.strip_prefix("mut ").unwrap_or(rest).trim_start();
    let (left, right) = rest.split_once('=')?;
    let binding = left
        .split(':')
        .next()?
        .trim()
        .trim_start_matches("ref ")
        .trim_start_matches("mut ")
        .trim();
    valid_identifier(binding).then(|| (binding.to_owned(), right.trim()))
}

fn expression_subject(expression: &str, kind: &str) -> Option<String> {
    let expression = expression.trim().trim_end_matches(';').trim();
    let expression = expression
        .strip_prefix('&')
        .unwrap_or(expression)
        .trim_start();
    let expression = expression
        .strip_prefix("mut ")
        .unwrap_or(expression)
        .trim_start_matches('*')
        .trim_start();
    if matches!(kind, "moves_to" | "copies_to" | "passes_to")
        && let Some((_, arguments)) = expression.split_once('(')
        && let Some(argument) = first_identifier(arguments)
    {
        return Some(argument);
    }
    first_identifier(expression)
}

fn first_identifier(expression: &str) -> Option<String> {
    let bytes = expression.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        while start < bytes.len() && !(bytes[start].is_ascii_alphabetic() || bytes[start] == b'_') {
            start += 1;
        }
        let mut end = start;
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
            end += 1;
        }
        if end == start {
            return None;
        }
        let candidate = &expression[start..end];
        if !matches!(candidate, "let" | "mut" | "ref" | "async" | "await" | "std")
            && candidate
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_lowercase)
        {
            return Some(candidate.to_owned());
        }
        start = end;
    }
    None
}

fn valid_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn source_role(kind: &str) -> &'static str {
    match kind {
        "borrows_shared" | "borrows_mut" => "the borrowed value",
        "moves_to" => "the moved value",
        "copies_to" => "the copied value",
        "live_across_await" => "the retained value",
        _ => "the source value",
    }
}

fn target_role(kind: &str) -> &'static str {
    match kind {
        "borrows_shared" | "borrows_mut" => "the resulting reference",
        "moves_to" | "copies_to" => "the destination",
        "live_across_await" => "the suspension point",
        "drops_at" | "cancellation_drops_at" => "the cleanup point",
        _ => "the destination",
    }
}

fn learner_place_label(label: &str) -> String {
    let label = label.trim();
    let Some(inner) = label
        .strip_prefix("*(")
        .and_then(|inner| inner.strip_suffix(')'))
        .map(str::trim)
    else {
        return label.to_owned();
    };
    if inner.is_empty() || internal_place_label(inner) {
        label.to_owned()
    } else {
        inner.to_owned()
    }
}

fn signature_artifact(source: &str, range: crate::semantic::Range, kind: &str) -> bool {
    let Some(line) = source.lines().nth(range.start.line as usize) else {
        return false;
    };
    let line = line.trim_start();
    let source_tokens: Vec<_> = line
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .filter(|token| !token.is_empty())
        .collect();
    if kind == "branch_join"
        && !source_tokens
            .iter()
            .any(|token| matches!(*token, "if" | "else" | "match"))
    {
        return true;
    }
    if kind == "loop_summary"
        && !source_tokens
            .iter()
            .any(|token| matches!(*token, "loop" | "while" | "for"))
    {
        return true;
    }
    let signature = line.starts_with("fn ")
        || line.starts_with("async fn ")
        || line.starts_with("pub fn ")
        || line.starts_with("pub async fn ");
    let structural_liveness = matches!(kind, "definitely_live" | "maybe_initialized" | "lifetime");
    (signature
        && (structural_liveness
            || matches!(
                kind,
                "move" | "copy" | "mutation" | "call" | "drop" | "return"
            )))
        || (structural_liveness && line.starts_with('}'))
}

fn developer_label(
    node: &GraphNode,
    slice: &GraphSlice,
    nodes: &HashMap<&str, &GraphNode>,
) -> String {
    if !internal_place_label(&node.label) {
        return node.label.clone();
    }
    let traversable = |kind: &str| {
        matches!(
            kind,
            "owns"
                | "projects_to"
                | "aliases"
                | "reborrows_from"
                | "borrows_shared"
                | "borrows_mut"
                | "copies_to"
                | "moves_to"
                | "passes_to"
                | "returns_as"
                | "mutates_through"
        )
    };
    let mut frontier = vec![node.id.as_str()];
    let mut seen = std::collections::HashSet::from([node.id.as_str()]);
    for _ in 0..=3 {
        let mut labels = Vec::new();
        for id in &frontier {
            if let Some(candidate) = nodes.get(id)
                && !internal_place_label(&candidate.label)
                && (candidate.kind == "place"
                    || candidate.kind == "binding"
                        && candidate
                            .properties
                            .get("user_binding")
                            .and_then(Value::as_bool)
                            .unwrap_or(false))
            {
                labels.push(candidate.label.clone());
            }
        }
        labels.sort();
        labels.dedup();
        if !labels.is_empty() {
            return labels.into_iter().next().unwrap();
        }
        let mut next = Vec::new();
        for edge in slice.edges.iter().filter(|edge| traversable(&edge.kind)) {
            let adjacent = if frontier.contains(&edge.source.as_str()) {
                Some(edge.target.as_str())
            } else if frontier.contains(&edge.target.as_str()) {
                Some(edge.source.as_str())
            } else {
                None
            };
            if let Some(adjacent) = adjacent
                && seen.insert(adjacent)
            {
                next.push(adjacent);
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    "an internal temporary".to_owned()
}

fn internal_place_label(label: &str) -> bool {
    label.starts_with('_') || label.contains("(_")
}

fn insight_presentation(node: &GraphNode) -> Option<InsightPresentation> {
    let presentation = match node.kind.as_str() {
        "borrow_event" => {
            if node
                .properties
                .get("mutable")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                InsightPresentation {
                    kind: "mut_borrow",
                    title: "Exclusive borrow",
                    summary: "This reference can write to the value and must be the only active access.",
                }
            } else {
                InsightPresentation {
                    kind: "imm_borrow",
                    title: "Shared borrow",
                    summary: "This is a read-only view; the original value keeps ownership.",
                }
            }
        }
        "liveness_event" => match node.properties.get("class").and_then(Value::as_str) {
            Some("definitely_live") => InsightPresentation {
                kind: "definitely_live",
                title: "Definitely live",
                summary: "The compiler sees an initialized value on every analyzed path here.",
            },
            Some("maybe_initialized") => InsightPresentation {
                kind: "maybe_initialized",
                title: "Maybe live",
                summary: "The value is initialized on some incoming paths, but not all of them.",
            },
            Some("must_live") => InsightPresentation {
                kind: "outlive",
                title: "Required lifetime",
                summary: "A dependent reference or use requires this place to remain valid here.",
            },
            _ => InsightPresentation {
                kind: "lifetime",
                title: "Lifetime region",
                summary: "Compiler liveness keeps this place available through the highlighted region.",
            },
        },
        "call_site" => InsightPresentation {
            kind: "call",
            title: "Ownership-sensitive call",
            summary: "This call may borrow, copy, or take ownership of its arguments.",
        },
        "move_event" => InsightPresentation {
            kind: "move",
            title: "Ownership moved",
            summary: "This value is transferred rather than copied.",
        },
        "mutation_event" => InsightPresentation {
            kind: "mutation",
            title: "Value updated",
            summary: "This operation changes the value in place.",
        },
        "drop_event" => InsightPresentation {
            kind: "drop",
            title: "Value dropped",
            summary: "The value's destructor or storage cleanup runs on this path.",
        },
        "return_event" => InsightPresentation {
            kind: "return",
            title: "Value returned",
            summary: "This value leaves the function as its result.",
        },
        "suspension_point" => InsightPresentation {
            kind: "async_suspend",
            title: "Async suspension",
            summary: "The generated future can suspend here while retaining compiler-live state.",
        },
        // Async constraint nodes are intentionally not rendered on their own.
        // The suspension-point hover folds every exact future field into one
        // human explanation, avoiding duplicate Zed popups and raw field types.
        "async_constraint" => return None,
        "branch_join" => InsightPresentation {
            kind: "branch_join",
            title: "Control-flow join",
            summary: "Ownership states from multiple predecessor paths meet at this point.",
        },
        "loop_summary" => InsightPresentation {
            kind: "loop_summary",
            title: "Loop-carried ownership",
            summary: "This summary represents ownership state carried around a control-flow back edge.",
        },
        "diagnostic" => InsightPresentation {
            kind: "diagnostic",
            title: "Analysis boundary",
            summary: "RustOwl deliberately widens certainty at unsupported or unresolved compiler evidence.",
        },
        _ => return None,
    };
    Some(presentation)
}

fn insight_markdown(
    node: &GraphNode,
    presentation: InsightPresentation,
    slice: &GraphSlice,
    nodes: &HashMap<&str, &GraphNode>,
) -> String {
    if node.kind == "suspension_point" {
        return async_suspension_markdown(node, slice, nodes);
    }
    let subject = insight_subject(node, slice, nodes);
    let mut markdown = format!(
        "### RustOwl · {}\n\n{}\n\n{}",
        presentation.title,
        presentation.summary,
        insight_consequence(node, &subject),
    );
    if let Some(why) = human_node_why(node, &subject) {
        markdown.push_str(&format!("\n\n**Why it matters** · {why}"));
    }

    let mut related_places = Vec::new();
    let mut capabilities = Vec::new();
    let mut related_edges: Vec<_> = slice
        .edges
        .iter()
        .filter(|edge| edge.source == node.id || edge.target == node.id)
        .collect();
    related_edges.sort_by_key(|edge| (edge.order, edge.kind.as_str(), edge.source.as_str()));
    for edge in related_edges {
        let other_id = if edge.source == node.id {
            edge.target.as_str()
        } else {
            edge.source.as_str()
        };
        if let Some(other) = nodes.get(other_id) {
            if matches!(other.kind.as_str(), "place" | "binding") {
                let related = learner_place_label(&developer_label(other, slice, nodes));
                if related != subject && related != "an internal temporary" {
                    related_places.push(format!("`{related}`"));
                }
            }
            if other.kind == "capability_snapshot"
                && let Some(capability) = other.properties.get("capability").and_then(Value::as_str)
            {
                capabilities.push(capability.replace('_', " "));
            }
        }
    }
    related_places.sort();
    related_places.dedup();
    capabilities.sort();
    capabilities.dedup();
    if !related_places.is_empty() {
        markdown.push_str(&format!(
            "\n\n**Related value{}** · {}",
            if related_places.len() == 1 { "" } else { "s" },
            related_places.join(", ")
        ));
    }
    if !capabilities.is_empty() {
        markdown.push_str(&format!(
            "\n\n**Access now** · {}",
            capabilities.join(" · ")
        ));
    }
    if node.kind == "call_site" && node.certainty == "unresolved" {
        markdown.push_str(
            "\n\n**Limit** · RustOwl can see how the arguments are used here, but it could not identify the called function.",
        );
    }
    append_human_provenance(&mut markdown, slice, &node.certainty);
    markdown
}

fn human_node_why(node: &GraphNode, subject: &str) -> Option<String> {
    match node.kind.as_str() {
        "borrow_event"
            if node
                .properties
                .get("mutable")
                .and_then(Value::as_bool)
                .unwrap_or(false) =>
        {
            Some("other reads and writes must wait until this reference's final use.".to_owned())
        }
        "borrow_event" => Some(format!(
            "`{subject}` cannot be mutably borrowed until the final shared use."
        )),
        "move_event" => Some(format!(
            "`{subject}` cannot be used again unless it is assigned a new value. Borrow it or clone deliberately if the caller still needs it."
        )),
        "drop_event" => Some(
            "destructors run here; any owned resources are released on this path.".to_owned(),
        ),
        "return_event" => Some(
            "the caller now owns the returned value—or must uphold the returned reference's lifetime."
                .to_owned(),
        ),
        "liveness_event" => Some(
            "this is an availability fact, not a guarantee that the value will be used.".to_owned(),
        ),
        _ => None,
    }
}

#[derive(Clone)]
struct RetainedFutureField {
    index: u64,
    name: Option<String>,
    type_name: String,
    send: String,
    static_lifetime: String,
    ignored_for_traits: bool,
}

fn async_suspension_markdown(
    suspension: &GraphNode,
    slice: &GraphSlice,
    nodes: &HashMap<&str, &GraphNode>,
) -> String {
    let mut fields: Vec<_> = nodes
        .values()
        .filter(|candidate| {
            candidate.kind == "future_field"
                && slice.edges.iter().any(|edge| {
                    edge.kind == "live_across_await"
                        && edge.source == candidate.id
                        && edge.target == suspension.id
                })
        })
        .map(|candidate| RetainedFutureField {
            index: candidate
                .properties
                .get("field_index")
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX),
            name: candidate
                .properties
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| {
                    !name.is_empty() && !name.starts_with('_') && !name.starts_with("__awaitee")
                })
                .map(str::to_owned),
            type_name: friendly_type(
                candidate
                    .properties
                    .get("type_name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
            ),
            send: candidate
                .properties
                .get("send")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
            static_lifetime: candidate
                .properties
                .get("static_lifetime")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
            ignored_for_traits: candidate
                .properties
                .get("ignore_for_traits")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
        .collect();
    fields.sort_by_key(|field| (field.name.is_none(), field.index));

    let named: Vec<_> = fields.iter().filter(|field| field.name.is_some()).collect();
    let unnamed_count = fields.len().saturating_sub(named.len());
    let title = match named.as_slice() {
        [field] => format!(
            "### RustOwl · Await holds `{}`",
            field.name.as_deref().unwrap_or("value")
        ),
        [] => "### RustOwl · Await boundary".to_owned(),
        many => format!("### RustOwl · Await holds {} source values", many.len()),
    };

    let mut markdown = title;
    if fields.is_empty() {
        markdown.push_str(
            "\n\nExecution can pause here, but RustOwl could not recover this suspension point's saved fields. No `Send` or `'static` claim is made for this analysis.\n\n**Why you care**\n\n- **Cancellation** · dropping the future stops execution after this point and runs its compiler-generated cleanup.",
        );
        markdown.push_str(
            "\n\nRustOwl found the pause point, but rustc did not expose the saved fields for this future. No `Send` or `'static` conclusion is shown.",
        );
        append_human_provenance(&mut markdown, slice, &suspension.certainty);
        return markdown;
    }

    if let [field] = named.as_slice() {
        markdown.push_str(&format!(
            "\n\nExecution can pause here. The generated future stores `{}: {}` because it is needed after execution resumes.",
            field.name.as_deref().unwrap_or("value"),
            field.type_name
        ));
    } else if !named.is_empty() {
        let names = named
            .iter()
            .filter_map(|field| field.name.as_deref())
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ");
        markdown.push_str(&format!(
            "\n\nExecution can pause here. The generated future stores {names} because they are needed after execution resumes."
        ));
    } else {
        markdown.push_str(
            "\n\nExecution can pause here. The generated future stores compiler-generated state needed to resume this operation.",
        );
    }

    markdown.push_str("\n\n**Why you care**");
    if let Some(field) = fields
        .iter()
        .find(|field| field.type_name.starts_with("&mut "))
    {
        markdown.push_str(&format!(
            "\n\n- **Exclusive borrow** · `{}` is a writable reference saved across this `.await`. The value it points to remains exclusively borrowed until the future passes its last use or is dropped.",
            field.name.as_deref().unwrap_or("the referenced value")
        ));
    } else if let Some(field) = fields.iter().find(|field| field.type_name.starts_with('&')) {
        markdown.push_str(&format!(
            "\n\n- **Shared borrow** · `{}` is a read-only reference saved across this `.await`. The value it points to cannot be mutated until the future passes its last use or is dropped.",
            field.name.as_deref().unwrap_or("the referenced value")
        ));
    } else {
        markdown.push_str(
            "\n\n- **Stored state** · these values become fields of the generated future while the task is suspended.",
        );
    }

    let trait_fields: Vec<_> = fields
        .iter()
        .filter(|field| !field.ignored_for_traits)
        .collect();
    markdown.push_str(&format!(
        "\n- **`Send`** · {}",
        async_status_explanation(&trait_fields, |field| &field.send, "`Send`", true)
    ));
    markdown.push_str(&format!(
        "\n- **`'static`** · {}",
        async_status_explanation(
            &trait_fields,
            |field| &field.static_lifetime,
            "`'static`",
            false,
        )
    ));
    markdown.push_str(
        "\n- **Cancellation** · dropping the future releases retained borrows and drops owned retained state; code after this `.await` does not run.",
    );

    if !named.is_empty() || unnamed_count > 0 {
        markdown.push_str("\n\n**Retained future state**");
        for field in named.iter().take(6) {
            markdown.push_str(&format!(
                "\n\n- `{}: {}` · `Send`: {} · `'static`: {}",
                field.name.as_deref().unwrap_or("value"),
                field.type_name,
                human_status(&field.send),
                human_status(&field.static_lifetime),
            ));
        }
        if named.len() > 6 {
            markdown.push_str(&format!("\n- …and {} more named fields", named.len() - 6));
        }
        if unnamed_count > 0 {
            markdown.push_str(&format!(
                "\n- {} compiler-generated field{} included in the checks (internal names hidden)",
                unnamed_count,
                if unnamed_count == 1 { "" } else { "s" }
            ));
        }
    }

    append_human_provenance(&mut markdown, slice, &suspension.certainty);
    markdown
}

fn async_status_explanation(
    fields: &[&RetainedFutureField],
    status: impl Fn(&RetainedFutureField) -> &String,
    requirement: &str,
    send: bool,
) -> String {
    if let Some(field) = fields.iter().find(|field| status(field) == "rejected") {
        let subject = field
            .name
            .as_deref()
            .map(|name| format!("`{name}: {}`", field.type_name))
            .unwrap_or_else(|| format!("`{}`", field.type_name));
        if send {
            return format!(
                "**no** — retained {subject} is not {requirement}; a multi-thread task spawner will reject this future."
            );
        }
        return format!(
            "**no** — retained {subject} borrows outside the future; detached spawns requiring {requirement} will reject it."
        );
    }
    if fields.is_empty() || fields.iter().any(|field| status(field) == "unknown") {
        return format!(
            "**not proven** — the compiler evidence contains a generic or unresolved field, so RustOwl will not claim {requirement}."
        );
    }
    if send {
        "**yes** — this future may be moved between threads.".to_owned()
    } else {
        "**yes** — this future does not retain caller-owned references.".to_owned()
    }
}

fn human_status(status: &str) -> &'static str {
    match status {
        "proven" => "yes",
        "rejected" => "no",
        _ => "not proven",
    }
}

fn friendly_type(type_name: &str) -> String {
    type_name
        .replace("alloc::string::String", "String")
        .replace("std::string::String", "String")
        .replace("alloc::rc::Rc", "Rc")
        .replace("alloc::sync::Arc", "Arc")
        .replace("core::", "")
        .replace("std::", "")
}

fn insight_subject(
    node: &GraphNode,
    slice: &GraphSlice,
    nodes: &HashMap<&str, &GraphNode>,
) -> String {
    if matches!(
        node.kind.as_str(),
        "borrow_event"
            | "liveness_event"
            | "move_event"
            | "mutation_event"
            | "drop_event"
            | "return_event"
            | "suspension_point"
    ) {
        let mut candidates: Vec<_> = slice
            .edges
            .iter()
            .filter_map(|edge| {
                let other_id = if edge.source == node.id {
                    edge.target.as_str()
                } else if edge.target == node.id {
                    edge.source.as_str()
                } else {
                    return None;
                };
                let other = nodes.get(other_id)?;
                if !matches!(other.kind.as_str(), "place" | "binding") {
                    return None;
                }
                let user_binding = other
                    .properties
                    .get("user_binding")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let label = learner_place_label(&developer_label(other, slice, nodes));
                Some((!user_binding, internal_place_label(&other.label), label))
            })
            .collect();
        candidates.sort();
        candidates.dedup();
        if let Some((_, _, label)) = candidates.into_iter().next() {
            return label;
        }
    }
    learner_place_label(&developer_label(node, slice, nodes))
}

fn insight_consequence(node: &GraphNode, subject: &str) -> String {
    match node.kind.as_str() {
        "borrow_event"
            if node
                .properties
                .get("mutable")
                .and_then(Value::as_bool)
                .unwrap_or(false) =>
        {
            format!(
                "`{subject}` has exclusive access for this borrow. Other code must wait before reading or writing the same value."
            )
        }
        "borrow_event" => format!(
            "`{subject}` may be read through shared references. Mutation becomes available after the final shared use."
        ),
        "move_event" => format!(
            "Ownership leaves `{subject}` here. Using the old source again requires reinitializing it first."
        ),
        "mutation_event" => format!(
            "`{subject}` receives a new value or state here, so active borrows must permit mutation."
        ),
        "drop_event" => format!(
            "`{subject}` is cleaned up here and cannot be used afterward unless it is initialized again."
        ),
        "return_event" => format!(
            "`{subject}` leaves this scope as a result; ownership or a reference continues at the caller."
        ),
        "suspension_point" => format!(
            "`{subject}` can be stored in the generated future while execution is paused. Its validity and cancellation cleanup both matter."
        ),
        "async_constraint" => {
            let type_name = node
                .properties
                .get("type_name")
                .and_then(Value::as_str)
                .unwrap_or(subject);
            let send = node
                .properties
                .get("send")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let static_lifetime = node
                .properties
                .get("static_lifetime")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            format!(
                "`{type_name}` is retained across this `.await`. Send is `{send}` and `'static` compatibility is `{static_lifetime}`; rejected evidence identifies the exact field that prevents detached-task compatibility."
            )
        }
        "liveness_event" => format!(
            "The compiler still considers `{subject}` available at this point on the indicated control-flow paths."
        ),
        "call_site" => format!(
            "Arguments flowing through `{subject}` may be borrowed, copied, or moved according to the compiler evidence below."
        ),
        "diagnostic" => "RustOwl cannot prove a stronger result at this boundary. Treat the surrounding ownership flow as conservative rather than as a compiler error.".to_owned(),
        _ => format!("This point changes or summarizes the ownership state of `{subject}`."),
    }
}

fn append_human_provenance(markdown: &mut String, slice: &GraphSlice, certainty: &str) {
    let status = if !slice.fresh {
        "Analysis is updating; this result is from the previous saved state."
    } else {
        match certainty {
            "compiler_proven" => "Verified by rustc against the current source.",
            "source_resolved" => "Matched to the current source.",
            "conservative" => "Conservative: this may happen on some control-flow paths.",
            _ => "Unresolved: RustOwl will not make a stronger claim here.",
        }
    };
    markdown.push_str(&format!("\n\n---\n{status}"));
    if slice.truncated {
        markdown.push_str(" More connected context is available in the RustOwl graph tools.");
    }
}

fn source_for_uri(uri: &str) -> Option<(std::path::PathBuf, String)> {
    let path = Url::parse(uri).ok()?.to_file_path().ok()?;
    let source = fs::read_to_string(&path).ok()?;
    Some((path, source))
}

fn same_source_file(source_path: &Path, graph_file: &str) -> bool {
    let graph_path = Path::new(graph_file);
    if source_path == graph_path {
        return true;
    }
    match (source_path.canonicalize(), graph_path.canonicalize()) {
        (Ok(source_path), Ok(graph_path)) => source_path == graph_path,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn slice(path: &str) -> GraphSlice {
        GraphSlice {
            schema_version: 1,
            revision_id: "revision-id".into(),
            revision_sequence: 7,
            source_fingerprint: "abc".into(),
            requested_document_version: Some(1),
            analyzed_document_version: Some(1),
            fresh: true,
            truncated: false,
            nodes: vec![
                GraphNode {
                    id: "place".into(),
                    kind: "place".into(),
                    label: "message".into(),
                    location: None,
                    certainty: "compiler_proven".into(),
                    properties: BTreeMap::new(),
                },
                GraphNode {
                    id: "borrow".into(),
                    kind: "borrow_event".into(),
                    label: "shared borrow region".into(),
                    location: Some(SourceLocation {
                        file: path.into(),
                        span: GraphSpan { start: 22, end: 30 },
                    }),
                    certainty: "compiler_proven".into(),
                    properties: BTreeMap::from([("mutable".into(), Value::Bool(false))]),
                },
            ],
            edges: vec![GraphEdge {
                kind: "borrows_shared".into(),
                source: "place".into(),
                target: "borrow".into(),
                location: Some(SourceLocation {
                    file: path.into(),
                    span: GraphSpan { start: 22, end: 30 },
                }),
                order: Some(0),
                certainty: "compiler_proven".into(),
                explanation: None,
            }],
        }
    }

    #[test]
    fn turns_compiler_graph_evidence_into_a_native_zed_helper() {
        let mut source = tempfile::NamedTempFile::new().unwrap();
        write!(source, "fn main() {{\n    let message = &source;\n}}\n").unwrap();
        let uri = Url::from_file_path(source.path()).unwrap().to_string();
        let decorations = graph_decorations(&uri, &slice(source.path().to_str().unwrap())).unwrap();

        assert_eq!(decorations.len(), 1);
        assert_eq!(decorations[0].kind, "imm_borrow");
        let markdown = decorations[0].hover_text.as_deref().unwrap();
        assert!(markdown.contains("Shared borrow"));
        assert!(markdown.contains("`message`"));
        assert!(markdown.contains("read-only view"));
        assert!(markdown.contains("Verified by rustc against the current source"));
        assert!(!markdown.contains("MIR"));
        assert!(!markdown.contains("revision"));
        assert!(!markdown.contains("schema"));
    }

    #[test]
    fn renders_borrow_edges_when_the_event_node_has_no_source_location() {
        let mut source = tempfile::NamedTempFile::new().unwrap();
        write!(source, "fn main() {{\n    let message = &source;\n}}\n").unwrap();
        let uri = Url::from_file_path(source.path()).unwrap().to_string();
        let mut graph = slice(source.path().to_str().unwrap());
        graph.nodes[1].location = None;

        let decorations = graph_decorations(&uri, &graph).unwrap();

        assert_eq!(decorations.len(), 1);
        assert_eq!(decorations[0].kind, "imm_borrow");
        let markdown = decorations[0].hover_text.as_deref().unwrap();
        assert!(markdown.contains("read-only view"));
        assert!(markdown.contains("`message` ── shared view ─→ `shared borrow region`"));
        assert!(!markdown.contains("**From**"));
        assert!(!markdown.contains("MIR"));
    }

    #[test]
    fn graph_query_contract_decodes_nested_snake_case_evidence() {
        let response: GraphQueryResponse = serde_json::from_value(serde_json::json!({
            "isAnalyzed": true,
            "status": "finished",
            "result": {
                "schema_version": 1,
                "revision_id": "r1",
                "revision_sequence": 3,
                "source_fingerprint": "abc",
                "requested_document_version": 2,
                "analyzed_document_version": 2,
                "fresh": true,
                "truncated": false,
                "nodes": [],
                "edges": []
            }
        }))
        .unwrap();
        assert!(response.is_analyzed);
        assert_eq!(response.result.unwrap().revision_sequence, 3);
    }

    #[test]
    fn hides_generated_flow_hints_from_function_signatures() {
        let source = "async fn inspect(message: &String) -> usize {\n    message.len()\n}\n";
        let signature = crate::semantic::Range {
            start: crate::semantic::Position {
                line: 0,
                character: 17,
            },
            end: crate::semantic::Position {
                line: 0,
                character: 24,
            },
        };
        let body = crate::semantic::Range {
            start: crate::semantic::Position {
                line: 1,
                character: 4,
            },
            end: crate::semantic::Position {
                line: 1,
                character: 11,
            },
        };
        let closing_brace = crate::semantic::Range {
            start: crate::semantic::Position {
                line: 2,
                character: 0,
            },
            end: crate::semantic::Position {
                line: 2,
                character: 1,
            },
        };

        assert!(signature_artifact(source, signature, "copy"));
        assert!(signature_artifact(source, signature, "maybe_initialized"));
        assert!(!signature_artifact(source, signature, "imm_borrow"));
        assert!(!signature_artifact(source, body, "copy"));
        assert!(signature_artifact(
            source,
            closing_brace,
            "maybe_initialized"
        ));
        assert!(!signature_artifact(source, closing_brace, "outlive"));
    }

    #[test]
    fn keeps_structural_mir_nodes_out_of_unrelated_source_hovers() {
        let source =
            "let writable = &mut *message;\nif ready { use_it(); }\nfor item in items {}\n";
        let at = |line| crate::semantic::Range {
            start: crate::semantic::Position { line, character: 0 },
            end: crate::semantic::Position { line, character: 3 },
        };

        assert!(signature_artifact(source, at(0), "branch_join"));
        assert!(signature_artifact(source, at(0), "loop_summary"));
        assert!(!signature_artifact(source, at(1), "branch_join"));
        assert!(!signature_artifact(source, at(2), "loop_summary"));
    }

    #[test]
    fn learner_labels_hide_simple_mir_dereference_syntax() {
        assert_eq!(learner_place_label("*(message)"), "message");
        assert_eq!(learner_place_label("*(state.field)"), "state.field");
        assert_eq!(learner_place_label("*(_23.0)"), "*(_23.0)");

        let source = GraphNode {
            id: "reference".into(),
            kind: "binding".into(),
            label: "writable".into(),
            location: None,
            certainty: "source_resolved".into(),
            properties: BTreeMap::from([("user_binding".into(), Value::Bool(true))]),
        };
        let target = GraphNode {
            id: "owner".into(),
            kind: "place".into(),
            label: "*(message)".into(),
            location: None,
            certainty: "source_resolved".into(),
            properties: BTreeMap::new(),
        };
        let range = crate::semantic::Range {
            start: crate::semantic::Position {
                line: 0,
                character: 8,
            },
            end: crate::semantic::Position {
                line: 0,
                character: 36,
            },
        };
        let labels = source_edge_labels(
            "    let writable = &mut *message;\n",
            range,
            "borrows_mut",
            &source,
            &target,
            "writable",
            "*(message)",
        );
        assert_eq!(labels, ("writable".into(), "message".into()));

        let graph = GraphSlice {
            schema_version: 1,
            revision_id: "revision-id".into(),
            revision_sequence: 8,
            source_fingerprint: "def".into(),
            requested_document_version: Some(1),
            analyzed_document_version: Some(1),
            fresh: true,
            truncated: false,
            nodes: vec![source, target],
            edges: vec![GraphEdge {
                kind: "borrows_mut".into(),
                source: "reference".into(),
                target: "owner".into(),
                location: None,
                order: None,
                certainty: "compiler_proven".into(),
                explanation: Some("reference assignment".into()),
            }],
        };
        let nodes: HashMap<_, _> = graph
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect();
        let markdown = edge_markdown(
            &graph.edges[0],
            edge_presentation(&graph.edges[0]).unwrap(),
            &graph.nodes[0],
            &graph.nodes[1],
            &graph,
            &nodes,
            "    let writable = &mut *message;\n",
            range,
        );
        assert!(markdown.contains("`writable` can modify `message`"));
        assert!(markdown.contains("`message` ── exclusive view ─→ `writable`"));
        assert!(!markdown.contains("*(message)"));
        assert!(!markdown.contains("MIR"));
    }

    #[test]
    fn liveness_helpers_explain_the_value_before_the_compiler_event() {
        let place = GraphNode {
            id: "message".into(),
            kind: "binding".into(),
            label: "message".into(),
            location: None,
            certainty: "source_resolved".into(),
            properties: BTreeMap::from([("user_binding".into(), Value::Bool(true))]),
        };
        let event = GraphNode {
            id: "liveness".into(),
            kind: "liveness_event".into(),
            label: "maybe_initialized".into(),
            location: None,
            certainty: "conservative".into(),
            properties: BTreeMap::from([(
                "class".into(),
                Value::String("maybe_initialized".into()),
            )]),
        };
        let graph = GraphSlice {
            schema_version: 1,
            revision_id: "revision-id".into(),
            revision_sequence: 8,
            source_fingerprint: "def".into(),
            requested_document_version: Some(1),
            analyzed_document_version: Some(1),
            fresh: true,
            truncated: false,
            nodes: vec![place, event],
            edges: vec![GraphEdge {
                kind: "reports".into(),
                source: "message".into(),
                target: "liveness".into(),
                location: None,
                order: None,
                certainty: "conservative".into(),
                explanation: None,
            }],
        };
        let nodes: HashMap<_, _> = graph
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect();
        let markdown = insight_markdown(
            &graph.nodes[1],
            insight_presentation(&graph.nodes[1]).unwrap(),
            &graph,
            &nodes,
        );
        assert!(markdown.contains("`message` available at this point"));
        assert!(!markdown.contains("maybe_initialized"));
        assert!(markdown.contains("Conservative: this may happen"));
        assert!(!markdown.contains("MIR"));
    }

    #[test]
    fn maps_internal_mir_places_to_user_bindings_without_hiding_provenance() {
        let revision = slice("/tmp/main.rs");
        let internal = GraphNode {
            id: "internal".into(),
            kind: "place".into(),
            label: "*(_23.0)".into(),
            location: None,
            certainty: "compiler_proven".into(),
            properties: BTreeMap::new(),
        };
        let binding = GraphNode {
            id: "binding".into(),
            kind: "binding".into(),
            label: "message".into(),
            location: None,
            certainty: "source_resolved".into(),
            properties: BTreeMap::from([("user_binding".into(), Value::Bool(true))]),
        };
        let nodes = HashMap::from([
            (internal.id.as_str(), &internal),
            (binding.id.as_str(), &binding),
        ]);
        let mut graph = revision;
        graph.nodes = vec![internal.clone(), binding.clone()];
        graph.edges = vec![GraphEdge {
            kind: "aliases".into(),
            source: internal.id.clone(),
            target: binding.id.clone(),
            location: None,
            order: Some(0),
            certainty: "source_resolved".into(),
            explanation: None,
        }];

        assert_eq!(developer_label(&internal, &graph, &nodes), "message");
    }

    #[test]
    fn layered_hover_serves_learners_before_expert_evidence() {
        let graph = slice("/tmp/main.rs");
        let nodes: HashMap<_, _> = graph
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect();
        let markdown = edge_markdown(
            &graph.edges[0],
            edge_presentation(&graph.edges[0]).unwrap(),
            &graph.nodes[0],
            &graph.nodes[1],
            &graph,
            &nodes,
            "let borrowed = &message;\n",
            crate::semantic::Range {
                start: crate::semantic::Position {
                    line: 0,
                    character: 4,
                },
                end: crate::semantic::Position {
                    line: 0,
                    character: 24,
                },
            },
        );

        assert!(markdown.contains("read-only view"));
        assert!(markdown.contains("**Why it matters**"));
        assert!(markdown.contains("**Flow**"));
        assert!(markdown.contains("Verified by rustc"));
        assert!(!markdown.contains("MIR"));
    }

    #[test]
    fn async_hover_explains_the_human_consequence_without_mir_temporaries() {
        let suspension = GraphNode {
            id: "await".into(),
            kind: "suspension_point".into(),
            label: "async suspension".into(),
            location: None,
            certainty: "compiler_proven".into(),
            properties: BTreeMap::new(),
        };
        let writable = GraphNode {
            id: "field-writable".into(),
            kind: "future_field".into(),
            label: "writable".into(),
            location: None,
            certainty: "compiler_proven".into(),
            properties: BTreeMap::from([
                ("field_index".into(), Value::from(0)),
                ("name".into(), Value::String("writable".into())),
                (
                    "type_name".into(),
                    Value::String("&mut alloc::string::String".into()),
                ),
                ("send".into(), Value::String("proven".into())),
                ("static_lifetime".into(), Value::String("rejected".into())),
                ("ignore_for_traits".into(), Value::Bool(false)),
            ]),
        };
        let generated = GraphNode {
            id: "field-generated".into(),
            kind: "future_field".into(),
            label: "generated future state #12".into(),
            location: None,
            certainty: "compiler_proven".into(),
            properties: BTreeMap::from([
                ("field_index".into(), Value::from(12)),
                ("name".into(), Value::Null),
                (
                    "type_name".into(),
                    Value::String("core::future::Ready<()>".into()),
                ),
                ("send".into(), Value::String("proven".into())),
                ("static_lifetime".into(), Value::String("proven".into())),
                ("ignore_for_traits".into(), Value::Bool(false)),
            ]),
        };
        let graph = GraphSlice {
            schema_version: 1,
            revision_id: "revision-id".into(),
            revision_sequence: 9,
            source_fingerprint: "async-source".into(),
            requested_document_version: Some(4),
            analyzed_document_version: Some(4),
            fresh: true,
            truncated: false,
            nodes: vec![suspension, writable, generated],
            edges: vec![
                GraphEdge {
                    kind: "live_across_await".into(),
                    source: "field-writable".into(),
                    target: "await".into(),
                    location: None,
                    order: Some(0),
                    certainty: "compiler_proven".into(),
                    explanation: None,
                },
                GraphEdge {
                    kind: "live_across_await".into(),
                    source: "field-generated".into(),
                    target: "await".into(),
                    location: None,
                    order: Some(12),
                    certainty: "compiler_proven".into(),
                    explanation: None,
                },
            ],
        };
        let nodes: HashMap<_, _> = graph
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect();
        let markdown = insight_markdown(
            &graph.nodes[0],
            insight_presentation(&graph.nodes[0]).unwrap(),
            &graph,
            &nodes,
        );

        assert!(markdown.contains("Await holds `writable`"));
        assert!(markdown.contains("`writable: &mut String`"));
        assert!(markdown.contains("**Exclusive borrow**"));
        assert!(markdown.contains("**`Send`** · **yes**"));
        assert!(markdown.contains("**`'static`** · **no**"));
        assert!(markdown.contains("**Cancellation**"));
        assert!(markdown.contains("1 compiler-generated field"));
        assert!(!markdown.contains("compiler temporary"));
        assert!(!markdown.contains("_12"));
        assert!(!markdown.contains("**Places**"));
    }

    #[test]
    fn source_assignment_names_replace_async_generator_temporaries_for_readers() {
        let source = GraphNode {
            id: "source".into(),
            kind: "place".into(),
            label: "_28".into(),
            location: None,
            certainty: "compiler_proven".into(),
            properties: BTreeMap::new(),
        };
        let target = GraphNode {
            id: "target".into(),
            kind: "place".into(),
            label: "*(_23.0)".into(),
            location: None,
            certainty: "compiler_proven".into(),
            properties: BTreeMap::new(),
        };
        let range = crate::semantic::Range {
            start: crate::semantic::Position {
                line: 0,
                character: 8,
            },
            end: crate::semantic::Position {
                line: 0,
                character: 35,
            },
        };

        let labels = source_edge_labels(
            "    let borrowed = message.as_str();\n",
            range,
            "borrows_shared",
            &source,
            &target,
            "compiler temporary _28",
            "compiler temporary *(_23.0)",
        );

        assert_eq!(labels, ("borrowed".into(), "message".into()));

        let graph = GraphSlice {
            schema_version: 1,
            revision_id: "revision-id".into(),
            revision_sequence: 8,
            source_fingerprint: "def".into(),
            requested_document_version: Some(1),
            analyzed_document_version: Some(1),
            fresh: true,
            truncated: false,
            nodes: vec![source, target],
            edges: vec![GraphEdge {
                kind: "borrows_shared".into(),
                source: "source".into(),
                target: "target".into(),
                location: None,
                order: None,
                certainty: "compiler_proven".into(),
                explanation: Some("reference assignment".into()),
            }],
        };
        let nodes: HashMap<_, _> = graph
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect();
        let markdown = edge_markdown(
            &graph.edges[0],
            edge_presentation(&graph.edges[0]).unwrap(),
            &graph.nodes[0],
            &graph.nodes[1],
            &graph,
            &nodes,
            "    let borrowed = message.as_str();\n",
            range,
        );

        assert!(markdown.contains("`borrowed` is a read-only view of `message`"));
        assert!(markdown.contains("`message` ── shared view ─→ `borrowed`"));
        assert!(!markdown.contains("*(_23.0)"));
        assert!(!markdown.contains("_28"));
        assert!(!markdown.contains("MIR"));
    }

    #[test]
    fn moved_call_argument_is_explained_in_source_terms_only() {
        let token = GraphNode {
            id: "token-mir".into(),
            kind: "place".into(),
            label: "_19".into(),
            location: None,
            certainty: "compiler_proven".into(),
            properties: BTreeMap::new(),
        };
        let parameter = GraphNode {
            id: "value".into(),
            kind: "binding".into(),
            label: "value".into(),
            location: None,
            certainty: "source_resolved".into(),
            properties: BTreeMap::from([("user_binding".into(), Value::Bool(true))]),
        };
        let edge = GraphEdge {
            kind: "moves_to".into(),
            source: token.id.clone(),
            target: parameter.id.clone(),
            location: None,
            order: Some(0),
            certainty: "compiler_proven".into(),
            explanation: Some("argument 1 binds to consume parameter 1".into()),
        };
        let graph = GraphSlice {
            schema_version: 1,
            revision_id: "f68f9039".into(),
            revision_sequence: 26,
            source_fingerprint: "360bcbc6".into(),
            requested_document_version: Some(0),
            analyzed_document_version: Some(0),
            fresh: true,
            truncated: false,
            nodes: vec![token, parameter],
            edges: vec![edge],
        };
        let nodes: HashMap<_, _> = graph
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect();
        let range = crate::semantic::Range {
            start: crate::semantic::Position {
                line: 0,
                character: 25,
            },
            end: crate::semantic::Position {
                line: 0,
                character: 39,
            },
        };
        let markdown = edge_markdown(
            &graph.edges[0],
            edge_presentation(&graph.edges[0]).unwrap(),
            &graph.nodes[0],
            &graph.nodes[1],
            &graph,
            &nodes,
            "let token_length: usize = consume(token);\n",
            range,
        );

        assert!(markdown.contains(
            "Calling `consume(token)` moves `token` into the `value` parameter of `consume`"
        ));
        assert!(markdown.contains("`token` cannot be used again after this line"));
        assert!(!markdown.contains("This value is transferred rather than copied"));
        assert!(markdown.contains("**Keep using `token`**"));
        assert!(markdown.contains("caller: `token` → `consume` → callee: `value`"));
        for forbidden in ["MIR", "_19", "revision", "schema", "360bcbc6", "f68f9039"] {
            assert!(
                !markdown.contains(forbidden),
                "leaked {forbidden}: {markdown}"
            );
        }
    }

    #[test]
    fn returned_method_result_uses_the_expression_the_developer_wrote() {
        let function = GraphNode {
            id: "callee".into(),
            kind: "function".into(),
            label: "<alloc::string::String as core::convert::AsRef<str>>::as_ref".into(),
            location: None,
            certainty: "compiler_proven".into(),
            properties: BTreeMap::new(),
        };
        let result = GraphNode {
            id: "result".into(),
            kind: "place".into(),
            label: "_28".into(),
            location: None,
            certainty: "compiler_proven".into(),
            properties: BTreeMap::new(),
        };
        let edge = GraphEdge {
            kind: "returns_as".into(),
            source: function.id.clone(),
            target: result.id.clone(),
            location: None,
            order: Some(0),
            certainty: "compiler_proven".into(),
            explanation: None,
        };
        let graph = GraphSlice {
            schema_version: 1,
            revision_id: "revision".into(),
            revision_sequence: 1,
            source_fingerprint: "fingerprint".into(),
            requested_document_version: Some(1),
            analyzed_document_version: Some(1),
            fresh: true,
            truncated: false,
            nodes: vec![function, result],
            edges: vec![edge],
        };
        let nodes: HashMap<_, _> = graph
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect();
        let range = crate::semantic::Range {
            start: crate::semantic::Position {
                line: 0,
                character: 4,
            },
            end: crate::semantic::Position {
                line: 0,
                character: 36,
            },
        };
        let markdown = edge_markdown(
            &graph.edges[0],
            edge_presentation(&graph.edges[0]).unwrap(),
            &graph.nodes[0],
            &graph.nodes[1],
            &graph,
            &nodes,
            "let borrowed = message.as_str();\n",
            range,
        );

        assert!(markdown.contains("result of `message.as_str()` is stored as `borrowed`"));
        assert!(markdown.contains("`message.as_str()` → `borrowed`"));
        assert!(!markdown.contains("alloc::"));
        assert!(!markdown.contains("_28"));
        assert!(!markdown.contains("MIR"));
    }
}
