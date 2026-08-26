//! Deterministic conversation controls shared by typed, voice, and project UI.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

/// Why a user message entered the conversation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputModality {
    Typed,
    Voice,
}

/// Visible microphone privacy state. It never has an ambiguous "on" value.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MicrophonePrivacy {
    Inactive,
    WakeWordOnly,
    CapturingSpeech,
}

/// Configured behavior while no explicit push-to-talk capture is active.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListeningMode {
    WakeOnly,
    Hybrid,
    Continuous,
}

/// Explicit state for independent persistent conversation controls.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlState {
    Disabled,
    Enabled,
}

impl ControlState {
    fn enabled(self) -> bool {
        self == Self::Enabled
    }
}

/// Provider selection remains independent from project conversation state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModelSelection {
    pub provider: String,
    pub model: String,
    pub effort: Option<String>,
}

/// One preserved general/project context.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConversationContext {
    pub session_id: Option<String>,
    pub turns: u64,
}

/// Accepted dispatch parameters. Typed dispatches are always silent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MessageDispatch {
    pub text: String,
    pub modality: InputModality,
    pub context: String,
    pub history_scope: String,
    pub speak_response: bool,
    pub restricted_tools: bool,
}

/// A transient stop result; stop does not silently change persistent modes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StopEffects {
    pub abort_foreground: bool,
    pub stop_playback: bool,
    pub clear_follow_up_capture: bool,
}

/// Conversation control failure with no hidden fallback.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ConversationError {
    #[error("message must not be blank")]
    BlankMessage,
    #[error("voice input is unavailable while muted")]
    Muted,
    #[error("voice input is unavailable while asleep; use a wake phrase first")]
    Asleep,
    #[error("project, persona, provider, and model names must not be blank")]
    BlankSelection,
}

/// Durable control state. Context sessions survive project/persona/model switches.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConversationState {
    pub listening_mode: ListeningMode,
    pub microphone: MicrophonePrivacy,
    pub sleeping: ControlState,
    pub muted: ControlState,
    pub quiet: ControlState,
    pub guest: ControlState,
    pub stay_open: ControlState,
    pub active_project: Option<String>,
    pub persona: String,
    pub model: Option<ModelSelection>,
    pub contexts: BTreeMap<String, ConversationContext>,
}

impl Default for ConversationState {
    fn default() -> Self {
        Self {
            listening_mode: ListeningMode::WakeOnly,
            microphone: MicrophonePrivacy::WakeWordOnly,
            sleeping: ControlState::Disabled,
            muted: ControlState::Disabled,
            quiet: ControlState::Disabled,
            guest: ControlState::Disabled,
            stay_open: ControlState::Disabled,
            active_project: None,
            persona: "JARVIS".into(),
            model: None,
            contexts: BTreeMap::from([("general".into(), ConversationContext::default())]),
        }
    }
}

impl ConversationState {
    fn context_key(&self) -> String {
        self.active_project
            .as_ref()
            .map_or_else(|| "general".into(), |project| format!("project:{project}"))
    }

    fn idle_microphone(&self) -> MicrophonePrivacy {
        if self.muted.enabled() {
            MicrophonePrivacy::Inactive
        } else if self.sleeping.enabled() || self.listening_mode != ListeningMode::Continuous {
            MicrophonePrivacy::WakeWordOnly
        } else {
            MicrophonePrivacy::CapturingSpeech
        }
    }

    fn dispatch(
        &mut self,
        text: &str,
        modality: InputModality,
    ) -> Result<MessageDispatch, ConversationError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(ConversationError::BlankMessage);
        }
        let context = self.context_key();
        self.contexts.entry(context.clone()).or_default().turns += 1;
        Ok(MessageDispatch {
            text: text.into(),
            modality,
            context,
            history_scope: if self.guest.enabled() {
                "guest"
            } else {
                "owner"
            }
            .into(),
            speak_response: modality == InputModality::Voice && !self.quiet.enabled(),
            restricted_tools: self.guest.enabled(),
        })
    }

    /// Submit typed input without opening the microphone or enabling speech.
    ///
    /// # Errors
    ///
    /// Returns `BlankMessage` for whitespace-only input.
    pub fn submit_typed(&mut self, text: &str) -> Result<MessageDispatch, ConversationError> {
        self.dispatch(text, InputModality::Typed)
    }

    /// Submit voice input only when the persistent privacy controls permit it.
    ///
    /// # Errors
    ///
    /// Returns an error for blank input, mute, or sleep state.
    pub fn submit_voice(&mut self, text: &str) -> Result<MessageDispatch, ConversationError> {
        if text.trim().is_empty() {
            return Err(ConversationError::BlankMessage);
        }
        if self.muted.enabled() {
            return Err(ConversationError::Muted);
        }
        if self.sleeping.enabled() {
            return Err(ConversationError::Asleep);
        }
        self.microphone = MicrophonePrivacy::CapturingSpeech;
        self.dispatch(text, InputModality::Voice)
    }

    /// Finish a response and either retain follow-up capture or return to idle.
    pub fn finish_response(&mut self) {
        self.microphone =
            if self.stay_open.enabled() && !self.muted.enabled() && !self.sleeping.enabled() {
                MicrophonePrivacy::CapturingSpeech
            } else {
                self.idle_microphone()
            };
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.muted = if muted {
            ControlState::Enabled
        } else {
            ControlState::Disabled
        };
        self.microphone = self.idle_microphone();
    }

    pub fn set_quiet(&mut self, quiet: bool) {
        self.quiet = if quiet {
            ControlState::Enabled
        } else {
            ControlState::Disabled
        };
    }

    pub fn sleep(&mut self) {
        self.sleeping = ControlState::Enabled;
        self.microphone = self.idle_microphone();
    }

    pub fn wake(&mut self) {
        self.sleeping = ControlState::Disabled;
        self.microphone = self.idle_microphone();
    }

    pub fn set_guest(&mut self, guest: bool) {
        self.guest = if guest {
            ControlState::Enabled
        } else {
            ControlState::Disabled
        };
    }

    pub fn set_stay_open(&mut self, stay_open: bool) {
        self.stay_open = if stay_open {
            ControlState::Enabled
        } else {
            ControlState::Disabled
        };
        if !stay_open {
            self.microphone = self.idle_microphone();
        }
    }

    /// Stop the current foreground work/playback without conflating stop with
    /// sleep, mute, quiet, or guest mode.
    #[must_use]
    pub fn stop(&mut self) -> StopEffects {
        self.microphone = self.idle_microphone();
        StopEffects {
            abort_foreground: true,
            stop_playback: true,
            clear_follow_up_capture: true,
        }
    }

    /// Select a project context without deleting any other context.
    ///
    /// # Errors
    ///
    /// Returns `BlankSelection` when the project name is blank.
    pub fn switch_project(&mut self, project: &str) -> Result<(), ConversationError> {
        let project = nonblank(project)?;
        self.contexts
            .entry(format!("project:{project}"))
            .or_default();
        self.active_project = Some(project.into());
        Ok(())
    }

    pub fn close_project(&mut self) {
        self.active_project = None;
    }

    /// Change persona while preserving sessions and project selection.
    ///
    /// # Errors
    ///
    /// Returns `BlankSelection` when the persona name is blank.
    pub fn switch_persona(&mut self, persona: &str) -> Result<(), ConversationError> {
        self.persona = nonblank(persona)?.into();
        Ok(())
    }

    /// Change provider/model selection without resetting conversation context.
    ///
    /// # Errors
    ///
    /// Returns `BlankSelection` when provider or model is blank.
    pub fn switch_model(
        &mut self,
        provider: &str,
        model: &str,
        effort: Option<String>,
    ) -> Result<(), ConversationError> {
        self.model = Some(ModelSelection {
            provider: nonblank(provider)?.into(),
            model: nonblank(model)?.into(),
            effort,
        });
        Ok(())
    }

    /// Associate the active context with a provider runtime session.
    ///
    /// # Errors
    ///
    /// Returns `BlankSelection` when the session ID is blank.
    pub fn attach_session(&mut self, session_id: &str) -> Result<(), ConversationError> {
        let session_id = nonblank(session_id)?;
        self.contexts
            .entry(self.context_key())
            .or_default()
            .session_id = Some(session_id.into());
        Ok(())
    }
}

fn nonblank(value: &str) -> Result<&str, ConversationError> {
    let value = value.trim();
    if value.is_empty() {
        Err(ConversationError::BlankSelection)
    } else {
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_chat_remains_silent_and_does_not_open_the_microphone() {
        let mut state = ConversationState::default();
        let before = state.microphone;
        let dispatch = state.submit_typed("hello").expect("typed dispatch");
        assert_eq!(dispatch.modality, InputModality::Typed);
        assert!(!dispatch.speak_response);
        assert_eq!(state.microphone, before);
    }

    #[test]
    fn sleep_mute_quiet_and_stop_are_distinct_controls() {
        let mut state = ConversationState::default();
        state.set_quiet(true);
        assert!(!state.submit_voice("quiet reply").unwrap().speak_response);
        state.finish_response();
        state.set_muted(true);
        assert_eq!(state.submit_voice("blocked"), Err(ConversationError::Muted));
        assert_eq!(state.microphone, MicrophonePrivacy::Inactive);
        state.set_muted(false);
        state.sleep();
        assert_eq!(
            state.submit_voice("blocked"),
            Err(ConversationError::Asleep)
        );
        assert_eq!(state.microphone, MicrophonePrivacy::WakeWordOnly);
        let persistent = (state.sleeping, state.muted, state.quiet, state.guest);
        let effects = state.stop();
        assert!(effects.abort_foreground && effects.stop_playback);
        assert_eq!(
            persistent,
            (state.sleeping, state.muted, state.quiet, state.guest)
        );
    }

    #[test]
    fn project_persona_and_model_switches_preserve_unrelated_contexts() {
        let mut state = ConversationState::default();
        state.attach_session("general-session").unwrap();
        state.submit_typed("general turn").unwrap();
        state.switch_project("atlas").unwrap();
        state.attach_session("atlas-session").unwrap();
        state.submit_typed("project turn").unwrap();
        state.switch_persona("Reviewer").unwrap();
        state
            .switch_model("fixture", "deterministic", Some("high".into()))
            .unwrap();
        state.close_project();
        assert_eq!(
            state.contexts["general"].session_id.as_deref(),
            Some("general-session")
        );
        assert_eq!(state.contexts["general"].turns, 1);
        assert_eq!(
            state.contexts["project:atlas"].session_id.as_deref(),
            Some("atlas-session")
        );
        assert_eq!(state.contexts["project:atlas"].turns, 1);
    }

    #[test]
    fn guest_and_follow_up_modes_keep_privacy_semantics_visible() {
        let mut state = ConversationState::default();
        state.set_guest(true);
        state.set_stay_open(true);
        let dispatch = state.submit_voice("guest question").unwrap();
        assert_eq!(dispatch.history_scope, "guest");
        assert!(dispatch.restricted_tools);
        state.finish_response();
        assert_eq!(state.microphone, MicrophonePrivacy::CapturingSpeech);
        state.set_stay_open(false);
        assert_eq!(state.microphone, MicrophonePrivacy::WakeWordOnly);
    }
}
