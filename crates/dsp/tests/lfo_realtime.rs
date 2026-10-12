//! The LFOs touch no heap once made: every type and mode, both paths, with
//! note-ons, point modulation and shape edits inside the real-time section.

use lspk_dsp::lfo::{Lfo, LfoBank, LfoType, Mode, Point, Rate, Settings, Span, Transport};
use lspk_dsp::simd;

lspk_rt::install_trap!();

#[test]
fn processing_lfos_never_allocates() {
    const VOICES: usize = 32;
    const FRAMES: usize = 256;
    // The first call reads the environment, which allocates.
    let _ = simd::level();
    let settings = Settings {
        rate: Rate::Hz {
            hz: 9.0,
            x10: false,
        },
        delay: Span::Seconds(0.001),
        fade_in: Span::Seconds(0.002),
        smooth: 20.0,
        ..Settings::default()
    };
    let mut bank = LfoBank::new(Lfo::new(settings, 7), VOICES);
    let mut lone = Lfo::new(settings, 8);
    let mut voice = lone.voice(0);
    let mut out = vec![0.0; VOICES * FRAMES];
    let mut out_y = vec![0.0; VOICES * FRAMES];
    let mut solo = vec![0.0; FRAMES];
    let mut transport = Transport::new(48_000.0);
    lspk_rt::rt_test!({
        for kind in [
            LfoType::Drawn,
            LfoType::Path,
            LfoType::Lorenz,
            LfoType::Rossler,
            LfoType::SampleAndHold,
        ] {
            for mode in [Mode::Free, Mode::Restart, Mode::OneShot] {
                for shared in [false, true] {
                    bank.lfo.settings.kind = kind;
                    bank.lfo.settings.mode = mode;
                    bank.lfo.settings.shared = shared;
                    bank.lfo
                        .shape_mut()
                        .set_points(&[Point::new(0.0, 1.0), Point::new(1.0, 0.0)]);
                    for v in (0..VOICES).step_by(3) {
                        bank.trigger(v, v * 7);
                        bank.voice_mut(v).set_bus(1, 0.5);
                    }
                    bank.process(&transport, FRAMES, &mut out, Some(&mut out_y));
                    bank.process_scalar(&transport, FRAMES, &mut out, None);
                    lone.settings.kind = kind;
                    lone.settings.mode = mode;
                    let block = lone.prepare(&transport, FRAMES);
                    lone.render(&block, &mut voice, 0, &mut solo[..100], None);
                    lone.trigger(&block, &mut voice, 100);
                    lone.render(&block, &mut voice, 100, &mut solo[100..], None);
                    transport.advance(FRAMES);
                }
            }
        }
    });
    assert!(out.iter().all(|x| (0.0..=1.0).contains(x)));
}
