//! Огибающая громкости для волны редактора текстов.

use std::sync::atomic::AtomicBool;

use nyanoto_core::{PEAKS_PER_SECOND, peaks};
use nyanoto_testkit::{Scratch, encode_flac};

const RATE: u32 = 44_100;

#[test]
fn quiet_and_loud_parts_are_where_they_are() {
    // Секунда тишины, затем секунда синуса с амплитудой 0.5.
    let mut samples = vec![0i16; RATE as usize * 2];
    for frame in 0..RATE as usize {
        let value = ((frame as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin() * 16_384.0) as i16;
        samples.extend_from_slice(&[value, value]);
    }
    let scratch = Scratch::new("peaks");
    let path = scratch.write("wave.flac", &encode_flac(&samples, RATE).bytes);

    let peaks = peaks(&path, &AtomicBool::new(false)).unwrap().unwrap();
    let per_second = PEAKS_PER_SECOND as usize;
    assert!((peaks.peak.len() as i64 - 2 * per_second as i64).abs() <= 1, "{} столбиков", peaks.peak.len());
    assert!((peaks.duration_ms() - 2000).abs() <= 5);
    assert!(peaks.peak[..per_second - 1].iter().all(|peak| *peak == 0.0), "тишина в первой секунде");
    for (peak, rms) in peaks.peak[per_second + 1..].iter().zip(&peaks.rms[per_second + 1..]) {
        assert!((peak - 0.5).abs() < 0.02, "пик {peak}");
        assert!((rms - 0.354).abs() < 0.03, "среднеквадратичное {rms}");
    }
}

#[test]
fn stop_flag_interrupts() {
    let scratch = Scratch::new("peaks-stop");
    let path = scratch.write("wave.flac", &encode_flac(&vec![1000i16; RATE as usize * 2], RATE).bytes);
    assert_eq!(peaks(&path, &AtomicBool::new(true)).unwrap(), None);
}
