//! Capture privacy, exclusions, and redaction policy.

use crate::{ActiveView, Rect, WindowId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use thiserror::Error;

/// Scope requested from the native capture picker/session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureScope {
    ActiveWindow,
    Window(WindowId),
    Display(String),
}

/// One explicit privacy exclusion.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrivacyExclusion {
    Application { application_id: String },
    Window { window_id: WindowId },
    TitleContains { text: String },
}

/// User-controlled capture rules evaluated before semantic or pixel access.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScreenPrivacyPolicy {
    pub capture_enabled: bool,
    pub allow_full_display_capture: bool,
    pub deny_secure_surfaces: bool,
    pub exclusions: Vec<PrivacyExclusion>,
    pub redacted_regions: Vec<Rect>,
    /// Applications whose accessibility text is always stripped even if their
    /// pixels are allowed (for example password managers).
    pub redact_semantics_for_applications: BTreeSet<String>,
}

impl Default for ScreenPrivacyPolicy {
    fn default() -> Self {
        Self {
            capture_enabled: false,
            allow_full_display_capture: false,
            deny_secure_surfaces: true,
            exclusions: Vec::new(),
            redacted_regions: Vec::new(),
            redact_semantics_for_applications: BTreeSet::new(),
        }
    }
}

impl ScreenPrivacyPolicy {
    /// Check capture access before invoking an operating-system picker or API.
    ///
    /// # Errors
    ///
    /// Denies disabled capture, full-display access without explicit opt-in,
    /// secure surfaces, and configured app/window/title exclusions.
    pub fn authorize(&self, scope: &CaptureScope, view: &ActiveView) -> Result<(), PrivacyError> {
        if !self.capture_enabled {
            return Err(PrivacyError::CaptureDisabled);
        }
        if matches!(scope, CaptureScope::Display(_)) && !self.allow_full_display_capture {
            return Err(PrivacyError::DisplayCaptureNotAllowed);
        }
        if self.deny_secure_surfaces && view.secure_surface {
            return Err(PrivacyError::SecureSurface);
        }
        for exclusion in &self.exclusions {
            let matches = match exclusion {
                PrivacyExclusion::Application { application_id } => {
                    application_id.eq_ignore_ascii_case(&view.application_id)
                }
                PrivacyExclusion::Window { window_id } => window_id == &view.window_id,
                PrivacyExclusion::TitleContains { text } => {
                    !text.trim().is_empty()
                        && view.title.to_lowercase().contains(&text.to_lowercase())
                }
            };
            if matches {
                return Err(PrivacyError::Excluded);
            }
        }
        if self
            .redacted_regions
            .iter()
            .any(|region| !region.is_valid())
        {
            return Err(PrivacyError::InvalidRedactionRegion);
        }
        Ok(())
    }

    #[must_use]
    pub fn redact_semantics(&self, application_id: &str) -> bool {
        self.redact_semantics_for_applications
            .iter()
            .any(|excluded| excluded.eq_ignore_ascii_case(application_id))
    }
}

/// Screen-context request denied by local privacy policy.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum PrivacyError {
    #[error("screen context is disabled")]
    CaptureDisabled,
    #[error("full-display capture requires explicit opt-in")]
    DisplayCaptureNotAllowed,
    #[error("secure surfaces cannot be inspected or captured")]
    SecureSurface,
    #[error("the active application or window is excluded from screen context")]
    Excluded,
    #[error("privacy policy contains an invalid redaction region")]
    InvalidRedactionRegion,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ActiveView;

    fn view() -> ActiveView {
        ActiveView {
            application_id: "org.example.Mail".into(),
            application_name: "Mail".into(),
            process_id: Some(12),
            window_id: WindowId("w1".into()),
            title: "Private inbox".into(),
            bounds: None,
            focused_node: None,
            secure_surface: false,
        }
    }

    #[test]
    fn privacy_is_off_and_window_scoped_by_default() {
        let policy = ScreenPrivacyPolicy::default();
        assert_eq!(
            policy.authorize(&CaptureScope::ActiveWindow, &view()),
            Err(PrivacyError::CaptureDisabled)
        );

        let policy = ScreenPrivacyPolicy {
            capture_enabled: true,
            ..policy
        };
        policy
            .authorize(&CaptureScope::ActiveWindow, &view())
            .expect("active window explicitly allowed");
        assert_eq!(
            policy.authorize(&CaptureScope::Display("display-1".into()), &view()),
            Err(PrivacyError::DisplayCaptureNotAllowed)
        );
    }

    #[test]
    fn exclusions_and_secure_surfaces_fail_closed() {
        let policy = ScreenPrivacyPolicy {
            capture_enabled: true,
            exclusions: vec![PrivacyExclusion::TitleContains {
                text: "INBOX".into(),
            }],
            ..ScreenPrivacyPolicy::default()
        };
        assert_eq!(
            policy.authorize(&CaptureScope::ActiveWindow, &view()),
            Err(PrivacyError::Excluded)
        );
        let mut secure = view();
        secure.secure_surface = true;
        let no_exclusions = ScreenPrivacyPolicy {
            capture_enabled: true,
            ..ScreenPrivacyPolicy::default()
        };
        assert_eq!(
            no_exclusions.authorize(&CaptureScope::ActiveWindow, &secure),
            Err(PrivacyError::SecureSurface)
        );
    }
}
