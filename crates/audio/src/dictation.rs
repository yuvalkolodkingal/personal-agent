//! Deterministic, platform-neutral streaming dictation and voice-command primitives.
//!
//! This module deliberately produces edit operations instead of injecting input. The desktop
//! runtime owns the OS-specific accessibility, clipboard, and keystroke adapters and can enforce
//! policy before an operation reaches the focused application.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use thiserror::Error;

const DEFAULT_STABILITY_OBSERVATIONS: u8 = 2;
const MAX_LATENCY_SAMPLES: usize = 512;

/// Dictation processing mode selected by a spoken or visible control.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DictationMode {
    /// English prose with spoken punctuation and layout controls.
    #[default]
    Natural,
    /// Nearly verbatim text; only explicit spelling is interpreted.
    Literal,
    /// English code vocabulary, punctuation, newlines, and indentation.
    Code,
}

/// Whether a partial token is safe to present as stable or may still be revised by STT.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenStability {
    Stable,
    Unstable,
}

/// One display token in the current streaming utterance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DictationToken {
    pub text: String,
    pub stability: TokenStability,
}

/// Recognition event accepted by [`DictationEngine`]. All time values use the same monotonic
/// clock owned by the caller, which makes latency reporting deterministic and testable.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PartialTranscript {
    pub text: String,
    pub final_result: bool,
    pub audio_end_ms: Option<u64>,
    pub received_at_ms: u64,
}

/// Which matching text occurrence a semantic correction should affect.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Occurrence {
    First,
    Last,
    All,
}

/// Rich or structural formatting requested by voice.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Formatting {
    Bold,
    Heading { level: u8 },
    BulletedList,
    NumberedList,
}

/// Platform-neutral operations applied to the focused editable target.
///
/// Counts are UTF-16 code units because Windows UIA, macOS accessibility-backed web controls,
/// browser DOM selections, and JavaScript all expose UTF-16 compatible offsets. Native adapters
/// must translate when their underlying API uses scalar or byte indices.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum EditOperation {
    /// Revise only the changed suffix of the active streaming transaction.
    ReplaceProvisionalTail {
        transaction_id: u64,
        retain_utf16: usize,
        delete_utf16: usize,
        insert: String,
        stable_prefix_utf16: usize,
    },
    /// Make the active provisional transaction part of document history.
    CommitTransaction {
        transaction_id: u64,
    },
    /// Remove the last engine-committed utterance after checking its expected text.
    DeleteLastUtterance {
        expected: String,
        utf16_len: usize,
    },
    Undo,
    ReplaceLiteral {
        find: String,
        replacement: String,
        occurrence: Occurrence,
    },
    InsertRelative {
        anchor: String,
        text: String,
        before: bool,
        occurrence: Occurrence,
    },
    InsertText {
        text: String,
    },
    FormatLastUtterance {
        expected: Option<String>,
        formatting: Formatting,
    },
    ChangeIndent {
        levels: i8,
    },
    SetMode {
        mode: DictationMode,
    },
}

/// Result of one streaming recognition update.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DictationUpdate {
    pub transaction_id: u64,
    pub mode: DictationMode,
    pub tokens: Vec<DictationToken>,
    pub rendered_text: String,
    pub final_result: bool,
    pub operations: Vec<EditOperation>,
}

/// Editing features exposed by a focused application adapter.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TargetCapabilities {
    /// Direct replacement of a provisional range without visible select/delete churn.
    pub provisional_replacement: bool,
    /// Semantic lookup and replacement within the current document or field.
    pub text_search: bool,
    /// Native or application-provided undo.
    pub undo: bool,
    /// Rich text spans and structural blocks.
    pub rich_text: bool,
    /// Plain-text Markdown formatting is accepted by this target.
    pub markdown: bool,
    /// Multiline insertion is accepted.
    pub multiline: bool,
    /// Target provides direct text APIs, such as UIA `TextPattern`/`AXValue`/AT-SPI
    /// `EditableText`.
    pub direct_text_api: bool,
    /// Accessibility action/value APIs are writable.
    pub accessibility_write: bool,
    /// Clipboard paste is allowed by user policy for this target.
    pub clipboard_paste: bool,
    /// Synthesized keystrokes are allowed by user policy for this target.
    pub keystrokes: bool,
}

/// Selected mechanism for applying an edit on the current platform and target.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditStrategy {
    SessionState,
    DirectTextApi,
    Accessibility,
    ClipboardPaste,
    Keystrokes,
    Unsupported,
}

/// Adapter plan for an edit, including whether semantics are degraded by fallback.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApplicationPlan {
    pub strategy: EditStrategy,
    pub exact: bool,
    pub reason: Option<String>,
}

/// Successful native edit receipt. The desktop uses this acknowledgement for apply latency and
/// must not claim dictation success without it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EditReceipt {
    pub transaction_id: Option<u64>,
    pub applied_at_ms: u64,
    pub strategy: EditStrategy,
    pub verified: bool,
}

/// OS/application adapter implemented outside the audio crate.
pub trait DictationTarget {
    fn capabilities(&self) -> TargetCapabilities;

    /// Apply and verify one edit against the still-focused target.
    ///
    /// # Errors
    ///
    /// Returns [`DictationError::StaleTarget`] if focus or document identity changed,
    /// [`DictationError::Unsupported`] if no permitted adapter can apply the edit, or
    /// [`DictationError::Verification`] when the edit postcondition cannot be proven.
    fn apply(&mut self, operation: &EditOperation) -> Result<EditReceipt, DictationError>;
}

/// Apply an operation and reject an unverified receipt so callers cannot report false success.
///
/// # Errors
///
/// Propagates adapter errors and returns [`DictationError::Verification`] when the native adapter
/// cannot prove the operation's postcondition.
pub fn apply_verified(
    target: &mut impl DictationTarget,
    operation: &EditOperation,
) -> Result<EditReceipt, DictationError> {
    let receipt = target.apply(operation)?;
    if receipt.verified {
        Ok(receipt)
    } else {
        Err(DictationError::Verification(
            "target adapter returned an unverified receipt".into(),
        ))
    }
}

/// Dictation or adapter failure with an actionable fallback explanation.
#[derive(Clone, Debug, Error, Eq, PartialEq, Serialize, Deserialize)]
pub enum DictationError {
    #[error("focused target does not support this edit: {0}")]
    Unsupported(String),
    #[error("edit target changed during dictation")]
    StaleTarget,
    #[error("edit was applied but postcondition verification failed: {0}")]
    Verification(String),
    #[error("dictation input is invalid: {0}")]
    InvalidInput(String),
}

/// Resolve the safest usable strategy without silently treating a degraded edit as exact.
#[must_use]
pub fn plan_edit(operation: &EditOperation, capabilities: &TargetCapabilities) -> ApplicationPlan {
    if matches!(
        operation,
        EditOperation::CommitTransaction { .. } | EditOperation::SetMode { .. }
    ) {
        return ApplicationPlan {
            strategy: EditStrategy::SessionState,
            exact: true,
            reason: None,
        };
    }
    let needs_search = matches!(
        operation,
        EditOperation::ReplaceLiteral { .. } | EditOperation::InsertRelative { .. }
    );
    let needs_rich = matches!(operation, EditOperation::FormatLastUtterance { .. });
    let needs_revision = matches!(
        operation,
        EditOperation::ReplaceProvisionalTail { .. } | EditOperation::DeleteLastUtterance { .. }
    );
    let needs_multiline = matches!(
        operation,
        EditOperation::InsertText { text } if text.contains('\n')
    );
    let native_exact = (!needs_search || capabilities.text_search)
        && (!needs_rich || capabilities.rich_text)
        && (!needs_multiline || capabilities.multiline)
        && (!matches!(operation, EditOperation::Undo) || capabilities.undo)
        && (!matches!(operation, EditOperation::ReplaceProvisionalTail { .. })
            || capabilities.provisional_replacement);

    if capabilities.direct_text_api && native_exact {
        return ApplicationPlan {
            strategy: EditStrategy::DirectTextApi,
            exact: true,
            reason: None,
        };
    }
    if capabilities.accessibility_write && native_exact {
        return ApplicationPlan {
            strategy: EditStrategy::Accessibility,
            exact: true,
            reason: None,
        };
    }
    if needs_rich
        && capabilities.markdown
        && capabilities.clipboard_paste
        && capabilities.keystrokes
    {
        return ApplicationPlan {
            strategy: EditStrategy::ClipboardPaste,
            exact: false,
            reason: Some("rich formatting will be represented as Markdown".into()),
        };
    }
    if !needs_search
        && !matches!(operation, EditOperation::Undo)
        && capabilities.clipboard_paste
        && (!needs_revision || capabilities.keystrokes)
    {
        return ApplicationPlan {
            strategy: EditStrategy::ClipboardPaste,
            exact: false,
            reason: Some(
                "target lacks transactional text APIs; selection may briefly change".into(),
            ),
        };
    }
    if !needs_search && !needs_rich && capabilities.keystrokes {
        return ApplicationPlan {
            strategy: EditStrategy::Keystrokes,
            exact: false,
            reason: Some("target only supports synthesized keyboard input".into()),
        };
    }
    ApplicationPlan {
        strategy: EditStrategy::Unsupported,
        exact: false,
        reason: Some(
            if needs_search {
                "target cannot search its editable text for a correction"
            } else if needs_rich {
                "target supports neither rich text nor an allowed Markdown fallback"
            } else {
                "no permitted writable target adapter is available"
            }
            .into(),
        ),
    }
}

/// Rolling latency percentile report in milliseconds.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LatencyDistribution {
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub maximum_ms: u64,
    pub sample_count: usize,
}

/// Recognition-to-partial and target-application latency.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LatencyReport {
    pub first_partial: LatencyDistribution,
    pub partial_updates: LatencyDistribution,
    pub finalization: LatencyDistribution,
    pub target_apply: LatencyDistribution,
}

/// Bounded latency recorder for the dictation HUD and diagnostics.
#[derive(Clone, Debug, Default)]
pub struct DictationLatencyMetrics {
    first_partial: VecDeque<u64>,
    partial_updates: VecDeque<u64>,
    finalization: VecDeque<u64>,
    target_apply: VecDeque<u64>,
    utterance_has_partial: bool,
}

impl DictationLatencyMetrics {
    pub fn record_recognition(&mut self, event: &PartialTranscript) {
        let Some(audio_end_ms) = event.audio_end_ms else {
            return;
        };
        let elapsed = event.received_at_ms.saturating_sub(audio_end_ms);
        if event.final_result {
            push_bounded(&mut self.finalization, elapsed);
            self.utterance_has_partial = false;
        } else if self.utterance_has_partial {
            push_bounded(&mut self.partial_updates, elapsed);
        } else {
            push_bounded(&mut self.first_partial, elapsed);
            self.utterance_has_partial = true;
        }
    }

    pub fn record_apply(&mut self, dispatched_at_ms: u64, receipt: &EditReceipt) {
        push_bounded(
            &mut self.target_apply,
            receipt.applied_at_ms.saturating_sub(dispatched_at_ms),
        );
    }

    #[must_use]
    pub fn report(&self) -> LatencyReport {
        LatencyReport {
            first_partial: distribution(&self.first_partial),
            partial_updates: distribution(&self.partial_updates),
            finalization: distribution(&self.finalization),
            target_apply: distribution(&self.target_apply),
        }
    }
}

fn push_bounded(samples: &mut VecDeque<u64>, value: u64) {
    if samples.len() == MAX_LATENCY_SAMPLES {
        samples.pop_front();
    }
    samples.push_back(value);
}

fn distribution(samples: &VecDeque<u64>) -> LatencyDistribution {
    if samples.is_empty() {
        return LatencyDistribution::default();
    }
    let mut ordered = samples.iter().copied().collect::<Vec<_>>();
    ordered.sort_unstable();
    let percentile = |percent: usize| {
        let index = (ordered.len().saturating_sub(1) * percent).div_ceil(100);
        ordered[index]
    };
    LatencyDistribution {
        p50_ms: percentile(50),
        p95_ms: percentile(95),
        maximum_ms: *ordered.last().unwrap_or(&0),
        sample_count: ordered.len(),
    }
}

/// Stateful streaming dictation diff and correction engine.
#[derive(Clone, Debug)]
pub struct DictationEngine {
    mode: DictationMode,
    transaction_id: u64,
    transaction_prefix: String,
    prior_rendered: String,
    prior_tokens: Vec<String>,
    stability_runs: Vec<u8>,
    committed_utterances: Vec<String>,
    stability_observations: u8,
    suppress_next_separator: bool,
    metrics: DictationLatencyMetrics,
}

impl Default for DictationEngine {
    fn default() -> Self {
        Self {
            mode: DictationMode::Natural,
            transaction_id: 1,
            transaction_prefix: String::new(),
            prior_rendered: String::new(),
            prior_tokens: Vec::new(),
            stability_runs: Vec::new(),
            committed_utterances: Vec::new(),
            stability_observations: DEFAULT_STABILITY_OBSERVATIONS,
            suppress_next_separator: false,
            metrics: DictationLatencyMetrics::default(),
        }
    }
}

impl DictationEngine {
    #[must_use]
    pub fn new(mode: DictationMode) -> Self {
        Self {
            mode,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn mode(&self) -> DictationMode {
        self.mode
    }

    #[must_use]
    pub fn latency_report(&self) -> LatencyReport {
        self.metrics.report()
    }

    pub fn record_apply(&mut self, dispatched_at_ms: u64, receipt: &EditReceipt) {
        self.metrics.record_apply(dispatched_at_ms, receipt);
    }

    /// Consume a partial or final transcript and emit the smallest safe suffix revision.
    ///
    /// Final correction/formatting phrases clear their provisional command text and emit semantic
    /// target operations. Normal final speech is committed as one undoable utterance.
    ///
    /// # Errors
    ///
    /// Returns [`DictationError::InvalidInput`] when a transcript contains characters that cannot
    /// be passed safely through native editing APIs.
    pub fn ingest(&mut self, event: PartialTranscript) -> Result<DictationUpdate, DictationError> {
        if event.text.contains('\0') {
            return Err(DictationError::InvalidInput(
                "transcript contains a NUL character".into(),
            ));
        }
        self.metrics.record_recognition(&event);
        let transaction_id = self.transaction_id;
        let PartialTranscript {
            text, final_result, ..
        } = event;
        let raw = text.trim();
        let control = final_result.then(|| parse_control(raw)).flatten();
        let display_text = control.as_ref().map_or_else(
            || render_spoken(raw, self.mode),
            |parsed| parsed.visible_text.clone().unwrap_or_default(),
        );
        if control.is_none() && self.prior_rendered.is_empty() && !display_text.is_empty() {
            self.transaction_prefix = self.separator_for(&display_text);
        }
        let rendered = format!("{}{}", self.transaction_prefix, display_text);
        let words = tokenize(&display_text);
        self.update_stability(&words);
        let tokens = words
            .iter()
            .enumerate()
            .map(|(index, text)| DictationToken {
                text: text.clone(),
                stability: if final_result
                    || self.stability_runs.get(index).copied().unwrap_or(0)
                        >= self.stability_observations
                {
                    TokenStability::Stable
                } else {
                    TokenStability::Unstable
                },
            })
            .collect::<Vec<_>>();
        let stable_prefix = tokens
            .iter()
            .take_while(|token| token.stability == TokenStability::Stable)
            .map(|token| token.text.encode_utf16().count() + 1)
            .sum::<usize>()
            .saturating_add(self.transaction_prefix.encode_utf16().count())
            .min(rendered.encode_utf16().count());
        let mut operations = Vec::new();
        if rendered != self.prior_rendered {
            operations.push(diff_operation(
                self.transaction_id,
                &self.prior_rendered,
                &rendered,
                stable_prefix,
            ));
        }
        if final_result {
            if let Some(control) = control {
                operations.extend(self.apply_control(control));
            } else if !rendered.is_empty() {
                operations.push(EditOperation::CommitTransaction {
                    transaction_id: self.transaction_id,
                });
                self.committed_utterances.push(rendered.clone());
            }
            self.start_next_transaction();
        } else {
            self.prior_rendered.clone_from(&rendered);
            self.prior_tokens = words;
        }
        Ok(DictationUpdate {
            transaction_id,
            mode: self.mode,
            tokens,
            rendered_text: display_text,
            final_result,
            operations,
        })
    }

    fn update_stability(&mut self, words: &[String]) {
        let prior_runs = self.stability_runs.clone();
        self.stability_runs = words
            .iter()
            .enumerate()
            .map(|(index, word)| {
                if self.prior_tokens.get(index) == Some(word) {
                    prior_runs
                        .get(index)
                        .copied()
                        .unwrap_or(1)
                        .saturating_add(1)
                } else {
                    1
                }
            })
            .collect();
    }

    fn apply_control(&mut self, control: ParsedControl) -> Vec<EditOperation> {
        let mut operations = control.operations;
        let latest = self.committed_utterances.last().cloned();
        for operation in &mut operations {
            match operation {
                EditOperation::DeleteLastUtterance {
                    expected,
                    utf16_len,
                } => {
                    if let Some(latest) = &latest {
                        expected.clone_from(latest);
                        *utf16_len = latest.encode_utf16().count();
                    } else {
                        *operation = EditOperation::Undo;
                    }
                }
                EditOperation::FormatLastUtterance { expected, .. } => {
                    expected.clone_from(&latest);
                }
                _ => {}
            }
        }
        for operation in &operations {
            if let EditOperation::SetMode { mode } = operation {
                self.mode = *mode;
            }
            if matches!(
                operation,
                EditOperation::InsertText { text } if text.ends_with('\n')
            ) || matches!(operation, EditOperation::ChangeIndent { .. })
            {
                self.suppress_next_separator = true;
            }
        }
        if operations
            .iter()
            .any(|operation| matches!(operation, EditOperation::DeleteLastUtterance { .. }))
        {
            self.committed_utterances.pop();
        }
        if operations.is_empty() && control.visible_text.is_none() {
            operations.push(EditOperation::Undo);
        }
        operations
    }

    fn start_next_transaction(&mut self) {
        self.transaction_id = self.transaction_id.saturating_add(1);
        self.transaction_prefix.clear();
        self.prior_rendered.clear();
        self.prior_tokens.clear();
        self.stability_runs.clear();
    }

    fn separator_for(&mut self, next: &str) -> String {
        if self.suppress_next_separator {
            self.suppress_next_separator = false;
            return String::new();
        }
        let prior = self
            .committed_utterances
            .last()
            .and_then(|utterance| utterance.chars().next_back());
        let next = next.chars().next();
        if prior.is_none_or(|character| character.is_whitespace() || "([{/'\"".contains(character))
            || next.is_some_and(|character| {
                character.is_whitespace() || ".,?!:;)]}".contains(character)
            })
        {
            String::new()
        } else {
            " ".into()
        }
    }
}

fn diff_operation(
    transaction_id: u64,
    before: &str,
    after: &str,
    stable_prefix_utf16: usize,
) -> EditOperation {
    let common_chars = before
        .chars()
        .zip(after.chars())
        .take_while(|(left, right)| left == right)
        .count();
    let retain_bytes = after
        .char_indices()
        .nth(common_chars)
        .map_or(after.len(), |(index, _)| index);
    let before_retain_bytes = before
        .char_indices()
        .nth(common_chars)
        .map_or(before.len(), |(index, _)| index);
    EditOperation::ReplaceProvisionalTail {
        transaction_id,
        retain_utf16: after[..retain_bytes].encode_utf16().count(),
        delete_utf16: before[before_retain_bytes..].encode_utf16().count(),
        insert: after[retain_bytes..].into(),
        stable_prefix_utf16,
    }
}

#[derive(Clone, Debug)]
struct ParsedControl {
    visible_text: Option<String>,
    operations: Vec<EditOperation>,
}

fn parse_control(raw: &str) -> Option<ParsedControl> {
    let command = raw
        .trim()
        .trim_matches(|character: char| character.is_ascii_punctuation());
    let normalized = command.to_ascii_lowercase();
    let set_mode = match normalized.as_str() {
        "natural mode" | "prose mode" => Some(DictationMode::Natural),
        "literal mode" => Some(DictationMode::Literal),
        "code mode" | "coding mode" => Some(DictationMode::Code),
        _ => None,
    };
    if let Some(mode) = set_mode {
        return Some(ParsedControl {
            visible_text: None,
            operations: vec![EditOperation::SetMode { mode }],
        });
    }
    if matches!(normalized.as_str(), "scratch that" | "delete that") {
        return Some(ParsedControl {
            visible_text: None,
            operations: vec![EditOperation::DeleteLastUtterance {
                expected: String::new(),
                utf16_len: 0,
            }],
        });
    }
    if matches!(normalized.as_str(), "undo" | "undo that") {
        return Some(ParsedControl {
            visible_text: None,
            operations: vec![EditOperation::Undo],
        });
    }
    if let Some((find, replacement)) = split_control(command, "replace ", " with ")
        .filter(|(find, replacement)| !find.is_empty() && !replacement.is_empty())
    {
        return Some(ParsedControl {
            visible_text: None,
            operations: vec![EditOperation::ReplaceLiteral {
                find: find.into(),
                replacement: replacement.into(),
                occurrence: Occurrence::Last,
            }],
        });
    }
    if let Some((text, anchor)) = split_control(command, "insert ", " before ")
        .filter(|(text, anchor)| !text.is_empty() && !anchor.is_empty())
    {
        return Some(relative_control(text, anchor, true));
    }
    if let Some((text, anchor)) = split_control(command, "insert ", " after ")
        .filter(|(text, anchor)| !text.is_empty() && !anchor.is_empty())
    {
        return Some(relative_control(text, anchor, false));
    }
    match normalized.as_str() {
        "new line" | "newline" => Some(insert_control("\n")),
        "new paragraph" | "paragraph break" => Some(insert_control("\n\n")),
        "indent" | "indent that" => Some(indent_control(1)),
        "dedent" | "outdent" | "outdent that" => Some(indent_control(-1)),
        "bold that" | "make that bold" => Some(format_control(Formatting::Bold)),
        "make that a heading" | "heading" => Some(format_control(Formatting::Heading { level: 1 })),
        "make that heading two" | "heading two" => {
            Some(format_control(Formatting::Heading { level: 2 }))
        }
        "make that a bullet list" | "bullet list" | "bullet that" | "bullets" => {
            Some(format_control(Formatting::BulletedList))
        }
        "make that a numbered list" | "number that" => {
            Some(format_control(Formatting::NumberedList))
        }
        _ => None,
    }
}

fn split_control<'a>(source: &'a str, prefix: &str, separator: &str) -> Option<(&'a str, &'a str)> {
    let lowered = source.to_ascii_lowercase();
    if !lowered.starts_with(prefix) {
        return None;
    }
    let separator_start = lowered[prefix.len()..].find(separator)? + prefix.len();
    Some((
        source[prefix.len()..separator_start].trim(),
        source[separator_start + separator.len()..].trim(),
    ))
}

fn relative_control(text: &str, anchor: &str, before: bool) -> ParsedControl {
    ParsedControl {
        visible_text: None,
        operations: vec![EditOperation::InsertRelative {
            anchor: anchor.into(),
            text: text.into(),
            before,
            occurrence: Occurrence::Last,
        }],
    }
}

fn insert_control(text: &str) -> ParsedControl {
    ParsedControl {
        visible_text: None,
        operations: vec![EditOperation::InsertText { text: text.into() }],
    }
}

fn indent_control(levels: i8) -> ParsedControl {
    ParsedControl {
        visible_text: None,
        operations: vec![EditOperation::ChangeIndent { levels }],
    }
}

fn format_control(formatting: Formatting) -> ParsedControl {
    ParsedControl {
        visible_text: None,
        operations: vec![EditOperation::FormatLastUtterance {
            expected: None,
            formatting,
        }],
    }
}

fn render_spoken(raw: &str, mode: DictationMode) -> String {
    match mode {
        DictationMode::Literal => render_literal(raw),
        DictationMode::Natural => render_vocabulary(raw, false),
        DictationMode::Code => render_vocabulary(raw, true),
    }
}

fn render_literal(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.to_ascii_lowercase().starts_with("spell ") {
        let spelling = &trimmed["spell ".len()..];
        let letters = spelling
            .split_whitespace()
            .filter_map(|part| {
                let mut characters = part.chars();
                let first = characters.next()?;
                characters.next().is_none().then_some(first)
            })
            .collect::<String>();
        if !letters.is_empty() {
            return letters;
        }
    }
    trimmed.into()
}

fn render_vocabulary(raw: &str, code: bool) -> String {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();
    if code {
        if lower.starts_with("spell ") {
            return render_literal(trimmed);
        }
        if lower == "indent" {
            return "    ".into();
        }
        if lower == "dedent" || lower == "outdent" {
            return String::new();
        }
    }
    let vocabulary: &[(&str, &str)] = if code {
        &[
            ("open parenthesis", "("),
            ("close parenthesis", ")"),
            ("open bracket", "["),
            ("close bracket", "]"),
            ("open brace", "{"),
            ("close brace", "}"),
            ("double quote", "\""),
            ("single quote", "'"),
            ("equals", "="),
            ("plus", "+"),
            ("minus", "-"),
            ("slash", "/"),
            ("backslash", "\\"),
            ("semicolon", ";"),
            ("colon", ":"),
            ("comma", ","),
            ("dot", "."),
            ("new line", "\n"),
            ("newline", "\n"),
            ("indent", "    "),
            ("tab", "    "),
        ]
    } else {
        &[
            ("new paragraph", "\n\n"),
            ("new line", "\n"),
            ("newline", "\n"),
            ("question mark", "?"),
            ("exclamation mark", "!"),
            ("full stop", "."),
            ("period", "."),
            ("comma", ","),
            ("semicolon", ";"),
            ("colon", ":"),
        ]
    };
    replace_phrases(trimmed, vocabulary)
}

fn replace_phrases(input: &str, vocabulary: &[(&str, &str)]) -> String {
    let mut output = input.to_owned();
    for (spoken, rendered) in vocabulary {
        output = replace_whole_phrase(&output, spoken, &format!(" {rendered} "));
    }
    cleanup_spacing(&output)
}

fn replace_whole_phrase(input: &str, phrase: &str, replacement: &str) -> String {
    let lowered = input.to_ascii_lowercase();
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(relative) = lowered[cursor..].find(phrase) {
        let start = cursor + relative;
        let end = start + phrase.len();
        let before_is_boundary = start == 0
            || !input[..start]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric);
        let after_is_boundary = end == input.len()
            || !input[end..]
                .chars()
                .next()
                .is_some_and(char::is_alphanumeric);
        if before_is_boundary && after_is_boundary {
            output.push_str(&input[cursor..start]);
            output.push_str(replacement);
            cursor = end;
        } else {
            output.push_str(&input[cursor..end]);
            cursor = end;
        }
    }
    output.push_str(&input[cursor..]);
    output
}

fn cleanup_spacing(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut prior_space = false;
    for character in input.chars() {
        if character == '\n' {
            while output.ends_with(' ') {
                output.pop();
            }
            output.push(character);
            prior_space = false;
        } else if character.is_whitespace() {
            if !prior_space && !output.ends_with('\n') {
                output.push(' ');
                prior_space = true;
            }
        } else {
            if matches!(character, '.' | ',' | '?' | '!' | ':' | ';') && output.ends_with(' ') {
                output.pop();
            }
            output.push(character);
            prior_space = false;
        }
    }
    output.trim().to_owned()
}

fn tokenize(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

/// Fast commands handled without an agent/model round trip.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum DeterministicCommand {
    LaunchApplication { name: String },
    FocusApplication { name: String },
    Stop,
    Mute,
    Unmute,
    Sleep,
    Wake,
    StartDictation,
    StopDictation,
    SetDictationMode { mode: DictationMode },
}

/// Intent context supplied by the active voice state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteContext {
    Auto,
    Command,
    Dictation,
}

/// Voice routing outcome. Agent goals preserve the original user wording.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "route")]
pub enum VoiceRoute {
    Commands { commands: Vec<DeterministicCommand> },
    AgentGoal { prompt: String },
    Dictation { text: String },
}

/// Separates predictable device/app controls from model-planned goals.
#[derive(Clone, Debug, Default)]
pub struct CommandRouter;

impl CommandRouter {
    #[must_use]
    pub fn route(&self, transcript: &str, context: RouteContext) -> VoiceRoute {
        let text = transcript.trim();
        let segments = split_command_chain(text);
        let commands = segments
            .iter()
            .filter_map(|segment| parse_deterministic(segment, context == RouteContext::Command))
            .collect::<Vec<_>>();
        let dictation_safe_commands = commands.iter().all(|command| {
            matches!(
                command,
                DeterministicCommand::Stop
                    | DeterministicCommand::StopDictation
                    | DeterministicCommand::SetDictationMode { .. }
            )
        });
        if !commands.is_empty()
            && commands.len() == segments.len()
            && (context != RouteContext::Dictation || dictation_safe_commands)
        {
            return VoiceRoute::Commands { commands };
        }
        if context == RouteContext::Dictation {
            return VoiceRoute::Dictation { text: text.into() };
        }
        let looks_like_goal = context == RouteContext::Command
            || starts_with_any(
                &text.to_lowercase(),
                &[
                    "find ",
                    "create ",
                    "write ",
                    "draft ",
                    "send ",
                    "run ",
                    "explain ",
                    "look up ",
                    "search ",
                    "read ",
                    "summarize ",
                    "check ",
                    "fix ",
                    "build ",
                ],
            );
        if looks_like_goal {
            VoiceRoute::AgentGoal {
                prompt: text.into(),
            }
        } else {
            VoiceRoute::Dictation { text: text.into() }
        }
    }
}

fn split_command_chain(text: &str) -> Vec<&str> {
    text.split(" and then ")
        .flat_map(|part| part.split(", then "))
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect()
}

fn parse_deterministic(segment: &str, allow_bare_open: bool) -> Option<DeterministicCommand> {
    let normalized = segment
        .trim()
        .trim_matches(|character: char| character.is_ascii_punctuation())
        .to_lowercase();
    if let Some(name) = normalized
        .strip_prefix("launch ")
        .or_else(|| normalized.strip_prefix("open app "))
        .or_else(|| {
            allow_bare_open
                .then(|| normalized.strip_prefix("open "))
                .flatten()
        })
        .filter(|name| !name.trim().is_empty())
    {
        return Some(DeterministicCommand::LaunchApplication {
            name: name.trim().into(),
        });
    }
    if let Some(name) = normalized
        .strip_prefix("focus ")
        .or_else(|| normalized.strip_prefix("switch to "))
        .filter(|name| !name.trim().is_empty())
    {
        return Some(DeterministicCommand::FocusApplication {
            name: name.trim().into(),
        });
    }
    match normalized.as_str() {
        "stop" | "cancel" | "stop listening" => Some(DeterministicCommand::Stop),
        "mute" | "mute microphone" => Some(DeterministicCommand::Mute),
        "unmute" | "unmute microphone" => Some(DeterministicCommand::Unmute),
        "sleep" | "go to sleep" => Some(DeterministicCommand::Sleep),
        "wake" | "wake up" => Some(DeterministicCommand::Wake),
        "dictate" | "start dictation" => Some(DeterministicCommand::StartDictation),
        "stop dictation" | "end dictation" => Some(DeterministicCommand::StopDictation),
        "natural mode" | "prose mode" => Some(DeterministicCommand::SetDictationMode {
            mode: DictationMode::Natural,
        }),
        "literal mode" => Some(DeterministicCommand::SetDictationMode {
            mode: DictationMode::Literal,
        }),
        "code mode" | "coding mode" => Some(DeterministicCommand::SetDictationMode {
            mode: DictationMode::Code,
        }),
        _ => None,
    }
}

fn starts_with_any(text: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| text.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn partial(text: &str, final_result: bool, at: u64) -> PartialTranscript {
        PartialTranscript {
            text: text.into(),
            final_result,
            audio_end_ms: Some(at.saturating_sub(40)),
            received_at_ms: at,
        }
    }

    #[test]
    fn streaming_partial_marks_repeated_prefix_stable_and_diffs_only_suffix() {
        let mut engine = DictationEngine::default();
        let first = engine
            .ingest(partial("hello wur", false, 100))
            .expect("first");
        assert!(
            first
                .tokens
                .iter()
                .all(|token| token.stability == TokenStability::Unstable)
        );
        let second = engine
            .ingest(partial("hello world", false, 150))
            .expect("second");
        assert_eq!(second.tokens[0].stability, TokenStability::Stable);
        assert_eq!(second.tokens[1].stability, TokenStability::Unstable);
        assert_eq!(
            second.operations,
            vec![EditOperation::ReplaceProvisionalTail {
                transaction_id: 1,
                retain_utf16: 7,
                delete_utf16: 2,
                insert: "orld".into(),
                stable_prefix_utf16: 6,
            }]
        );
    }

    #[test]
    fn final_result_commits_and_next_partial_uses_new_transaction() {
        let mut engine = DictationEngine::default();
        let final_update = engine
            .ingest(partial("hello world period", true, 100))
            .expect("final");
        assert_eq!(final_update.rendered_text, "hello world.");
        assert!(matches!(
            final_update.operations.last(),
            Some(EditOperation::CommitTransaction { transaction_id: 1 })
        ));
        let next = engine.ingest(partial("next", false, 200)).expect("next");
        assert!(matches!(
            next.operations.first(),
            Some(EditOperation::ReplaceProvisionalTail {
                transaction_id: 2,
                ..
            })
        ));
    }

    #[test]
    fn endpointed_utterances_get_spaces_except_after_layout_controls() {
        let mut engine = DictationEngine::default();
        engine
            .ingest(partial("Hello", true, 100))
            .expect("first utterance");
        let second = engine
            .ingest(partial("world", true, 200))
            .expect("second utterance");
        assert_eq!(second.rendered_text, "world");
        assert!(
            second
                .operations
                .contains(&EditOperation::ReplaceProvisionalTail {
                    transaction_id: 2,
                    retain_utf16: 0,
                    delete_utf16: 0,
                    insert: " world".into(),
                    stable_prefix_utf16: 6,
                })
        );
        engine
            .ingest(partial("new paragraph", true, 300))
            .expect("paragraph");
        let after_paragraph = engine
            .ingest(partial("Next", true, 400))
            .expect("after paragraph");
        assert!(
            after_paragraph
                .operations
                .contains(&EditOperation::ReplaceProvisionalTail {
                    transaction_id: 4,
                    retain_utf16: 0,
                    delete_utf16: 0,
                    insert: "Next".into(),
                    stable_prefix_utf16: 4,
                })
        );
    }

    #[test]
    fn correction_grammar_covers_delete_undo_replace_and_relative_insert() {
        let mut engine = DictationEngine::default();
        engine
            .ingest(partial("wrong phrase", true, 100))
            .expect("commit");
        let deleted = engine
            .ingest(partial("scratch that", true, 200))
            .expect("delete");
        assert!(
            deleted
                .operations
                .contains(&EditOperation::DeleteLastUtterance {
                    expected: "wrong phrase".into(),
                    utf16_len: 12,
                })
        );
        let replaced = engine
            .ingest(partial("replace color with colour", true, 300))
            .expect("replace");
        assert!(
            replaced
                .operations
                .contains(&EditOperation::ReplaceLiteral {
                    find: "color".into(),
                    replacement: "colour".into(),
                    occurrence: Occurrence::Last,
                })
        );
        let inserted = engine
            .ingest(partial("insert very before important", true, 400))
            .expect("insert");
        assert!(
            inserted
                .operations
                .contains(&EditOperation::InsertRelative {
                    anchor: "important".into(),
                    text: "very".into(),
                    before: true,
                    occurrence: Occurrence::Last,
                })
        );
        let undone = engine.ingest(partial("undo", true, 500)).expect("undo");
        assert!(undone.operations.contains(&EditOperation::Undo));
    }

    #[test]
    fn formatting_and_layout_commands_are_semantic_operations() {
        let mut engine = DictationEngine::default();
        for (spoken, expected) in [
            (
                "new paragraph",
                EditOperation::InsertText {
                    text: "\n\n".into(),
                },
            ),
            (
                "bold that",
                EditOperation::FormatLastUtterance {
                    expected: None,
                    formatting: Formatting::Bold,
                },
            ),
            (
                "heading two",
                EditOperation::FormatLastUtterance {
                    expected: None,
                    formatting: Formatting::Heading { level: 2 },
                },
            ),
            (
                "bullets",
                EditOperation::FormatLastUtterance {
                    expected: None,
                    formatting: Formatting::BulletedList,
                },
            ),
            ("indent", EditOperation::ChangeIndent { levels: 1 }),
        ] {
            let update = engine.ingest(partial(spoken, true, 100)).expect("format");
            assert!(
                update.operations.contains(&expected),
                "{spoken}: {:?}",
                update.operations
            );
        }
    }

    #[test]
    fn literal_spelling_and_code_vocabulary_are_deterministic() {
        let mut literal = DictationEngine::new(DictationMode::Literal);
        assert_eq!(
            literal
                .ingest(partial("spell R u s t", true, 100))
                .expect("spell")
                .rendered_text,
            "Rust"
        );
        let mut code = DictationEngine::new(DictationMode::Code);
        assert_eq!(
            code.ingest(partial(
                "call open parenthesis value close parenthesis semicolon",
                true,
                100
            ))
            .expect("code")
            .rendered_text,
            "call ( value );"
        );
        assert_eq!(
            code.ingest(partial("spell A P I", true, 200))
                .expect("code spelling")
                .rendered_text,
            "API"
        );
    }

    #[test]
    fn prose_and_corrections_preserve_proper_name_case_and_word_boundaries() {
        let mut engine = DictationEngine::default();
        let prose = engine
            .ingest(partial(
                "Email Yuval comma about the periodic job",
                true,
                100,
            ))
            .expect("prose");
        assert_eq!(prose.rendered_text, "Email Yuval, about the periodic job");
        let correction = engine
            .ingest(partial("replace GitHub with GitLab", true, 200))
            .expect("correction");
        assert!(
            correction
                .operations
                .contains(&EditOperation::ReplaceLiteral {
                    find: "GitHub".into(),
                    replacement: "GitLab".into(),
                    occurrence: Occurrence::Last,
                })
        );
    }

    #[test]
    fn spoken_mode_switch_affects_following_utterance_without_inserting_control() {
        let mut engine = DictationEngine::default();
        let mode = engine
            .ingest(partial("code mode", true, 100))
            .expect("mode");
        assert_eq!(engine.mode(), DictationMode::Code);
        assert!(mode.operations.contains(&EditOperation::SetMode {
            mode: DictationMode::Code
        }));
        assert!(
            !mode
                .operations
                .iter()
                .any(|operation| matches!(operation, EditOperation::CommitTransaction { .. }))
        );
        let code = engine
            .ingest(partial("x equals one semicolon", true, 200))
            .expect("code");
        assert_eq!(code.rendered_text, "x = one;");
    }

    #[test]
    fn planner_reports_exact_and_degraded_target_strategies() {
        let direct = TargetCapabilities {
            provisional_replacement: true,
            text_search: true,
            undo: true,
            rich_text: true,
            multiline: true,
            direct_text_api: true,
            ..TargetCapabilities::default()
        };
        assert_eq!(
            plan_edit(&EditOperation::Undo, &direct).strategy,
            EditStrategy::DirectTextApi
        );
        let markdown = TargetCapabilities {
            markdown: true,
            clipboard_paste: true,
            keystrokes: true,
            ..TargetCapabilities::default()
        };
        let plan = plan_edit(
            &EditOperation::FormatLastUtterance {
                expected: None,
                formatting: Formatting::Bold,
            },
            &markdown,
        );
        assert_eq!(plan.strategy, EditStrategy::ClipboardPaste);
        assert!(!plan.exact);
        assert_eq!(
            plan_edit(
                &EditOperation::SetMode {
                    mode: DictationMode::Code,
                },
                &TargetCapabilities::default(),
            )
            .strategy,
            EditStrategy::SessionState
        );
        assert_eq!(
            plan_edit(
                &EditOperation::ReplaceLiteral {
                    find: "a".into(),
                    replacement: "b".into(),
                    occurrence: Occurrence::Last,
                },
                &TargetCapabilities {
                    keystrokes: true,
                    ..TargetCapabilities::default()
                }
            )
            .strategy,
            EditStrategy::Unsupported
        );
    }

    #[test]
    fn command_router_separates_fast_commands_agent_goals_and_dictation() {
        let router = CommandRouter;
        assert_eq!(
            router.route("launch vscode and then focus terminal", RouteContext::Auto),
            VoiceRoute::Commands {
                commands: vec![
                    DeterministicCommand::LaunchApplication {
                        name: "vscode".into()
                    },
                    DeterministicCommand::FocusApplication {
                        name: "terminal".into()
                    },
                ]
            }
        );
        assert_eq!(
            router.route("run the tests and explain the failure", RouteContext::Auto),
            VoiceRoute::AgentGoal {
                prompt: "run the tests and explain the failure".into()
            }
        );
        assert_eq!(
            router.route("launch party next Friday", RouteContext::Dictation),
            VoiceRoute::Dictation {
                text: "launch party next Friday".into()
            }
        );
        assert_eq!(
            router.route("open Visual Studio Code", RouteContext::Command),
            VoiceRoute::Commands {
                commands: vec![DeterministicCommand::LaunchApplication {
                    name: "visual studio code".into(),
                }]
            }
        );
    }

    #[test]
    fn latency_metrics_report_first_partial_updates_final_and_apply() {
        let mut metrics = DictationLatencyMetrics::default();
        metrics.record_recognition(&PartialTranscript {
            text: "one".into(),
            final_result: false,
            audio_end_ms: Some(100),
            received_at_ms: 130,
        });
        metrics.record_recognition(&PartialTranscript {
            text: "one two".into(),
            final_result: false,
            audio_end_ms: Some(150),
            received_at_ms: 200,
        });
        metrics.record_recognition(&PartialTranscript {
            text: "one two".into(),
            final_result: true,
            audio_end_ms: Some(190),
            received_at_ms: 250,
        });
        metrics.record_apply(
            260,
            &EditReceipt {
                transaction_id: Some(1),
                applied_at_ms: 275,
                strategy: EditStrategy::DirectTextApi,
                verified: true,
            },
        );
        let report = metrics.report();
        assert_eq!(report.first_partial.p50_ms, 30);
        assert_eq!(report.partial_updates.p50_ms, 50);
        assert_eq!(report.finalization.p50_ms, 60);
        assert_eq!(report.target_apply.p50_ms, 15);
    }

    struct FakeTarget {
        verified: bool,
    }

    impl DictationTarget for FakeTarget {
        fn capabilities(&self) -> TargetCapabilities {
            TargetCapabilities {
                direct_text_api: true,
                ..TargetCapabilities::default()
            }
        }

        fn apply(&mut self, _: &EditOperation) -> Result<EditReceipt, DictationError> {
            Ok(EditReceipt {
                transaction_id: Some(1),
                applied_at_ms: 10,
                strategy: EditStrategy::DirectTextApi,
                verified: self.verified,
            })
        }
    }

    #[test]
    fn verified_apply_never_treats_an_unproven_edit_as_success() {
        let operation = EditOperation::InsertText {
            text: "hello".into(),
        };
        assert!(apply_verified(&mut FakeTarget { verified: true }, &operation).is_ok());
        assert!(matches!(
            apply_verified(&mut FakeTarget { verified: false }, &operation),
            Err(DictationError::Verification(_))
        ));
    }

    #[test]
    fn unicode_diff_counts_utf16_units() {
        let operation = diff_operation(7, "hello 👋", "hello 👋!", 8);
        assert_eq!(
            operation,
            EditOperation::ReplaceProvisionalTail {
                transaction_id: 7,
                retain_utf16: 8,
                delete_utf16: 0,
                insert: "!".into(),
                stable_prefix_utf16: 8,
            }
        );
    }
}
