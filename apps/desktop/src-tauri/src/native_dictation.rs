//! Explicitly armed dictation into the focused application.
//!
//! The engine intentionally defaults to review-before-insert. Native keystroke fallbacks cannot
//! read back an arbitrary Wayland field, so they are reported as unverified submissions instead
//! of pretending to be accessibility-backed edits. Every mutating operation re-checks the active
//! window identity and rejects secure surfaces and the Personal Agent window itself.

use personal_agent_audio::{DictationUpdate, EditOperation, Formatting, Occurrence};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeSet, VecDeque};
use std::env;
#[cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;

const MAX_NATIVE_TEXT_BYTES: usize = 16 * 1024;
const LAST_EDIT_UNDO_WINDOW_MS: u64 = 30_000;
const LATENCY_SAMPLES: usize = 128;
const MAX_CLIPBOARD_SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NativeAvailability {
    Degraded,
    PermissionRequired,
    Unavailable,
}

#[allow(clippy::struct_excessive_bools)] // The IPC contract exposes independent OS capabilities.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct NativeInputContract {
    pub platform: String,
    pub session: String,
    pub adapter: String,
    pub availability: NativeAvailability,
    pub review_before_insert: bool,
    pub supports_text_insertion: bool,
    pub supports_live_revisions: bool,
    pub supports_verified_edits: bool,
    pub detail: String,
    pub remediation: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct NativeDictationTarget {
    pub application_id: String,
    pub title: String,
    pub window_id: String,
    pub secure: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PendingEditKind {
    Insert,
    ReplaceLast,
    UndoLast,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct NativeDictationPending {
    pub transaction_id: u64,
    pub text: String,
    pub final_result: bool,
    pub kind: PendingEditKind,
    pub warning: Option<String>,
    pub preview_latency_ms: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) struct NativeDictationMetrics {
    pub last_apply_ms: Option<u64>,
    pub p95_apply_ms: Option<u64>,
    pub apply_samples: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct NativeDictationStatus {
    pub contract: NativeInputContract,
    pub armed_target: Option<NativeDictationTarget>,
    pub pending: Option<NativeDictationPending>,
    pub undo_available: bool,
    pub metrics: NativeDictationMetrics,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct NativeDictationApplyResult {
    pub submitted: bool,
    pub verified: bool,
    pub adapter: String,
    pub elapsed_ms: u64,
    pub detail: String,
    pub status: NativeDictationStatus,
}

#[derive(Clone, Debug)]
struct AppliedEdit {
    target: NativeDictationTarget,
    text: String,
    applied_at_ms: u64,
}

#[derive(Clone, Debug)]
enum NativeInputAdapter {
    HyprlandClipboard,
    Wtype,
    Ydotool { socket: PathBuf },
    Xdotool,
    ContractOnly(NativeInputContract),
}

impl NativeInputAdapter {
    fn contract(&self) -> NativeInputContract {
        match self {
            Self::HyprlandClipboard => NativeInputContract {
                platform: "linux".into(),
                session: "wayland".into(),
                adapter: "hyprland_clipboard".into(),
                availability: NativeAvailability::Degraded,
                review_before_insert: true,
                supports_text_insertion: true,
                supports_live_revisions: false,
                supports_verified_edits: false,
                detail: "Hyprland can send a paste shortcut to the armed window. Personal Agent snapshots and restores the existing clipboard after the reviewed insertion.".into(),
                remediation: Some(
                    "Install wtype or authorize an accessibility/RemoteDesktop adapter for clipboard-free editing and read-back verification."
                        .into(),
                ),
            },
            Self::Wtype => NativeInputContract {
                platform: "linux".into(),
                session: desktop_session(),
                adapter: "wtype".into(),
                availability: NativeAvailability::Degraded,
                review_before_insert: true,
                supports_text_insertion: true,
                supports_live_revisions: false,
                supports_verified_edits: false,
                detail: "Wayland virtual-keyboard text insertion is available. The target cannot be read back, so edits remain unverified.".into(),
                remediation: Some(
                    "For verified field edits, connect an accessibility/RemoteDesktop portal adapter."
                        .into(),
                ),
            },
            Self::Ydotool { socket } => NativeInputContract {
                platform: "linux".into(),
                session: desktop_session(),
                adapter: "ydotool".into(),
                availability: NativeAvailability::Degraded,
                review_before_insert: true,
                supports_text_insertion: true,
                supports_live_revisions: false,
                supports_verified_edits: false,
                detail: format!(
                    "The ydotool daemon socket {} is reachable. Input is submitted as unverified keystrokes.",
                    socket.display()
                ),
                remediation: Some(
                    "For verified field edits, connect an accessibility/RemoteDesktop portal adapter."
                        .into(),
                ),
            },
            Self::Xdotool => NativeInputContract {
                platform: "linux".into(),
                session: desktop_session(),
                adapter: "xdotool".into(),
                availability: NativeAvailability::Degraded,
                review_before_insert: true,
                supports_text_insertion: true,
                supports_live_revisions: false,
                supports_verified_edits: false,
                detail: "X11 keystroke insertion is available. The target cannot be read back, so edits remain unverified.".into(),
                remediation: Some("Use an AT-SPI adapter for verified field edits.".into()),
            },
            Self::ContractOnly(contract) => contract.clone(),
        }
    }

    async fn type_text(&self, text: &str, target: &NativeDictationTarget) -> Result<(), String> {
        validate_native_text(text)?;
        match self {
            Self::HyprlandClipboard => hyprland_paste(text, target).await,
            Self::Wtype => run("wtype", &["--", text], None).await,
            Self::Ydotool { socket } => run("ydotool", &["type", "--", text], Some(socket)).await,
            Self::Xdotool => run("xdotool", &["type", "--clearmodifiers", "--", text], None).await,
            Self::ContractOnly(contract) => Err(contract.detail.clone()),
        }
    }

    async fn undo(&self, target: &NativeDictationTarget) -> Result<(), String> {
        match self {
            Self::HyprlandClipboard => send_hypr_shortcut("CTRL", "Z", target).await,
            Self::Wtype => run("wtype", &["-M", "ctrl", "z", "-m", "ctrl"], None).await,
            Self::Ydotool { socket } => {
                // Linux input key codes: left-control=29, z=44.
                run(
                    "ydotool",
                    &["key", "29:1", "44:1", "44:0", "29:0"],
                    Some(socket),
                )
                .await
            }
            Self::Xdotool => run("xdotool", &["key", "--clearmodifiers", "ctrl+z"], None).await,
            Self::ContractOnly(contract) => Err(contract.detail.clone()),
        }
    }
}

pub(crate) struct NativeDictationSession {
    adapter: NativeInputAdapter,
    armed_target: Option<NativeDictationTarget>,
    pending: Option<NativeDictationPending>,
    last_applied: Option<AppliedEdit>,
    apply_latencies: VecDeque<u64>,
}

impl NativeDictationSession {
    #[must_use]
    pub(crate) fn discover() -> Self {
        Self {
            adapter: discover_adapter(),
            armed_target: None,
            pending: None,
            last_applied: None,
            apply_latencies: VecDeque::new(),
        }
    }

    #[must_use]
    pub(crate) fn status(&mut self) -> NativeDictationStatus {
        if self.armed_target.is_none() {
            self.adapter = discover_adapter();
        }
        let undo_available = self.last_applied.as_ref().is_some_and(|edit| {
            now_ms().saturating_sub(edit.applied_at_ms) <= LAST_EDIT_UNDO_WINDOW_MS
        });
        NativeDictationStatus {
            contract: self.adapter.contract(),
            armed_target: self.armed_target.clone(),
            pending: self.pending.clone(),
            undo_available,
            metrics: self.metrics(),
        }
    }

    pub(crate) async fn arm(&mut self, delay_ms: u64) -> Result<NativeDictationStatus, String> {
        self.adapter = discover_adapter();
        let contract = self.adapter.contract();
        if !contract.supports_text_insertion {
            return Err(contract.remediation.unwrap_or(contract.detail));
        }
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms.min(10_000))).await;
        let target = active_target().await?;
        validate_target(&target)?;
        self.armed_target = Some(target);
        self.pending = None;
        Ok(self.status())
    }

    pub(crate) fn disarm(&mut self) -> NativeDictationStatus {
        self.armed_target = None;
        self.pending = None;
        self.status()
    }

    pub(crate) async fn stage(
        &mut self,
        update: DictationUpdate,
    ) -> Result<NativeDictationStatus, String> {
        self.require_same_focused_target().await?;
        let preview_started = Instant::now();
        let mut pending = self.pending.take().unwrap_or(NativeDictationPending {
            transaction_id: update.transaction_id,
            text: String::new(),
            final_result: false,
            kind: PendingEditKind::Insert,
            warning: None,
            preview_latency_ms: 0,
        });
        if pending.transaction_id != update.transaction_id
            && update
                .operations
                .iter()
                .any(|operation| matches!(operation, EditOperation::ReplaceProvisionalTail { .. }))
        {
            pending = NativeDictationPending {
                transaction_id: update.transaction_id,
                text: String::new(),
                final_result: false,
                kind: PendingEditKind::Insert,
                warning: None,
                preview_latency_ms: 0,
            };
        }
        if update
            .operations
            .iter()
            .any(|operation| matches!(operation, EditOperation::ReplaceProvisionalTail { .. }))
        {
            pending.text.clone_from(&update.rendered_text);
            pending.kind = PendingEditKind::Insert;
        }
        for operation in &update.operations {
            apply_review_operation(&mut pending, operation, self.last_applied.as_ref())?;
        }
        pending.transaction_id = update.transaction_id;
        pending.final_result = update.final_result;
        pending.preview_latency_ms =
            u64::try_from(preview_started.elapsed().as_millis()).unwrap_or(u64::MAX);
        validate_native_text_or_intent(&pending)?;
        self.pending = Some(pending);
        Ok(self.status())
    }

    pub(crate) fn discard(&mut self) -> NativeDictationStatus {
        self.pending = None;
        self.status()
    }

    pub(crate) async fn confirm(
        &mut self,
        confirmed: bool,
        delay_ms: u64,
    ) -> Result<NativeDictationApplyResult, String> {
        if !confirmed {
            return Err("focused-app dictation requires an explicit Apply confirmation".into());
        }
        let pending = self
            .pending
            .clone()
            .ok_or_else(|| "there is no reviewed dictation to apply".to_owned())?;
        if !pending.final_result {
            return Err("wait for the final English transcript before applying it".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms.min(10_000))).await;
        let target = self.require_same_focused_target().await?;
        let started = Instant::now();
        match pending.kind {
            PendingEditKind::Insert => self.adapter.type_text(&pending.text, &target).await?,
            PendingEditKind::ReplaceLast => {
                self.require_recent_last_edit(&target)?;
                self.adapter.undo(&target).await?;
                self.adapter.type_text(&pending.text, &target).await?;
            }
            PendingEditKind::UndoLast => {
                self.require_recent_last_edit(&target)?;
                self.adapter.undo(&target).await?;
            }
        }
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.record_apply_latency(elapsed_ms);
        if pending.kind == PendingEditKind::UndoLast {
            self.last_applied = None;
        } else {
            self.last_applied = Some(AppliedEdit {
                target,
                text: pending.text,
                applied_at_ms: now_ms(),
            });
        }
        self.pending = None;
        Ok(NativeDictationApplyResult {
            submitted: true,
            verified: false,
            adapter: self.adapter.contract().adapter,
            elapsed_ms,
            detail: "Text was submitted to the unchanged focused target. This keystroke adapter cannot read the field back, so verify it visually before continuing.".into(),
            status: self.status(),
        })
    }

    pub(crate) async fn undo(
        &mut self,
        confirmed: bool,
        delay_ms: u64,
    ) -> Result<NativeDictationApplyResult, String> {
        if !confirmed {
            return Err("undo requires explicit confirmation".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms.min(10_000))).await;
        let target = self.require_same_focused_target().await?;
        self.require_recent_last_edit(&target)?;
        let started = Instant::now();
        self.adapter.undo(&target).await?;
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.record_apply_latency(elapsed_ms);
        self.last_applied = None;
        self.pending = None;
        Ok(NativeDictationApplyResult {
            submitted: true,
            verified: false,
            adapter: self.adapter.contract().adapter,
            elapsed_ms,
            detail: "Undo was submitted only to the unchanged target within the safety window. Verify the field visually.".into(),
            status: self.status(),
        })
    }

    async fn require_same_focused_target(&self) -> Result<NativeDictationTarget, String> {
        let armed = self
            .armed_target
            .as_ref()
            .ok_or_else(|| "arm a focused application before dictating".to_owned())?;
        let current = active_target().await?;
        validate_target(&current)?;
        if current.window_id != armed.window_id || current.application_id != armed.application_id {
            return Err(format!(
                "focus changed from {} to {}; re-arm the intended field before applying text",
                target_label(armed),
                target_label(&current)
            ));
        }
        Ok(current)
    }

    fn require_recent_last_edit(&self, target: &NativeDictationTarget) -> Result<(), String> {
        let edit = self
            .last_applied
            .as_ref()
            .ok_or_else(|| "there is no recent native dictation edit to undo".to_owned())?;
        if edit.target.window_id != target.window_id {
            return Err(
                "the last dictation belongs to a different window; undo was blocked".into(),
            );
        }
        if now_ms().saturating_sub(edit.applied_at_ms) > LAST_EDIT_UNDO_WINDOW_MS {
            return Err("the 30-second safe undo window expired".into());
        }
        Ok(())
    }

    fn record_apply_latency(&mut self, elapsed_ms: u64) {
        if self.apply_latencies.len() == LATENCY_SAMPLES {
            self.apply_latencies.pop_front();
        }
        self.apply_latencies.push_back(elapsed_ms);
    }

    fn metrics(&self) -> NativeDictationMetrics {
        let mut values = self.apply_latencies.iter().copied().collect::<Vec<_>>();
        values.sort_unstable();
        let p95 = (!values.is_empty()).then(|| {
            let index = (values.len() * 95).div_ceil(100).saturating_sub(1);
            values[index]
        });
        NativeDictationMetrics {
            last_apply_ms: self.apply_latencies.back().copied(),
            p95_apply_ms: p95,
            apply_samples: self.apply_latencies.len(),
        }
    }
}

fn apply_review_operation(
    pending: &mut NativeDictationPending,
    operation: &EditOperation,
    last_applied: Option<&AppliedEdit>,
) -> Result<(), String> {
    match operation {
        EditOperation::ReplaceProvisionalTail { .. }
        | EditOperation::CommitTransaction { .. }
        | EditOperation::SetMode { .. } => {}
        EditOperation::InsertText { text } => pending.text.push_str(text),
        EditOperation::ReplaceLiteral {
            find,
            replacement,
            occurrence,
        } => {
            prepare_last_edit_if_needed(pending, last_applied)?;
            pending.text = replace_occurrence(&pending.text, find, replacement, *occurrence)?;
        }
        EditOperation::InsertRelative {
            anchor,
            text,
            before,
            occurrence,
        } => {
            prepare_last_edit_if_needed(pending, last_applied)?;
            let replacement = if *before {
                format!("{text} {anchor}")
            } else {
                format!("{anchor} {text}")
            };
            pending.text = replace_occurrence(&pending.text, anchor, &replacement, *occurrence)?;
        }
        EditOperation::DeleteLastUtterance { .. } | EditOperation::Undo => {
            let last = last_applied
                .ok_or_else(|| "there is no recent dictated insertion to undo".to_owned())?;
            pending.text.clone_from(&last.text);
            pending.kind = PendingEditKind::UndoLast;
            pending.warning = Some(
                "Apply will send one native Undo to the same window within 30 seconds.".into(),
            );
        }
        EditOperation::FormatLastUtterance {
            expected,
            formatting,
        } => {
            prepare_last_edit_if_needed(pending, last_applied)?;
            let expected = expected.as_deref().unwrap_or(pending.text.as_str());
            let formatted = markdown_format(expected, formatting);
            pending.text =
                replace_occurrence(&pending.text, expected, &formatted, Occurrence::Last)?;
            pending.warning =
                Some("The focused-app fallback inserts plain-text Markdown formatting.".into());
        }
        EditOperation::ChangeIndent { levels } => {
            prepare_last_edit_if_needed(pending, last_applied)?;
            pending.text = change_last_line_indent(&pending.text, *levels);
        }
    }
    Ok(())
}

fn prepare_last_edit_if_needed(
    pending: &mut NativeDictationPending,
    last_applied: Option<&AppliedEdit>,
) -> Result<(), String> {
    if pending.text.is_empty() {
        let last = last_applied.ok_or_else(|| {
            "this correction needs readable document text; only the most recent dictated insertion can be safely revised"
                .to_owned()
        })?;
        pending.text.clone_from(&last.text);
        pending.kind = PendingEditKind::ReplaceLast;
        pending.warning = Some(
            "Apply will undo the last native insertion and type the reviewed correction. It is blocked if focus changed or the 30-second window expired."
                .into(),
        );
    }
    Ok(())
}

fn replace_occurrence(
    value: &str,
    find: &str,
    replacement: &str,
    occurrence: Occurrence,
) -> Result<String, String> {
    if find.is_empty() {
        return Err("a correction target cannot be empty".into());
    }
    let result = match occurrence {
        Occurrence::All => value.replace(find, replacement),
        Occurrence::First => value.replacen(find, replacement, 1),
        Occurrence::Last => value.rfind(find).map_or_else(
            || value.to_owned(),
            |start| {
                format!(
                    "{}{}{}",
                    &value[..start],
                    replacement,
                    &value[start + find.len()..]
                )
            },
        ),
    };
    if result == value {
        return Err(format!("the reviewed text does not contain {find:?}"));
    }
    Ok(result)
}

fn markdown_format(value: &str, formatting: &Formatting) -> String {
    match formatting {
        Formatting::Bold => format!("**{value}**"),
        Formatting::Heading { level } => {
            format!(
                "{} {}",
                "#".repeat(usize::from((*level).clamp(1, 6))),
                value.trim_start()
            )
        }
        Formatting::BulletedList => value
            .lines()
            .map(|line| format!("- {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
        Formatting::NumberedList => value
            .lines()
            .enumerate()
            .map(|(index, line)| format!("{}. {line}", index + 1))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn change_last_line_indent(value: &str, levels: i8) -> String {
    let line_start = value.rfind('\n').map_or(0, |index| index + 1);
    let (prefix, line) = value.split_at(line_start);
    if levels > 0 {
        format!(
            "{prefix}{}{line}",
            "    ".repeat(levels.unsigned_abs().into())
        )
    } else {
        let remove = line
            .chars()
            .take_while(|character| *character == ' ')
            .count()
            .min(usize::from(levels.unsigned_abs()) * 4);
        format!("{prefix}{}", &line[remove..])
    }
}

fn validate_native_text_or_intent(pending: &NativeDictationPending) -> Result<(), String> {
    if pending.kind == PendingEditKind::UndoLast {
        Ok(())
    } else {
        validate_native_text(&pending.text)
    }
}

fn validate_native_text(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("the reviewed transcript is empty".into());
    }
    if text.len() > MAX_NATIVE_TEXT_BYTES {
        return Err("one native dictation insertion is limited to 16 KiB".into());
    }
    if text
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    {
        return Err("the reviewed transcript contains unsupported control characters".into());
    }
    Ok(())
}

fn discover_adapter() -> NativeInputAdapter {
    let commands = [
        "hyprctl", "wl-copy", "wl-paste", "wtype", "ydotool", "xdotool",
    ]
    .into_iter()
    .filter(|command| executable(command))
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    discover_adapter_from(
        env::consts::OS,
        &desktop_session(),
        &commands,
        ydotool_socket(),
    )
}

fn discover_adapter_from(
    platform: &str,
    session: &str,
    commands: &BTreeSet<String>,
    ydotool_socket: Option<PathBuf>,
) -> NativeInputAdapter {
    match platform {
        "linux" => discover_linux_adapter(session, commands, ydotool_socket),
        "macos" => contract_only(
            "macos",
            "aqua",
            "ax-cgevent-contract",
            NativeAvailability::PermissionRequired,
            "The signed AXUIElement/CGEvent helper is not connected; Apple Events is not treated as proof of Accessibility permission.",
            Some(
                "Install the signed native helper, then grant Personal Agent Accessibility permission in System Settings.",
            ),
        ),
        "windows" => contract_only(
            "windows",
            "desktop",
            "uia-sendinput-contract",
            NativeAvailability::PermissionRequired,
            "The signed UI Automation/SendInput helper is not connected and integrity-level compatibility is unknown.",
            Some(
                "Install the signed Windows helper and run it at the same integrity level as the target application.",
            ),
        ),
        other => contract_only(
            other,
            session,
            "none",
            NativeAvailability::Unavailable,
            &format!("Focused-app dictation is not implemented for {other}."),
            None,
        ),
    }
}

fn discover_linux_adapter(
    session: &str,
    commands: &BTreeSet<String>,
    ydotool_socket: Option<PathBuf>,
) -> NativeInputAdapter {
    let wayland = session == "wayland";
    if wayland && commands.contains("wtype") {
        return NativeInputAdapter::Wtype;
    }
    if wayland
        && ["hyprctl", "wl-copy", "wl-paste"]
            .iter()
            .all(|command| commands.contains(*command))
    {
        return NativeInputAdapter::HyprlandClipboard;
    }
    if commands.contains("ydotool")
        && let Some(socket) = ydotool_socket
    {
        return NativeInputAdapter::Ydotool { socket };
    }
    if !wayland && commands.contains("xdotool") {
        return NativeInputAdapter::Xdotool;
    }
    if wayland && commands.contains("ydotool") {
        return contract_only(
            "linux",
            session,
            "ydotool",
            NativeAvailability::PermissionRequired,
            "ydotool is installed, but its user daemon socket is not reachable; no text will be injected.",
            Some(
                "Enable a user ydotoold service and grant access to its socket, or install wtype.",
            ),
        );
    }
    contract_only(
        "linux",
        session,
        if wayland {
            "xdg-remote-desktop-contract"
        } else {
            "at-spi-contract"
        },
        NativeAvailability::Unavailable,
        "No authorized native text-input adapter is connected.",
        Some(if wayland {
            "Install wtype, or authorize an XDG RemoteDesktop portal session. ydotool is usable only while its daemon socket is reachable."
        } else {
            "Install xdotool or connect an AT-SPI editable-text adapter."
        }),
    )
}

fn contract_only(
    platform: &str,
    session: &str,
    adapter: &str,
    availability: NativeAvailability,
    detail: &str,
    remediation: Option<&str>,
) -> NativeInputAdapter {
    NativeInputAdapter::ContractOnly(NativeInputContract {
        platform: platform.into(),
        session: session.into(),
        adapter: adapter.into(),
        availability,
        review_before_insert: true,
        supports_text_insertion: false,
        supports_live_revisions: false,
        supports_verified_edits: false,
        detail: detail.into(),
        remediation: remediation.map(str::to_owned),
    })
}

fn desktop_session() -> String {
    env::var("XDG_SESSION_TYPE")
        .unwrap_or_else(|_| {
            if env::var_os("WAYLAND_DISPLAY").is_some() {
                "wayland"
            } else {
                "x11"
            }
            .into()
        })
        .to_ascii_lowercase()
}

fn ydotool_socket() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = env::var_os("YDOTOOL_SOCKET") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(directory) = env::var_os("XDG_RUNTIME_DIR") {
        candidates.push(PathBuf::from(directory).join(".ydotool_socket"));
    }
    candidates.push(PathBuf::from("/tmp/.ydotool_socket"));
    candidates.into_iter().find(|path| is_socket(path))
}

#[cfg(unix)]
fn is_socket(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt as _;
    fs::metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket())
}

#[cfg(not(unix))]
fn is_socket(_path: &Path) -> bool {
    false
}

fn executable(program: &str) -> bool {
    env::var_os("PATH").is_some_and(|paths| {
        env::split_paths(&paths).any(|directory| Path::new(&directory).join(program).is_file())
    })
}

async fn run(program: &str, args: &[&str], socket: Option<&Path>) -> Result<(), String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(socket) = socket {
        command.env("YDOTOOL_SOCKET", socket);
    }
    let output = command.output().await.map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

struct ClipboardSnapshot {
    mime: Option<String>,
    bytes: Vec<u8>,
}

impl ClipboardSnapshot {
    async fn capture() -> Result<Self, String> {
        let types = command_output("wl-paste", &["--list-types"], None).await;
        let types = match types {
            Ok(output) => String::from_utf8_lossy(&output).to_string(),
            Err(error)
                if error.contains("No selection")
                    || error.contains("nothing is copied")
                    || error.contains("no clipboard") =>
            {
                return Ok(Self {
                    mime: None,
                    bytes: Vec::new(),
                });
            }
            Err(error) => return Err(format!("the clipboard could not be snapshotted: {error}")),
        };
        let offered = types
            .lines()
            .filter(|value| !value.trim().is_empty())
            .collect::<Vec<_>>();
        if offered.is_empty() {
            return Ok(Self {
                mime: None,
                bytes: Vec::new(),
            });
        }
        let mime = offered
            .iter()
            .copied()
            .find(|value| *value == "text/plain;charset=utf-8")
            .or_else(|| offered.iter().copied().find(|value| *value == "text/plain"))
            .unwrap_or(offered[0])
            .to_owned();
        let bytes = command_output("wl-paste", &["--type", &mime], None).await?;
        if bytes.len() > MAX_CLIPBOARD_SNAPSHOT_BYTES {
            return Err(
                "the current clipboard is larger than 32 MiB; dictation refused to overwrite it"
                    .into(),
            );
        }
        Ok(Self {
            mime: Some(mime),
            bytes,
        })
    }

    async fn restore(self) -> Result<(), String> {
        if let Some(mime) = self.mime {
            command_with_stdin("wl-copy", &["--type", &mime], &self.bytes).await
        } else {
            run("wl-copy", &["--clear"], None).await
        }
    }
}

async fn hyprland_paste(text: &str, target: &NativeDictationTarget) -> Result<(), String> {
    let clipboard = ClipboardSnapshot::capture().await?;
    command_with_stdin(
        "wl-copy",
        &["--type", "text/plain;charset=utf-8"],
        text.as_bytes(),
    )
    .await?;
    let paste = send_hypr_shortcut("CTRL", "V", target).await;
    // Give the destination client a bounded window to request clipboard data before restoration.
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    let restoration = clipboard.restore().await;
    match (paste, restoration) {
        (Err(paste), Err(restoration)) => Err(format!(
            "paste failed ({paste}) and the clipboard could not be restored ({restoration})"
        )),
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

async fn send_hypr_shortcut(
    modifiers: &str,
    key: &str,
    target: &NativeDictationTarget,
) -> Result<(), String> {
    let address = target.window_id.trim();
    let hexadecimal = address
        .strip_prefix("0x")
        .filter(|value| !value.is_empty())
        .is_some_and(|value| value.chars().all(|character| character.is_ascii_hexdigit()));
    if !hexadecimal {
        return Err("Hyprland returned an invalid focused-window address".into());
    }
    let expression = format!(
        "hl.dsp.send_shortcut({{ mods = \"{modifiers}\", key = \"{key}\", window = \"address:{address}\" }})"
    );
    let output = command_capture("hyprctl", &["dispatch", &expression], None).await?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if text.contains("warning:") || text.contains("error:") {
        Err(text.trim().to_owned())
    } else {
        Ok(())
    }
}

async fn command_with_stdin(program: &str, args: &[&str], bytes: &[u8]) -> Result<(), String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| format!("{program} did not expose standard input"))?;
    stdin
        .write_all(bytes)
        .await
        .map_err(|error| error.to_string())?;
    drop(stdin);
    let output = child
        .wait_with_output()
        .await
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

async fn command_output(
    program: &str,
    args: &[&str],
    socket: Option<&Path>,
) -> Result<Vec<u8>, String> {
    Ok(command_capture(program, args, socket).await?.stdout)
}

struct CommandCapture {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

async fn command_capture(
    program: &str,
    args: &[&str],
    socket: Option<&Path>,
) -> Result<CommandCapture, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(socket) = socket {
        command.env("YDOTOOL_SOCKET", socket);
    }
    let output = command.output().await.map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(CommandCapture {
            stdout: output.stdout,
            stderr: output.stderr,
        })
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

async fn active_target() -> Result<NativeDictationTarget, String> {
    match env::consts::OS {
        "linux" if executable("hyprctl") => {
            let output = Command::new("hyprctl")
                .args(["activewindow", "-j"])
                .stdin(Stdio::null())
                .output()
                .await
                .map_err(|error| error.to_string())?;
            if !output.status.success() {
                return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
            }
            let value: Value = serde_json::from_slice(&output.stdout)
                .map_err(|error| format!("invalid Hyprland active-window response: {error}"))?;
            let application_id = value
                .get("class")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned();
            let title = value
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let window_id = value
                .get("address")
                .and_then(Value::as_str)
                .ok_or_else(|| "Hyprland did not report an active window address".to_owned())?
                .to_owned();
            Ok(NativeDictationTarget {
                secure: secure_title(&title),
                application_id,
                title,
                window_id,
            })
        }
        "linux" if executable("xdotool") => {
            let id = output_text("xdotool", &["getactivewindow"]).await?;
            let title = output_text("xdotool", &["getwindowname", id.trim()]).await?;
            Ok(NativeDictationTarget {
                application_id: "x11-window".into(),
                title: title.trim().into(),
                window_id: id.trim().into(),
                secure: secure_title(&title),
            })
        }
        platform => Err(format!(
            "active focused-target observation is not connected on {platform}"
        )),
    }
}

async fn output_text(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

fn validate_target(target: &NativeDictationTarget) -> Result<(), String> {
    let identity = format!("{} {}", target.application_id, target.title).to_ascii_lowercase();
    if identity.contains("personal-agent") || identity.contains("personal agent") {
        return Err(
            "the Personal Agent window cannot be armed as an external target; switch to the destination application during the countdown"
                .into(),
        );
    }
    if target.secure {
        return Err(
            "dictation is blocked on password, credential, authentication, and lock surfaces"
                .into(),
        );
    }
    if target.title.trim().is_empty() {
        return Err("the focused target has no stable title; arming was blocked".into());
    }
    Ok(())
}

fn secure_title(title: &str) -> bool {
    let title = title.to_ascii_lowercase();
    [
        "password",
        "passphrase",
        "keychain",
        "credential",
        "authentication",
        "sign in",
        "login",
        "lock screen",
    ]
    .iter()
    .any(|word| title.contains(word))
}

fn target_label(target: &NativeDictationTarget) -> String {
    format!("{} ({})", target.title, target.application_id)
}

fn now_ms() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn installed_ydotool_without_daemon_is_not_reported_ready() {
        let adapter = discover_adapter_from("linux", "wayland", &commands(&["ydotool"]), None);
        let contract = adapter.contract();
        assert_eq!(
            contract.availability,
            NativeAvailability::PermissionRequired
        );
        assert!(!contract.supports_text_insertion);
        assert!(contract.detail.contains("socket is not reachable"));
    }

    #[test]
    fn wayland_prefers_wtype_and_xdotool_is_not_a_wayland_fallback() {
        let adapter =
            discover_adapter_from("linux", "wayland", &commands(&["wtype", "xdotool"]), None);
        assert_eq!(adapter.contract().adapter, "wtype");
        let x11_only = discover_adapter_from("linux", "wayland", &commands(&["xdotool"]), None);
        assert!(!x11_only.contract().supports_text_insertion);
    }

    #[test]
    fn current_hyprland_clipboard_fallback_is_explicit_and_review_only() {
        let adapter = discover_adapter_from(
            "linux",
            "wayland",
            &commands(&["hyprctl", "wl-copy", "wl-paste", "ydotool"]),
            None,
        );
        let contract = adapter.contract();
        assert_eq!(contract.adapter, "hyprland_clipboard");
        assert!(contract.supports_text_insertion);
        assert!(!contract.supports_live_revisions);
        assert!(!contract.supports_verified_edits);
        assert!(contract.detail.contains("restores the existing clipboard"));
    }

    #[test]
    fn macos_and_windows_contracts_do_not_pretend_permission_is_granted() {
        for platform in ["macos", "windows"] {
            let contract =
                discover_adapter_from(platform, "desktop", &BTreeSet::new(), None).contract();
            assert_eq!(
                contract.availability,
                NativeAvailability::PermissionRequired
            );
            assert!(!contract.supports_text_insertion);
            assert!(!contract.supports_verified_edits);
        }
    }

    #[test]
    fn staged_corrections_are_reviewed_against_only_the_last_own_edit() {
        let target = NativeDictationTarget {
            application_id: "editor".into(),
            title: "Draft".into(),
            window_id: "one".into(),
            secure: false,
        };
        let last = AppliedEdit {
            target,
            text: "hello color".into(),
            applied_at_ms: now_ms(),
        };
        let mut pending = NativeDictationPending {
            transaction_id: 2,
            text: String::new(),
            final_result: true,
            kind: PendingEditKind::Insert,
            warning: None,
            preview_latency_ms: 0,
        };
        apply_review_operation(
            &mut pending,
            &EditOperation::ReplaceLiteral {
                find: "color".into(),
                replacement: "colour".into(),
                occurrence: Occurrence::Last,
            },
            Some(&last),
        )
        .unwrap();
        assert_eq!(pending.text, "hello colour");
        assert_eq!(pending.kind, PendingEditKind::ReplaceLast);
        assert!(pending.warning.is_some());
    }

    #[test]
    fn secure_and_agent_windows_are_rejected() {
        let secure = NativeDictationTarget {
            application_id: "browser".into(),
            title: "Password login".into(),
            window_id: "one".into(),
            secure: true,
        };
        assert!(validate_target(&secure).is_err());
        let own = NativeDictationTarget {
            application_id: "personal-agent-desktop".into(),
            title: "Personal Agent".into(),
            window_id: "two".into(),
            secure: false,
        };
        assert!(validate_target(&own).is_err());
    }

    #[test]
    fn text_validation_allows_english_layout_but_blocks_control_input() {
        assert!(validate_native_text("Heading\n\tEnglish text.").is_ok());
        assert!(validate_native_text("unsafe\u{0}text").is_err());
    }
}
