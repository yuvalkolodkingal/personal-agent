//! Input dispatch against resolved accessibility nodes.
//!
//! Every action follows the same contract as the desktop coordinator: validate
//! that the caller's handle belongs to the current generation, re-resolve the
//! node's live geometry, dispatch, then re-snapshot and check the postcondition.
//! Nothing here evaluates page script; coordinates come from `DOM.getBoxModel`
//! and text goes in through `Input`.

use super::{CdpBrowser, NodeRecord};
use crate::{BrowserError, NodeHandle, PageSnapshot, validate_handle};
use base64::Engine as _;
use chromiumoxide_cdp::cdp::browser_protocol::dom::{
    BackendNodeId, FocusParams, GetBoxModelParams, ScrollIntoViewIfNeededParams,
    SetFileInputFilesParams,
};
use chromiumoxide_cdp::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, DispatchMouseEventParams, DispatchMouseEventType,
    InsertTextParams, MouseButton,
};
use chromiumoxide_cdp::cdp::browser_protocol::page::{CaptureScreenshotParams, PrintToPdfParams};
use std::path::{Path, PathBuf};

/// A keyboard key the engine is allowed to synthesize. Keeping this closed means
/// page text can never be replayed as a shortcut.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControlKey {
    Home,
    ArrowDown,
}

impl ControlKey {
    const fn parts(self) -> (&'static str, i64) {
        match self {
            Self::Home => ("Home", 36),
            Self::ArrowDown => ("ArrowDown", 40),
        }
    }
}

impl CdpBrowser {
    /// Check the handle's generation and map it to a live backend node.
    fn resolve(&self, handle: &NodeHandle) -> Result<NodeRecord, BrowserError> {
        self.guard_takeover()?;
        let current = self.current.as_ref().ok_or(BrowserError::StaleHandle)?;
        validate_handle(current, handle)?;
        self.nodes
            .get(&handle.opaque_id)
            .cloned()
            .ok_or(BrowserError::StaleHandle)
    }

    fn addressed(&self) -> Result<(super::CdpClient, String), BrowserError> {
        let session = self.session()?;
        Ok((session.client.clone(), session.attachment_id.clone()))
    }

    /// Scroll the node into view and return the centre of its content box in
    /// viewport CSS pixels, which is the coordinate space `Input` expects.
    async fn point_of(&self, backend_node_id: i64) -> Result<(f64, f64), BrowserError> {
        let (client, session_id) = self.addressed()?;
        client
            .send(
                Some(&session_id),
                &ScrollIntoViewIfNeededParams::builder()
                    .backend_node_id(BackendNodeId::new(backend_node_id))
                    .build(),
            )
            .await?;
        let model = client
            .send(
                Some(&session_id),
                &GetBoxModelParams::builder()
                    .backend_node_id(BackendNodeId::new(backend_node_id))
                    .build(),
            )
            .await?
            .model;
        let quad = model.content.inner();
        if quad.len() < 8 {
            return Err(BrowserError::Operation(
                "element has no layout box to click".into(),
            ));
        }
        let xs = [quad[0], quad[2], quad[4], quad[6]];
        let ys = [quad[1], quad[3], quad[5], quad[7]];
        let x = xs.iter().sum::<f64>() / 4.0;
        let y = ys.iter().sum::<f64>() / 4.0;
        if !x.is_finite() || !y.is_finite() {
            return Err(BrowserError::Operation(
                "element has a degenerate layout box".into(),
            ));
        }
        Ok((x, y))
    }

    async fn mouse(
        &self,
        kind: DispatchMouseEventType,
        x: f64,
        y: f64,
        clicks: i64,
    ) -> Result<(), BrowserError> {
        let (client, session_id) = self.addressed()?;
        let params = DispatchMouseEventParams::builder()
            .r#type(kind)
            .x(x)
            .y(y)
            .button(MouseButton::Left)
            .click_count(clicks)
            .build()
            .map_err(BrowserError::Operation)?;
        client.send(Some(&session_id), &params).await?;
        Ok(())
    }

    async fn press(&self, key: ControlKey) -> Result<(), BrowserError> {
        let (client, session_id) = self.addressed()?;
        let (name, code) = key.parts();
        for kind in [
            DispatchKeyEventType::RawKeyDown,
            DispatchKeyEventType::KeyUp,
        ] {
            let params = DispatchKeyEventParams::builder()
                .r#type(kind)
                .key(name)
                .code(name)
                .windows_virtual_key_code(code)
                .native_virtual_key_code(code)
                .build()
                .map_err(BrowserError::Operation)?;
            client.send(Some(&session_id), &params).await?;
        }
        Ok(())
    }

    async fn focus(&self, backend_node_id: i64) -> Result<(), BrowserError> {
        let (client, session_id) = self.addressed()?;
        client
            .send(
                Some(&session_id),
                &FocusParams::builder()
                    .backend_node_id(BackendNodeId::new(backend_node_id))
                    .build(),
            )
            .await?;
        Ok(())
    }

    /// Click the centre of an element after scrolling it into view.
    ///
    /// # Errors
    ///
    /// Returns [`BrowserError::StaleHandle`] for a handle from an earlier
    /// generation, or a protocol error.
    pub async fn click_node(&mut self, handle: &NodeHandle) -> Result<PageSnapshot, BrowserError> {
        let node = self.resolve(handle)?;
        let (x, y) = self.point_of(node.backend_node_id).await?;
        self.mouse(DispatchMouseEventType::MouseMoved, x, y, 0)
            .await?;
        self.mouse(DispatchMouseEventType::MousePressed, x, y, 1)
            .await?;
        self.mouse(DispatchMouseEventType::MouseReleased, x, y, 1)
            .await?;
        self.settle().await;
        self.snapshot_after_action().await
    }

    /// Move the pointer over an element without pressing anything.
    ///
    /// # Errors
    ///
    /// Returns [`BrowserError::StaleHandle`] for a stale handle, or a protocol
    /// error.
    pub async fn hover_node(&mut self, handle: &NodeHandle) -> Result<PageSnapshot, BrowserError> {
        let node = self.resolve(handle)?;
        let (x, y) = self.point_of(node.backend_node_id).await?;
        self.mouse(DispatchMouseEventType::MouseMoved, x, y, 0)
            .await?;
        self.snapshot_after_action().await
    }

    /// Scroll the viewport by a wheel delta anchored on an element.
    ///
    /// # Errors
    ///
    /// Returns [`BrowserError::StaleHandle`] for a stale handle, or a protocol
    /// error.
    pub async fn scroll_node(
        &mut self,
        handle: &NodeHandle,
        delta_x: f64,
        delta_y: f64,
    ) -> Result<PageSnapshot, BrowserError> {
        let node = self.resolve(handle)?;
        let (x, y) = self.point_of(node.backend_node_id).await?;
        let (client, session_id) = self.addressed()?;
        let params = DispatchMouseEventParams::builder()
            .r#type(DispatchMouseEventType::MouseWheel)
            .x(x)
            .y(y)
            .delta_x(delta_x)
            .delta_y(delta_y)
            .build()
            .map_err(BrowserError::Operation)?;
        client.send(Some(&session_id), &params).await?;
        self.snapshot_after_action().await
    }

    /// Focus a field and insert text, then verify the field really holds it.
    ///
    /// # Errors
    ///
    /// Returns [`BrowserError::StaleHandle`] for a stale handle and
    /// [`BrowserError::PostconditionFailed`] when the field did not accept the
    /// text.
    pub async fn type_into(
        &mut self,
        handle: &NodeHandle,
        text: &str,
    ) -> Result<PageSnapshot, BrowserError> {
        let node = self.resolve(handle)?;
        self.point_of(node.backend_node_id).await?;
        self.focus(node.backend_node_id).await?;
        let (client, session_id) = self.addressed()?;
        client
            .send(Some(&session_id), &InsertTextParams::new(text.to_owned()))
            .await?;
        let snapshot = self.snapshot_after_action().await?;
        if text.is_empty() {
            return Ok(snapshot);
        }
        let observed = snapshot
            .node_by_opaque_id(&handle.opaque_id)
            .and_then(|node| node.value.clone());
        match observed {
            Some(value) if value.contains(text) => Ok(snapshot),
            Some(value) => Err(BrowserError::PostconditionFailed(format!(
                "field holds {value:?} after typing"
            ))),
            None => Err(BrowserError::PostconditionFailed(
                "the field disappeared while typing".into(),
            )),
        }
    }

    /// Choose an option of a `select` by its visible label.
    ///
    /// The option is chosen with keyboard navigation over the closed control, so
    /// no script is injected to set `value` behind the page's back.
    ///
    /// # Errors
    ///
    /// Returns [`BrowserError::Operation`] when the label is not one of the
    /// element's options and [`BrowserError::PostconditionFailed`] when the
    /// control did not end up on that option.
    pub async fn select_option(
        &mut self,
        handle: &NodeHandle,
        label: &str,
    ) -> Result<PageSnapshot, BrowserError> {
        let node = self.resolve(handle)?;
        let index = node
            .options
            .iter()
            .position(|option| option == label)
            .ok_or_else(|| {
                BrowserError::Operation(format!(
                    "{label:?} is not one of the element's options {:?}",
                    node.options
                ))
            })?;
        self.point_of(node.backend_node_id).await?;
        self.focus(node.backend_node_id).await?;
        self.press(ControlKey::Home).await?;
        for _ in 0..index {
            self.press(ControlKey::ArrowDown).await?;
        }
        let snapshot = self.snapshot_after_action().await?;
        let observed = snapshot
            .node_by_opaque_id(&handle.opaque_id)
            .and_then(|node| node.value.clone());
        if observed.as_deref() == Some(label) {
            Ok(snapshot)
        } else {
            Err(BrowserError::PostconditionFailed(format!(
                "select shows {observed:?} after choosing {label:?}"
            )))
        }
    }

    /// Attach user-approved files to a file input.
    ///
    /// # Errors
    ///
    /// Returns [`BrowserError::UploadPathNotApproved`] for any path the user did
    /// not approve through [`CdpBrowser::approve_upload`].
    pub async fn upload_files(
        &mut self,
        handle: &NodeHandle,
        paths: &[PathBuf],
    ) -> Result<PageSnapshot, BrowserError> {
        let node = self.resolve(handle)?;
        let approved = self.check_approved(paths)?;
        let (client, session_id) = self.addressed()?;
        client
            .send(
                Some(&session_id),
                &SetFileInputFilesParams::builder()
                    .files(approved)
                    .backend_node_id(BackendNodeId::new(node.backend_node_id))
                    .build()
                    .map_err(BrowserError::Operation)?,
            )
            .await?;
        let snapshot = self.snapshot_after_action().await?;
        if snapshot.node_by_opaque_id(&handle.opaque_id).is_none() {
            return Err(BrowserError::PostconditionFailed(
                "the file input disappeared while attaching files".into(),
            ));
        }
        Ok(snapshot)
    }

    fn check_approved(&self, paths: &[PathBuf]) -> Result<Vec<String>, BrowserError> {
        if paths.is_empty() {
            return Err(BrowserError::Operation("no files to upload".into()));
        }
        paths
            .iter()
            .map(|path| {
                let resolved = path.canonicalize().map_err(|error| {
                    BrowserError::UploadPathNotApproved(format!("{}: {error}", path.display()))
                })?;
                if self.approved_uploads.contains(&resolved) {
                    Ok(resolved.display().to_string())
                } else {
                    Err(BrowserError::UploadPathNotApproved(
                        resolved.display().to_string(),
                    ))
                }
            })
            .collect()
    }

    /// Capture a PNG of the current viewport.
    ///
    /// # Errors
    ///
    /// Returns a [`BrowserError`] when no session is open or the capture fails.
    pub async fn screenshot(&self) -> Result<Vec<u8>, BrowserError> {
        let (client, session_id) = self.addressed()?;
        let shot = client
            .send(Some(&session_id), &CaptureScreenshotParams::default())
            .await?;
        decode(String::from(shot.data).as_str(), "screenshot")
    }

    /// Render the current page to PDF.
    ///
    /// # Errors
    ///
    /// Returns a [`BrowserError`] when no session is open or printing fails.
    pub async fn print_pdf(&self) -> Result<Vec<u8>, BrowserError> {
        let (client, session_id) = self.addressed()?;
        let pdf = client
            .send(Some(&session_id), &PrintToPdfParams::default())
            .await?;
        decode(String::from(pdf.data).as_str(), "pdf")
    }

    /// Give the renderer a moment to apply an action before re-reading it.
    async fn settle(&self) {
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    }

    /// Re-read the page so the caller's next step sees post-action state and all
    /// previously issued handles are invalidated.
    async fn snapshot_after_action(&mut self) -> Result<PageSnapshot, BrowserError> {
        self.read_page().await
    }
}

fn decode(data: &str, what: &str) -> Result<Vec<u8>, BrowserError> {
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|error| BrowserError::Operation(format!("malformed {what} payload: {error}")))
}

/// Whether a path is inside a directory, used by callers building approval lists.
#[must_use]
pub fn is_within(path: &Path, directory: &Path) -> bool {
    match (path.canonicalize(), directory.canonicalize()) {
        (Ok(path), Ok(directory)) => path.starts_with(directory),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdp::CdpConfig;

    #[test]
    fn uploads_are_refused_until_the_user_approves_the_exact_file() {
        let directory = tempfile::tempdir().expect("temp dir");
        let approved = directory.path().join("approved.txt");
        let other = directory.path().join("other.txt");
        std::fs::write(&approved, b"ok").expect("write approved");
        std::fs::write(&other, b"no").expect("write other");

        let mut browser = CdpBrowser::new(CdpConfig::default());
        assert!(matches!(
            browser.check_approved(std::slice::from_ref(&approved)),
            Err(BrowserError::UploadPathNotApproved(_))
        ));
        browser.approve_upload(&approved).expect("approve");
        browser
            .check_approved(std::slice::from_ref(&approved))
            .expect("allowed");
        assert!(matches!(
            browser.check_approved(&[other]),
            Err(BrowserError::UploadPathNotApproved(_))
        ));
        assert!(matches!(
            browser.check_approved(&[]),
            Err(BrowserError::Operation(_))
        ));
    }

    #[test]
    fn approving_a_missing_file_fails_closed() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut browser = CdpBrowser::new(CdpConfig::default());
        assert!(matches!(
            browser.approve_upload(&directory.path().join("absent.txt")),
            Err(BrowserError::UploadPathNotApproved(_))
        ));
    }

    #[test]
    fn actions_are_refused_while_the_user_holds_the_browser() {
        let mut browser = CdpBrowser::new(CdpConfig::default());
        browser.takeover = true;
        let handle = NodeHandle {
            page_id: "p".into(),
            generation: 1,
            opaque_id: "5".into(),
        };
        assert!(matches!(
            browser.resolve(&handle),
            Err(BrowserError::TakeoverActive)
        ));
    }

    #[test]
    fn handles_without_a_current_snapshot_are_stale() {
        let browser = CdpBrowser::new(CdpConfig::default());
        let handle = NodeHandle {
            page_id: "p".into(),
            generation: 1,
            opaque_id: "5".into(),
        };
        assert!(matches!(
            browser.resolve(&handle),
            Err(BrowserError::StaleHandle)
        ));
    }

    #[test]
    fn control_keys_carry_their_virtual_key_codes() {
        assert_eq!(ControlKey::Home.parts(), ("Home", 36));
        assert_eq!(ControlKey::ArrowDown.parts(), ("ArrowDown", 40));
    }

    #[test]
    fn is_within_rejects_paths_outside_the_directory() {
        let directory = tempfile::tempdir().expect("temp dir");
        let inside = directory.path().join("inside.txt");
        std::fs::write(&inside, b"x").expect("write");
        assert!(is_within(&inside, directory.path()));
        assert!(!is_within(Path::new("/etc/passwd"), directory.path()));
    }
}
