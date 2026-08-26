//! Plugin and skill installation policy. Renderer code is never loadable.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use thiserror::Error;

/// Permitted plugin execution boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginRuntime {
    Declarative,
    Wasi,
    Process,
}

/// Signed manifest shown in installation preview.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub publisher: String,
    pub runtime: PluginRuntime,
    pub entrypoint: Option<String>,
    pub scopes: BTreeSet<String>,
    pub signed: bool,
    pub renderer_code: bool,
}

/// Install gate failure.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum PluginError {
    #[error("unsigned plugins are disabled by default")]
    Unsigned,
    #[error("plugins may contribute only declarative UI")]
    RendererCode,
    #[error("plugin requests forbidden core-policy scope: {0}")]
    PolicyRewrite(String),
}

/// Static security preview before an installer can ask for user consent.
///
/// # Errors
///
/// Rejects unsigned manifests (unless explicitly allowed), renderer code, and
/// any scope that attempts to rewrite core policy.
pub fn inspect(manifest: &PluginManifest, allow_unsigned: bool) -> Result<(), PluginError> {
    if !manifest.signed && !allow_unsigned {
        return Err(PluginError::Unsigned);
    }
    if manifest.renderer_code {
        return Err(PluginError::RendererCode);
    }
    if let Some(scope) = manifest
        .scopes
        .iter()
        .find(|scope| scope.starts_with("core.policy."))
    {
        return Err(PluginError::PolicyRewrite(scope.clone()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plugin_cannot_rewrite_safety_policy() {
        let manifest = PluginManifest {
            id: "evil".into(),
            name: "Evil".into(),
            version: "1".into(),
            publisher: "test".into(),
            runtime: PluginRuntime::Wasi,
            entrypoint: Some("evil.wasm".into()),
            scopes: ["core.policy.disable".into()].into(),
            signed: true,
            renderer_code: false,
        };
        assert!(matches!(
            inspect(&manifest, false),
            Err(PluginError::PolicyRewrite(_))
        ));
    }
}
