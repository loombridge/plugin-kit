//! The analyser allocates only in `new`.

use lspk_dsp::Analyser;

lspk_rt::install_trap!();

#[test]
fn pushing_and_reading_stay_off_the_heap() {
    let mut analyser = Analyser::new(512);
    let block: Vec<f32> = (0..64).map(|i| (i as f32 * 0.3).sin()).collect();
    let (mut scope, mut spectrum) = (vec![0.0; 256], vec![0.0; analyser.bins()]);
    lspk_rt::rt_test!({
        for _ in 0..16 {
            analyser.push(&block);
        }
        analyser.scope(&mut scope);
        analyser.spectrum(&mut spectrum);
    });
}
