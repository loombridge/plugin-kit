//! Perception of the interface: `ui.capture` draws the plug-in's editor as
//! it looks now into an image and hands it to the agent, as `preview` hands
//! it audio.
//!
//! The picture comes from a fresh editor ([`Plugin::editor`]) drawn off
//! screen from the live parameter values, in software: no window opens, the
//! host's window (if one is open) is untouched, and it works headless. An
//! agent in a `run` call sees the values as the commands before it left
//! them.

use lspk_agent::Media;
use lspk_commands::{CommandSpec, Context, Permission, RegisterError, Registry};
use lspk_plugin::{EditorContext, Host, Plugin};
use lspk_test::snapshot::encode_png;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::commands::parse;

/// Captures the interface as an image.
pub const CAPTURE: &str = "ui.capture";

/// The smallest scale, in pixels to a point.
pub const MIN_SCALE: f32 = 0.25;
/// The largest scale, in pixels to a point: bigger pictures cost an agent
/// more than they show.
pub const MAX_SCALE: f32 = 4.0;

/// Arguments of `ui.capture`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaptureArgs {
    /// Pixels to a point, from 0.25 to 4. Left out, 1: the editor at its
    /// own size at 100 % zoom.
    #[serde(default)]
    pub scale: Option<f32>,
}

/// What `ui.capture` returns beside the image.
#[derive(Debug, Serialize, JsonSchema)]
pub struct CaptureResult {
    /// The image's width, in pixels.
    pub width: u32,
    /// The image's height, in pixels.
    pub height: u32,
    /// Pixels to a point.
    pub scale: f32,
}

/// Adds `ui.capture` to `registry`.
///
/// # Errors
///
/// It's already registered.
pub fn register(registry: &mut Registry) -> Result<(), RegisterError> {
    registry.register(
        CommandSpec::new(CAPTURE, "Capture Interface")
            .description(
                "Draws the plug-in's interface as it looks now and returns it as a PNG image, \
                 to see what a person would: the controls, their values and the layout. It \
                 draws from the plug-in's current parameter values, off screen, so no window \
                 opens and an open one is untouched; run after `params.set` in one call to see \
                 the new values. Give `scale` (0.25 to 4 pixels to a point) for a smaller or \
                 sharper picture. It fails for a plug-in with no interface.",
            )
            .permission(Permission::Read)
            .context(Context::Global)
            .args::<CaptureArgs>()
            .result::<CaptureResult>(),
    )
}

/// Runs `ui.capture` for `plugin`: its result and the image.
///
/// # Errors
///
/// The arguments don't fit, the plug-in has no interface, or its
/// parameters aren't described yet.
pub fn run<P: Plugin>(plugin: &mut P, host: &Host, args: &Value) -> Result<(Value, Media), String> {
    let CaptureArgs { scale } = parse(args)?;
    let scale = scale.unwrap_or(1.0);
    if !(MIN_SCALE..=MAX_SCALE).contains(&scale) {
        return Err(format!(
            "`scale` is from {MIN_SCALE} to {MAX_SCALE} pixels to a point, not {scale}"
        ));
    }
    let values = host
        .values_shared()
        .ok_or("the plug-in is still starting")?;
    let editor = plugin
        .editor()
        .ok_or_else(|| format!("{} has no interface to capture", P::INFO.name))?;
    let image = editor
        .capture(&EditorContext::detached(values), scale)
        .map_err(|e| e.to_string())?;
    let result = CaptureResult {
        width: image.width,
        height: image.height,
        scale,
    };
    let media = Media {
        mime_type: "image/png",
        bytes: encode_png(&image)?,
        uri: None,
    };
    Ok((
        serde_json::to_value(result).map_err(|e| e.to_string())?,
        media,
    ))
}
