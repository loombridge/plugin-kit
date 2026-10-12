//! Envelopes allocate nothing once made: every event and every sample runs
//! inside the allocation trap.

use lspk_dsp::envelope::{Timing, Trigger};
use lspk_dsp::{Envelope, EnvelopeBank, EnvelopeParams};

lspk_rt::install_trap!();

#[test]
fn events_and_samples_allocate_nothing() {
    {
        // Made before the real-time section, as a plug-in makes them at
        // activation.
        let timing = Timing::new(48_000.0);
        let mut bank = EnvelopeBank::new(32, EnvelopeParams::AMP, timing);
        let mut envelope = Envelope::new(EnvelopeParams::MODULATION, timing);
        let mut out = vec![0.0; bank.stride() * 256];
        let mut single = vec![0.0; 256];
        lspk_rt::rt_test!({
            for block in 0..200 {
                let voice = block % 32;
                match block % 4 {
                    0 => bank.trigger(voice, Trigger::New),
                    1 => bank.trigger(voice, Trigger::Retrigger),
                    2 => bank.release(voice),
                    _ => bank.set_params(voice, EnvelopeParams::MODULATION),
                }
                bank.set_timing(timing.with_scale(1.0 + (block % 3) as f64));
                bank.process(&mut out);
                envelope.trigger(Trigger::New);
                envelope.process(&mut single);
                envelope.release();
                envelope.process(&mut single);
            }
        });
    }
}
