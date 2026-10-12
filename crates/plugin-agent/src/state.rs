//! The agent interface's own setting, saved in front of the plug-in's own
//! state.
//!
//! | Bytes | What |
//! |---|---|
//! | 4 | `LSPA` |
//! | 1 | the layout's version, 1 |
//! | 1 | flags: bit 0, the agent server is on |
//! | the rest | the plug-in's own state |
//!
//! A state without the header (saved before the plug-in had the agent
//! interface) is all the plug-in's own, with the server off.

const MAGIC: &[u8; 4] = b"LSPA";
const VERSION: u8 = 1;
const SERVER_ON: u8 = 1;

/// The setting this layer saves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Settings {
    /// The person has switched the agent server on.
    pub server: bool,
}

/// `settings`, then the plug-in's `own` state.
pub(crate) fn encode(settings: Settings, own: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(6 + own.len());
    bytes.extend_from_slice(MAGIC);
    bytes.push(VERSION);
    bytes.push(if settings.server { SERVER_ON } else { 0 });
    bytes.extend_from_slice(own);
    bytes
}

/// The settings in `bytes`, and the plug-in's own state after them.
pub(crate) fn decode(bytes: &[u8]) -> (Settings, &[u8]) {
    match bytes {
        [m0, m1, m2, m3, VERSION, flags, own @ ..] if [*m0, *m1, *m2, *m3] == *MAGIC => (
            Settings {
                server: flags & SERVER_ON != 0,
            },
            own,
        ),
        _ => (Settings::default(), bytes),
    }
}

/// A whole saved state (as a format saves it: every parameter's value,
/// then the plug-in's own bytes) with the agent server switched on or off,
/// as a project saved with it on would hold. `None` if `state` isn't one.
#[must_use]
pub fn with_server(state: &[u8], on: bool) -> Option<Vec<u8>> {
    let own = lspk_plugin::state::decode(state).ok()?.own;
    let (_, plugin) = decode(own);
    let mut out = state[..state.len() - own.len()].to_vec();
    out.extend_from_slice(&encode(Settings { server: on }, plugin));
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setting_goes_in_front_of_the_plugins_own_state() {
        let bytes = encode(Settings { server: true }, b"own");
        assert_eq!(decode(&bytes), (Settings { server: true }, &b"own"[..]));
        let off = encode(Settings::default(), b"");
        assert_eq!(decode(&off), (Settings::default(), &b""[..]));
        // A state from before the agent interface is all the plug-in's.
        assert_eq!(decode(b"older"), (Settings::default(), &b"older"[..]));
    }
}
