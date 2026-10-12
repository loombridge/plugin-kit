//! The state a format saves for a plug-in: every parameter's value by ID,
//! then the plug-in's own bytes.
//!
//! The layout, all little-endian:
//!
//! | Bytes | What |
//! |---|---|
//! | 4 | `LSPK` |
//! | 4 | the layout's version, 1 |
//! | 4 | how many parameters follow, `n` |
//! | 12 × `n` | each parameter's ID (4 bytes) and value (8 bytes, an IEEE 754 double) |
//! | the rest | the plug-in's own state ([`Plugin::save_state`](crate::Plugin::save_state)) |
//!
//! Values are saved by ID, so a plug-in can add parameters and still load
//! old states: a parameter a state doesn't name takes its default, and one
//! the plug-in no longer has is ignored.

use lspk_params::ParamId;

use crate::PluginError;
use crate::params::ParamValues;

const MAGIC: &[u8; 4] = b"LSPK";
const VERSION: u32 = 1;

/// The state of `values`, followed by `own`.
#[must_use]
pub fn encode(values: &ParamValues, own: &[u8]) -> Vec<u8> {
    let count = values.len();
    let mut bytes = Vec::with_capacity(12 + count * 12 + own.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&VERSION.to_le_bytes());
    bytes.extend_from_slice(&u32::try_from(count).unwrap_or(u32::MAX).to_le_bytes());
    for (index, descriptor) in values.descriptors().iter().enumerate() {
        bytes.extend_from_slice(&descriptor.id.0.to_le_bytes());
        bytes.extend_from_slice(&values.value_at(index).to_le_bytes());
    }
    bytes.extend_from_slice(own);
    bytes
}

/// A decoded state.
#[derive(Debug, PartialEq)]
pub struct Decoded<'a> {
    /// Each saved parameter's ID and value.
    pub params: Vec<(ParamId, f64)>,
    /// The plug-in's own state.
    pub own: &'a [u8],
}

/// Reads a state [`encode`] wrote.
///
/// # Errors
///
/// `bytes` isn't a state, or is from a later layout.
pub fn decode(bytes: &[u8]) -> Result<Decoded<'_>, PluginError> {
    let not_a_state = || PluginError::new("not a plug-in kit state");
    let word = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };
    if bytes.get(..4) != Some(MAGIC) {
        return Err(not_a_state());
    }
    let version = word(4).ok_or_else(not_a_state)?;
    if version != VERSION {
        return Err(PluginError::new(format!(
            "a state in layout {version}, which this version can't read"
        )));
    }
    let count = word(8).ok_or_else(not_a_state)? as usize;
    let end = count
        .checked_mul(12)
        .and_then(|n| n.checked_add(12))
        .filter(|end| *end <= bytes.len())
        .ok_or_else(not_a_state)?;
    let params = bytes[12..end]
        .as_chunks::<12>()
        .0
        .iter()
        .map(|entry| {
            let (id, value) = entry.split_at(4);
            let id = u32::from_le_bytes(id.try_into().unwrap_or_default());
            let value = f64::from_le_bytes(value.try_into().unwrap_or_default());
            (ParamId(id), value)
        })
        .collect();
    Ok(Decoded {
        params,
        own: &bytes[end..],
    })
}

/// Loads `decoded`'s values into `values`: each parameter it names takes
/// its value, and every other its default.
pub fn apply(decoded: &Decoded<'_>, values: &ParamValues) {
    for descriptor in values.descriptors() {
        values.set(descriptor.id, descriptor.default);
    }
    for (id, value) in &decoded.params {
        values.set(*id, *value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lspk_params::ParamDescriptor;

    fn values() -> ParamValues {
        ParamValues::new(vec![
            ParamDescriptor::gain(ParamId(1), "Gain", 6.0, 0.0),
            ParamDescriptor::toggle(ParamId(2), "On", true),
        ])
        .expect("valid parameters")
    }

    #[test]
    fn a_state_round_trips_with_the_plugins_own_bytes() {
        let saved = values();
        saved.set(ParamId(1), 0.0);
        saved.set(ParamId(2), 0.0);
        let bytes = encode(&saved, b"own");
        let decoded = decode(&bytes).expect("a state");
        assert_eq!(decoded.params, [(ParamId(1), 0.0), (ParamId(2), 0.0)]);
        assert_eq!(decoded.own, b"own");
        let loaded = values();
        apply(&decoded, &loaded);
        assert_eq!(loaded.get(ParamId(1)), Some(0.0));
        assert_eq!(loaded.get(ParamId(2)), Some(0.0));
    }

    #[test]
    fn missing_parameters_take_defaults_and_unknown_ones_are_ignored() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"LSPK");
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&99u32.to_le_bytes());
        bytes.extend_from_slice(&0.5f64.to_le_bytes());
        let values = values();
        values.set(ParamId(1), 0.25);
        apply(&decode(&bytes).expect("a state"), &values);
        assert_eq!(values.get(ParamId(1)), Some(1.0));
        assert_eq!(values.get(ParamId(2)), Some(1.0));
    }

    #[test]
    fn other_bytes_are_refused() {
        assert!(decode(b"").is_err());
        assert!(decode(b"LSPK").is_err());
        let mut later = b"LSPK".to_vec();
        later.extend_from_slice(&2u32.to_le_bytes());
        later.extend_from_slice(&0u32.to_le_bytes());
        assert!(decode(&later).unwrap_err().message().contains("layout 2"));
        let mut short = b"LSPK".to_vec();
        short.extend_from_slice(&1u32.to_le_bytes());
        short.extend_from_slice(&5u32.to_le_bytes());
        assert!(decode(&short).is_err());
    }
}
