//! The computer keyboard as a MIDI keyboard, for instruments.
//!
//! Two rows of letter keys play an octave and a third, as on a piano: the
//! home row the white keys from C (A, S, D, F, G, H, J, K, L, ;), the row
//! above the black keys (W, E, T, Y, U, O, P). Z and X move down and up an
//! octave, C and V lower and raise the velocity. A key plays the note it
//! started with until it's released, whatever changes in between.

use crate::engine::MidiMessage;

/// The lowest and highest octave shift, from the middle.
const OCTAVES: (i8, i8) = (-3, 4);
/// How far C and V move the velocity.
const VELOCITY_STEP: u8 = 20;

/// What a key on the computer keyboard does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    /// It plays this many semitones above the octave's C.
    Note(u8),
    /// Down an octave.
    OctaveDown,
    /// Up an octave.
    OctaveUp,
    /// Softer.
    VelocityDown,
    /// Louder.
    VelocityUp,
}

/// What `key` does, if anything: a lower-case letter, or `;`.
#[must_use]
pub fn action(key: char) -> Option<KeyAction> {
    let note = |n| Some(KeyAction::Note(n));
    match key.to_ascii_lowercase() {
        'a' => note(0),
        'w' => note(1),
        's' => note(2),
        'e' => note(3),
        'd' => note(4),
        'f' => note(5),
        't' => note(6),
        'g' => note(7),
        'y' => note(8),
        'h' => note(9),
        'u' => note(10),
        'j' => note(11),
        'k' => note(12),
        'o' => note(13),
        'l' => note(14),
        'p' => note(15),
        ';' => note(16),
        'z' => Some(KeyAction::OctaveDown),
        'x' => Some(KeyAction::OctaveUp),
        'c' => Some(KeyAction::VelocityDown),
        'v' => Some(KeyAction::VelocityUp),
        _ => None,
    }
}

/// The computer keyboard's octave, velocity and held keys.
#[derive(Clone, Debug)]
pub struct ComputerKeyboard {
    octave: i8,
    velocity: u8,
    /// The note each held key plays, by the key's lower-case character.
    held: Vec<(char, u8)>,
}

impl Default for ComputerKeyboard {
    fn default() -> Self {
        Self::new(0, 100)
    }
}

impl ComputerKeyboard {
    /// A keyboard shifted `octave` octaves from the middle, whose A key
    /// plays middle C (key 60) at octave 0, playing at `velocity`.
    #[must_use]
    pub fn new(octave: i8, velocity: u8) -> Self {
        Self {
            octave: octave.clamp(OCTAVES.0, OCTAVES.1),
            velocity: velocity.clamp(1, 127),
            held: Vec::new(),
        }
    }

    /// The octave shift.
    #[must_use]
    pub fn octave(&self) -> i8 {
        self.octave
    }

    /// The velocity, from 1 to 127.
    #[must_use]
    pub fn velocity(&self) -> u8 {
        self.velocity
    }

    /// The key A plays now.
    #[must_use]
    pub fn base_key(&self) -> u8 {
        (60 + 12 * i16::from(self.octave)).clamp(0, 127) as u8
    }

    /// A key went down; returns the note-on it plays, if any. A key that's
    /// already down (the keyboard repeating) plays nothing.
    pub fn press(&mut self, key: char) -> Option<MidiMessage> {
        let key = key.to_ascii_lowercase();
        match action(key)? {
            KeyAction::Note(offset) => {
                if self.held.iter().any(|(k, _)| *k == key) {
                    return None;
                }
                let note = u8::try_from(u16::from(self.base_key()) + u16::from(offset)).ok()?;
                if note > 127 {
                    return None;
                }
                self.held.push((key, note));
                Some([0x90, note, self.velocity])
            }
            KeyAction::OctaveDown => {
                self.octave = (self.octave - 1).max(OCTAVES.0);
                None
            }
            KeyAction::OctaveUp => {
                self.octave = (self.octave + 1).min(OCTAVES.1);
                None
            }
            KeyAction::VelocityDown => {
                self.velocity = self.velocity.saturating_sub(VELOCITY_STEP).max(1);
                None
            }
            KeyAction::VelocityUp => {
                self.velocity = self.velocity.saturating_add(VELOCITY_STEP).min(127);
                None
            }
        }
    }

    /// A key came up; returns the note-off for the note it started, if any.
    pub fn release(&mut self, key: char) -> Option<MidiMessage> {
        let key = key.to_ascii_lowercase();
        let at = self.held.iter().position(|(k, _)| *k == key)?;
        let (_, note) = self.held.remove(at);
        Some([0x80, note, 0])
    }

    /// Releases every held key, as when the window loses the keyboard.
    pub fn release_all(&mut self) -> Vec<MidiMessage> {
        self.held
            .drain(..)
            .map(|(_, note)| [0x80, note, 0])
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_home_row_plays_white_keys_from_middle_c() {
        let mut keyboard = ComputerKeyboard::default();
        assert_eq!(keyboard.press('a'), Some([0x90, 60, 100]));
        assert_eq!(keyboard.press('A'), None, "a held key doesn't repeat");
        assert_eq!(keyboard.press('w'), Some([0x90, 61, 100]));
        assert_eq!(keyboard.press(';'), Some([0x90, 76, 100]));
        assert_eq!(keyboard.release('a'), Some([0x80, 60, 0]));
        assert_eq!(keyboard.release('a'), None);
        assert_eq!(keyboard.press('q'), None);
        assert_eq!(keyboard.release_all().len(), 2);
    }

    #[test]
    fn octave_and_velocity_move_within_their_limits() {
        let mut keyboard = ComputerKeyboard::default();
        keyboard.press('a');
        keyboard.press('x');
        keyboard.press('v');
        assert_eq!(keyboard.press('s'), Some([0x90, 74, 120]));
        // The first key ends the note it started.
        assert_eq!(keyboard.release('a'), Some([0x80, 60, 0]));
        for _ in 0..10 {
            keyboard.press('z');
            keyboard.press('c');
        }
        assert_eq!((keyboard.octave(), keyboard.velocity()), (-3, 1));
        assert_eq!(keyboard.base_key(), 24);
        for _ in 0..10 {
            keyboard.press('x');
            keyboard.press('v');
        }
        assert_eq!((keyboard.octave(), keyboard.velocity()), (4, 127));
        assert_eq!(keyboard.press(';'), Some([0x90, 124, 127]));
    }
}
