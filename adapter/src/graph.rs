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
    pub revision_id: String,
    pub revision_sequence: u64,
    pub source_fingerprint: String,
    pub requested_document_version: Option<i64>,
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
        "borrows_shared" => Some(InsightPresentation {
            kind: "imm_borrow",
            title: "Shared borrow",
            summary: "The compiler creates a shared reference while the source retains ownership.",
        }),
        "borrows_mut" => Some(InsightPresentation {
            kind: "mut_borrow",
            title: "Exclusive borrow",
            summary: "The compiler creates an exclusive writable reference to this place.",
        }),
        "moves_to" => Some(InsightPresentation {
            kind: "move",
            title: "Ownership moved",
            summary: "The compiler consumes a non-Copy value along this MIR data-flow edge.",
        }),
        "copies_to" => Some(InsightPresentation {
            kind: "copy",
            title: "Value copied",
            summary: "The compiler duplicates this Copy value; the source remains available.",
        }),
        "mutates_through" => Some(InsightPresentation {
            kind: "mutation",
            title: "Value updated",
            summary: "This compiler-proven data-flow edge writes a new state into its destination.",
        }),
        "returns_as" => Some(InsightPresentation {
            kind: "return",
            title: "Value returned",
            summary: "Ownership or a reference flows into a call result or function return place.",
        }),
        "drops_at" | "cancellation_drops_at" => Some(InsightPresentation {
            kind: "drop",
            title: "Value dropped",
            summary: "The compiler routes this value into destructor or cancellation cleanup.",
        }),
        "live_across_await" => Some(InsightPresentation {
            kind: "async_suspend",
            title: "Live across async suspension",
            summary: "Compiler liveness retains this place inside the future across a suspension point.",
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
    let (source_label, target_label, source_node, target_node) = if reverse_borrow_assignment {
        (target_label, source_label, target, source)
    } else {
        (source_label, target_label, source, target)
    };
    let consequence = edge_consequence(&edge.kind, &source_label, &target_label);
    let source_evidence = evidence_label(&source_label, &source_node.label);
    let target_evidence = evidence_label(&target_label, &target_node.label);
    let mut markdown = format!(
        "### RustOwl · {}\n\n{}\n\n**What this means** · {}\n\n**Compiler evidence**\n\n- **Certainty** · {}\n- **MIR flow** · `{}`\n- **From** · `{}`\n- **To** · `{}`",
        presentation.title,
        presentation.summary,
        consequence,
        certainty_label(&edge.certainty),
        edge.kind.replace('_', " "),
        source_evidence,
        target_evidence,
    );
    if let Some(explanation) = edge.explanation.as_deref() {
        markdown.push_str(&format!("\n- **Compiler flow** · {explanation}"));
    }
    markdown.push_str(&format!(
        "\n\n**Ownership flow**\n\n`{}` → **{}** → `{}`",
        source_label,
        edge.kind.replace('_', " "),
        target_label,
    ));
    append_revision_footer(&mut markdown, slice);
    markdown
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

fn evidence_label(readable: &str, compiler: &str) -> String {
    if readable == compiler {
        readable.to_owned()
    } else {
        format!("{readable} (MIR {compiler})")
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

fn edge_consequence(kind: &str, source: &str, target: &str) -> String {
    match kind {
        "borrows_shared" => format!(
            "`{source}` keeps ownership. `{target}` may read the value, while mutation waits until the last shared use ends."
        ),
        "borrows_mut" => format!(
            "`{target}` has temporary exclusive write access to `{source}`; competing reads and writes are blocked for that borrow."
        ),
        "moves_to" => format!(
            "Ownership leaves `{source}` and continues at `{target}`. The source cannot be used again unless it is reinitialized."
        ),
        "copies_to" => format!(
            "The value is copied from `{source}` to `{target}`. Both remain usable because the transferred value is `Copy`."
        ),
        "mutates_through" => format!(
            "This operation writes the next state of `{target}`; any active borrow must permit mutation."
        ),
        "returns_as" => format!(
            "The value or reference flows from `{source}` into `{target}` as a call or function result."
        ),
        "drops_at" | "cancellation_drops_at" => format!(
            "`{source}` is cleaned up at `{target}` and is unavailable afterward unless initialized again."
        ),
        "live_across_await" => format!(
            "`{source}` is stored inside the future across `{target}`. It must remain valid through suspension and is considered on cancellation."
        ),
        _ => format!("Ownership state flows from `{source}` to `{target}`."),
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
            labels.truncate(2);
            return format!("{} (via {})", labels.join(" / "), node.label);
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
    format!("compiler temporary {}", node.label)
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
                    summary: "`&mut T` grants temporary writable access while excluding competing aliases.",
                }
            } else {
                InsightPresentation {
                    kind: "imm_borrow",
                    title: "Shared borrow",
                    summary: "`&T` grants shared read access while the source retains ownership.",
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
            summary: "MIR records values passed into this call and the place receiving its result.",
        },
        "move_event" => InsightPresentation {
            kind: "move",
            title: "Ownership moved",
            summary: "A non-Copy value leaves its source place until that place is reinitialized.",
        },
        "mutation_event" => InsightPresentation {
            kind: "mutation",
            title: "Value updated",
            summary: "This MIR assignment writes a new state into its destination place.",
        },
        "drop_event" => InsightPresentation {
            kind: "drop",
            title: "Value dropped",
            summary: "The value's destructor or storage cleanup runs on this path.",
        },
        "return_event" => InsightPresentation {
            kind: "return",
            title: "Value returned",
            summary: "Ownership or a reference leaves this function through its MIR return place.",
        },
        "suspension_point" => InsightPresentation {
            kind: "async_suspend",
            title: "Async suspension",
            summary: "The generated future can suspend here while retaining compiler-live state.",
        },
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
    let subject = insight_subject(node, slice, nodes);
    let mut markdown = format!(
        "### RustOwl · {}\n\n{}\n\n**What this means** · {}\n\n**Compiler evidence**\n\n- **Certainty** · {}\n- **Evidence kind** · `{}`\n- **Subject** · `{}`",
        presentation.title,
        presentation.summary,
        insight_consequence(node, &subject),
        certainty_label(&node.certainty),
        node.kind.replace('_', " "),
        subject,
    );
    if node.kind == "liveness_event" {
        let state = node
            .properties
            .get("class")
            .and_then(Value::as_str)
            .unwrap_or(&node.label)
            .replace('_', " ");
        markdown.push_str(&format!("\n- **Compiler state** · `{state}`"));
    }

    let mut related_places = Vec::new();
    let mut capabilities = Vec::new();
    let mut flows = Vec::new();
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
                if related != subject {
                    related_places.push(format!("`{related}`"));
                }
            }
            if other.kind == "capability_snapshot"
                && let Some(capability) = other.properties.get("capability").and_then(Value::as_str)
            {
                capabilities.push(capability.replace('_', " "));
            }
            if flows.len() < 4 {
                let (from, to) = if edge.source == node.id {
                    (
                        developer_label(node, slice, nodes),
                        developer_label(other, slice, nodes),
                    )
                } else {
                    (
                        developer_label(other, slice, nodes),
                        developer_label(node, slice, nodes),
                    )
                };
                flows.push(format!(
                    "`{from}` → **{}** → `{to}`{}",
                    edge.kind.replace('_', " "),
                    edge.explanation
                        .as_deref()
                        .map(|explanation| format!(" · {explanation}"))
                        .unwrap_or_default()
                ));
            }
        }
    }
    related_places.sort();
    related_places.dedup();
    capabilities.sort();
    capabilities.dedup();
    if !related_places.is_empty() {
        markdown.push_str(&format!(
            "\n- **Place{}** · {}",
            if related_places.len() == 1 { "" } else { "s" },
            related_places.join(", ")
        ));
    }
    if !capabilities.is_empty() {
        markdown.push_str(&format!(
            "\n- **Capability** · {}",
            capabilities.join(" · ")
        ));
    }
    append_property_facts(&mut markdown, node);
    if node.kind == "suspension_point" {
        markdown.push_str(
            "\n- **Cancellation** · dropping the future follows the compiler cleanup edge and drops retained state",
        );
    }
    if node.kind == "call_site" && node.certainty == "unresolved" {
        markdown.push_str(
            "\n- **Boundary** · argument moves/borrows are compiler-grounded; the callee identity is not resolved in this revision",
        );
    }
    if !flows.is_empty() {
        markdown.push_str("\n\n**Local ownership flow**\n\n");
        markdown.push_str(&flows.join("  \n"));
    }
    append_revision_footer(&mut markdown, slice);
    markdown
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

fn append_revision_footer(markdown: &mut String, slice: &GraphSlice) {
    let revision = slice.revision_id.chars().take(8).collect::<String>();
    let fingerprint = slice.source_fingerprint.chars().take(8).collect::<String>();
    let version = match (
        slice.requested_document_version,
        slice.analyzed_document_version,
    ) {
        (Some(requested), Some(analyzed)) if requested == analyzed => {
            format!(" · document v{analyzed}")
        }
        (Some(requested), Some(analyzed)) => {
            format!(" · requested v{requested}, analyzed v{analyzed}")
        }
        (Some(requested), None) => format!(" · requested v{requested}"),
        (None, Some(analyzed)) => format!(" · analyzed v{analyzed}"),
        (None, None) => String::new(),
    };
    markdown.push_str(&format!(
        "\n\n`revision {} ({revision}) · source {fingerprint} · schema v{}{}{}{} `",
        slice.revision_sequence,
        slice.schema_version,
        version,
        if slice.fresh {
            " · fresh"
        } else {
            " · stale"
        },
        if slice.truncated {
            " · bounded result"
        } else {
            ""
        },
    ));
}

fn append_property_facts(markdown: &mut String, node: &GraphNode) {
    for (property, label) in [
        ("resume_block", "Resume block"),
        ("drop_block", "Cancellation block"),
        ("predecessors", "Predecessors"),
        ("back_edge_sources", "Back edges"),
        ("boundary", "Boundary"),
    ] {
        let Some(value) = node.properties.get(property) else {
            continue;
        };
        markdown.push_str(&format!("\n- **{label}** · `{}`", compact_value(value)));
    }
}

fn compact_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        _ => value.to_string(),
    }
}

fn certainty_label(certainty: &str) -> &'static str {
    match certainty {
        "compiler_proven" => "compiler-proven MIR fact",
        "source_resolved" => "source-resolved fact",
        "conservative" => "conservative possibility",
        _ => "explicitly unresolved boundary",
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
        assert!(markdown.contains("compiler-proven MIR fact"));
        assert!(markdown.contains("`message`"));
        assert!(markdown.contains("**borrows shared**"));
        assert!(markdown.contains("revision 7 (revision)"));
        assert!(markdown.contains("schema v1 · document v1 · fresh"));
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
        assert!(markdown.contains("The compiler creates a shared reference"));
        assert!(markdown.contains("**From** · `message`"));
        assert!(markdown.contains("**To** · `shared borrow region`"));
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
        let learner = markdown.split("**Compiler evidence**").next().unwrap();
        assert!(learner.contains("`writable` has temporary exclusive write access to `message`"));
        assert!(!learner.contains("*(message)"));
        assert!(markdown.contains("**From** · `message (MIR *(message))`"));
        assert!(markdown.contains("`message` → **borrows mut** → `writable`"));
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
        let learner = markdown.split("**Compiler evidence**").next().unwrap();
        assert!(learner.contains("`message` available at this point"));
        assert!(!learner.contains("maybe_initialized"));
        assert!(markdown.contains("**Subject** · `message`"));
        assert!(markdown.contains("**Compiler state** · `maybe initialized`"));
        assert!(markdown.contains("`message` → **reports** → `maybe_initialized`"));
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

        assert_eq!(
            developer_label(&internal, &graph, &nodes),
            "message (via *(_23.0))"
        );
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

        assert!(markdown.contains("**What this means**"));
        assert!(markdown.contains("keeps ownership"));
        assert!(markdown.contains("**Compiler evidence**"));
        assert!(markdown.contains("**MIR flow**"));
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
        assert_eq!(
            evidence_label(&labels.0, &source.label),
            "borrowed (MIR _28)"
        );

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

        assert!(markdown.contains("`message` keeps ownership. `borrowed` may read"));
        assert!(markdown.contains("`message` → **borrows shared** → `borrowed`"));
        assert!(markdown.contains("**From** · `message (MIR *(_23.0))`"));
        assert!(markdown.contains("**To** · `borrowed (MIR _28)`"));
    }
}
