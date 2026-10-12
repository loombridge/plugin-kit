//! MIDI in: every connected input, and the computer keyboard, into one
//! queue the audio loop reads.
//!
//! The queue's writing end is shared behind a mutex by the MIDI inputs'
//! threads and the window; only those threads ever take the lock. The audio
//! thread owns the reading end and never waits.

use std::sync::{Arc, Mutex};

use midir::{Ignore, MidiInput, MidiInputConnection};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::engine::MidiMessage;

/// How many messages the queue holds between two audio buffers.
pub const QUEUE_CAPACITY: usize = 1024;

/// The writing end of the MIDI queue; clones share it.
#[derive(Clone)]
pub struct MidiSender {
    producer: Arc<Mutex<Producer<MidiMessage>>>,
}

impl MidiSender {
    /// Queues `message`; false if the queue is full. Never call it on an
    /// audio thread.
    pub fn send(&self, message: MidiMessage) -> bool {
        match self.producer.lock() {
            Ok(mut producer) => producer.push(message).is_ok(),
            Err(_) => false,
        }
    }
}

impl std::fmt::Debug for MidiSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MidiSender").finish_non_exhaustive()
    }
}

/// A MIDI queue holding [`QUEUE_CAPACITY`] messages: the writing end for
/// the inputs and the keyboard, and the reading end for the audio loop.
#[must_use]
pub fn queue() -> (MidiSender, Consumer<MidiMessage>) {
    let (producer, consumer) = RingBuffer::new(QUEUE_CAPACITY);
    (
        MidiSender {
            producer: Arc::new(Mutex::new(producer)),
        },
        consumer,
    )
}

/// The channel message at the start of `bytes`, if it is one: a status from
/// 0x80 to 0xEF and its data bytes. System messages are passed over.
#[must_use]
pub fn channel_message(bytes: &[u8]) -> Option<MidiMessage> {
    let status = *bytes.first()?;
    if !(0x80..0xf0).contains(&status) {
        return None;
    }
    let data = |i: usize| bytes.get(i).copied().unwrap_or(0) & 0x7f;
    Some([status, data(1), data(2)])
}

/// Every MIDI input on the machine, connected to the queue. [`scan`] picks
/// up inputs plugged in since, and lets go of ones unplugged.
///
/// [`scan`]: MidiInputs::scan
pub struct MidiInputs {
    client: String,
    sender: MidiSender,
    scanner: Option<MidiInput>,
    connections: Vec<Connection>,
    error: Option<String>,
}

struct Connection {
    id: String,
    name: String,
    _connection: MidiInputConnection<()>,
}

impl MidiInputs {
    /// Inputs that send to `sender`, under the client name `client`; none
    /// are connected until the first [`scan`](Self::scan).
    #[must_use]
    pub fn new(client: &str, sender: MidiSender) -> Self {
        let mut inputs = Self {
            client: client.to_owned(),
            sender,
            scanner: None,
            connections: Vec::new(),
            error: None,
        };
        match MidiInput::new(client) {
            Ok(scanner) => inputs.scanner = Some(scanner),
            Err(error) => inputs.error = Some(format!("MIDI isn't available: {error}")),
        }
        inputs
    }

    /// Connects every input not yet connected, and drops connections whose
    /// input has gone.
    pub fn scan(&mut self) {
        let Some(scanner) = &self.scanner else { return };
        let ports = scanner.ports();
        let present: Vec<String> = ports.iter().map(midir::MidiInputPort::id).collect();
        self.connections.retain(|c| present.contains(&c.id));
        for port in &ports {
            let id = port.id();
            if self.connections.iter().any(|c| c.id == id) {
                continue;
            }
            let name = scanner.port_name(port).unwrap_or_else(|_| id.clone());
            let Ok(mut input) = MidiInput::new(&self.client) else {
                continue;
            };
            input.ignore(Ignore::All);
            let sender = self.sender.clone();
            match input.connect(
                port,
                &self.client,
                move |_, bytes, ()| {
                    if let Some(message) = channel_message(bytes) {
                        sender.send(message);
                    }
                },
                (),
            ) {
                Ok(connection) => self.connections.push(Connection {
                    id,
                    name,
                    _connection: connection,
                }),
                Err(error) => self.error = Some(format!("{name}: {error}")),
            }
        }
    }

    /// The names of the connected inputs.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.connections.iter().map(|c| c.name.clone()).collect()
    }

    /// The last thing that went wrong, if anything did.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_messages_pass_and_system_ones_do_not() {
        assert_eq!(channel_message(&[0x90, 60, 100]), Some([0x90, 60, 100]));
        assert_eq!(channel_message(&[0xc3, 5]), Some([0xc3, 5, 0]));
        assert_eq!(channel_message(&[0xf8]), None);
        assert_eq!(channel_message(&[0xf0, 1, 2, 0xf7]), None);
        assert_eq!(channel_message(&[60]), None);
        assert_eq!(channel_message(&[]), None);
    }

    #[test]
    fn the_queue_carries_messages_until_full() {
        let (sender, mut consumer) = queue();
        assert!(sender.send([0x90, 60, 1]));
        assert_eq!(consumer.pop(), Ok([0x90, 60, 1]));
        for _ in 0..QUEUE_CAPACITY {
            assert!(sender.clone().send([0x80, 60, 0]));
        }
        assert!(!sender.send([0x80, 61, 0]));
    }
}
