//! Rational-ratio polyphase resampling for the output path.
//!
//! librespot decodes at 44.1 kHz. Windows' shared-mode audio engine runs at
//! whatever rate the user configured — 48 kHz on most machines — and cpal only
//! ever opens shared-mode streams, so the device rate is not negotiable and
//! every sample is resampled on the way out.
//!
//! rodio does that conversion with `SampleRateConverter`, which its own
//! documentation describes as "simple linear interpolation for up-sampling".
//! Straight-line interpolation between adjacent samples is a poor
//! reconstruction of a band-limited signal: the error grows with the square of
//! frequency, so it is inaudible in the bass and severe in the treble. Measured
//! against the analytically exact answer at 44.1 -> 48 kHz it lands at -94.6 dB
//! at 100 Hz but only -15.0 dB at 10 kHz and -8.5 dB at 15 kHz. That is the
//! loudest thing in the signal path by a wide margin — far above the Ogg Vorbis
//! quantisation noise the player exists to deliver faithfully.
//!
//! This module replaces it. Output frame `p` is reconstructed at input time
//! `p * input_rate / output_rate` by a windowed-sinc kernel, evaluated through
//! a precomputed polyphase bank so the per-sample cost is [`TAPS`] multiply-adds
//! and no transcendental calls. The same measurement puts it at -133 dB at
//! 100 Hz, -116 dB at 10 kHz and -110 dB at 19 kHz — below 16-bit resolution
//! across the whole audible band, and far below what the codec itself leaves.
//!
//! Two properties beyond fidelity fall out of doing it here:
//!
//! - **No drift.** Position is tracked as an exact rational (`phase` over
//!   `phase_count`, advanced by `step`), so the output frame count is
//!   determined to the frame for any input length. The previous arrangement had
//!   to stop rodio's converter from being rebuilt mid-stream, because each
//!   rebuild leaked a fraction of a frame; that whole class of bug is gone
//!   rather than managed.
//! - **No downstream resampling.** The native callback receives device-rate
//!   samples and only converts representation and channel layout.
//!
//! On AVX2 x86-64, 44.1 → 48 kHz stereo evaluates independent output samples
//! in parallel. This changes neither the tap reduction order nor the streaming
//! window: any output that doesn't fit a batch is emitted individually.

use std::f64::consts::PI;

/// Input frames spanned by each output sample. With [`KAISER_BETA`] this sets
/// how steeply the filter can turn over at the cutoff, and 64 is comfortably
/// enough: the measured reconstruction error stays below -109 dB across the
/// whole audible band, which is two orders of magnitude under 16-bit
/// resolution. The cost is 64 multiply-adds per output sample per channel —
/// 6.1 M/s at 48 kHz stereo, and on the player thread rather than the audio
/// callback, so it cannot contribute to an underrun.
const TAPS: usize = 64;

/// Taps that sit before the output instant. The kernel is symmetric about the
/// instant it reconstructs, so this centres the window on the interval the
/// output falls in and the filter adds no delay — only `TAPS - CENTER` frames
/// of look-ahead (~0.75 ms) before the first output can be emitted.
const CENTER: usize = TAPS / 2 - 1;

/// Kaiser shape parameter: how deep the stopband goes, and therefore — for a
/// fixed [`TAPS`] — how wide the transition around the cutoff has to be.
///
/// Swept against the measurement in
/// `reconstruction_error_stays_below_the_codec_floor`. Lower values keep the
/// passband flat a little closer to Nyquist but raise the floor everywhere:
/// 9.5 measures -103 dB at 10 kHz where 11.0 measures -116 dB. The only thing
/// 11.0 gives up is 0.009 dB of droop at 20 kHz, in a band Spotify's Ogg Vorbis
/// has already discarded.
const KAISER_BETA: f64 = 11.0;

/// Cap on precomputed phases, bounding the coefficient table at 1 MB.
///
/// The phase count is `output_rate / gcd(input_rate, output_rate)`: 160 for
/// 44.1 -> 48 kHz, 320 for 96 kHz, 640 for 192 kHz. Every rate a sound device
/// actually exposes is a multiple of 100, so the cap cannot bind in practice.
/// If some device ever did report a rate near-coprime with 44100, only the
/// *coefficient lookup* quantises — position stays exact rational arithmetic,
/// so the rate stays exact and only reconstruction loses a little accuracy.
const MAX_PHASES: usize = 4096;

/// Streaming resampler for one fixed rate pair.
pub struct Resampler {
    bank: FilterBank,
    /// Phases actually stored: `min(phase_count, MAX_PHASES)`.
    table_phases: usize,
    /// Input frames advanced per output frame, as `step` over `phase_count`.
    step: usize,
    phase_count: usize,
    channels: usize,
    /// Interleaved input: `CENTER` frames of left context, then everything not
    /// yet consumed.
    history: Vec<f32>,
    /// Frame index into `history` that the next output sample sits on.
    cursor: usize,
    /// Fractional part of that position, over `phase_count`.
    phase: usize,
}

enum FilterBank {
    /// `table_phases * TAPS` coefficients, oldest tap first.
    Scalar(Vec<f32>),
    #[cfg(target_arch = "x86_64")]
    Stereo48k(&'static stereo_48k::Bank),
}

impl Resampler {
    /// Builds a resampler for `input_rate` to `output_rate`, or `None` when the
    /// rates match and the samples should pass through untouched.
    pub fn new(input_rate: u32, output_rate: u32, channels: u16) -> Option<Resampler> {
        if input_rate == output_rate || input_rate == 0 || output_rate == 0 || channels == 0 {
            return None;
        }

        let divisor = gcd(input_rate as usize, output_rate as usize);
        let step = input_rate as usize / divisor;
        let phase_count = output_rate as usize / divisor;
        let table_phases = phase_count.min(MAX_PHASES);

        // The cutoff belongs at the lower of the two Nyquist limits, in cycles
        // per input sample. Upsampling must reject the input's images, which
        // begin just above its own Nyquist; downsampling must additionally keep
        // the input's own content from folding into the shorter output band.
        // The transition band straddles it: content below stays flat, the
        // images above are attenuated by the stopband depth [`KAISER_BETA`]
        // buys.
        let cutoff = 0.5 * f64::from(input_rate.min(output_rate)) / f64::from(input_rate);

        #[cfg(target_arch = "x86_64")]
        let bank = if (input_rate, output_rate, channels) == (44_100, 48_000, 2)
            && is_x86_feature_detected!("avx2")
        {
            FilterBank::Stereo48k(&stereo_48k::BANK)
        } else {
            FilterBank::Scalar(build_bank(table_phases, cutoff))
        };
        #[cfg(not(target_arch = "x86_64"))]
        let bank = FilterBank::Scalar(build_bank(table_phases, cutoff));

        let mut resampler = Resampler {
            bank,
            table_phases,
            step,
            phase_count,
            channels: channels as usize,
            history: Vec::new(),
            cursor: 0,
            phase: 0,
        };
        resampler.reset();
        Some(resampler)
    }

    /// Discards filter state at a discontinuity (seek or explicit selection),
    /// so old audio cannot bleed into what follows, and restarts phase at zero.
    pub fn reset(&mut self) {
        self.history.clear();
        // A track begins from silence, so zero-filling the left context is not
        // an approximation — it is what precedes the first sample.
        self.history.resize(CENTER * self.channels, 0.0);
        self.cursor = CENTER;
        self.phase = 0;
    }

    /// Upper bound on the output `input_samples` can produce, for sizing the
    /// destination. Deliberately generous rather than tight: the exact count
    /// depends on how much history is already buffered.
    fn output_capacity(&self, input_samples: usize) -> usize {
        input_samples * self.phase_count / self.step + self.channels
    }

    /// Consumes `input` (interleaved) and appends every output frame it
    /// completes to `out`. Frames the filter still needs look-ahead for stay
    /// buffered until the next call.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        out.reserve(self.output_capacity(input.len()));
        self.history.extend_from_slice(input);

        self.emit_available(out);

        // Everything before the current left context is spent.
        let spent = self.cursor - CENTER;
        self.history.drain(..spent * self.channels);
        self.cursor = CENTER;
    }

    fn emit_available(&mut self, out: &mut Vec<f32>) {
        let phases = match &self.bank {
            FilterBank::Scalar(phases) => phases,
            #[cfg(target_arch = "x86_64")]
            FilterBank::Stereo48k(bank) => {
                // SAFETY: new only selects this bank for AVX2 stereo, step 147/160.
                unsafe { bank.emit(&self.history, &mut self.cursor, &mut self.phase, out) };
                return;
            }
        };
        let frames = self.history.len() / self.channels;
        while self.cursor + TAPS - CENTER <= frames {
            let base = (self.cursor - CENTER) * self.channels;
            let window = &self.history[base..base + TAPS * self.channels];
            let index = if self.table_phases == self.phase_count {
                self.phase
            } else {
                self.phase * self.table_phases / self.phase_count
            };
            let coefficients = &phases[index * TAPS..(index + 1) * TAPS];

            for channel in 0..self.channels {
                let mut sum = 0.0f32;
                for (tap, coefficient) in coefficients.iter().enumerate() {
                    sum += window[tap * self.channels + channel] * coefficient;
                }
                out.push(sum);
            }

            let advanced = self.phase + self.step;
            self.cursor += advanced / self.phase_count;
            self.phase = advanced % self.phase_count;
        }
    }

    /// Emit the remaining input frames using silence as EOF look-ahead.
    /// Reset afterwards so a later Play cannot emit the padding as old audio.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        let frames = self.history.len() / self.channels;
        let pending = frames.saturating_sub(self.cursor);
        if pending != 0 {
            // One fewer padded frame emits precisely the original remainder.
            self.history.resize(self.history.len() + (TAPS - CENTER - 1) * self.channels, 0.0);
            self.process(&[], out);
        }
        self.reset();
    }
}

fn gcd(mut left: usize, mut right: usize) -> usize {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

/// Windowed-sinc coefficients for every phase, each normalised to unity gain.
///
/// `cutoff` is in cycles per *input* sample. Normalising per phase rather than
/// globally matters: without it the bank's DC gain varies slightly from phase
/// to phase, which modulates the signal at the phase-cycle rate — an audible
/// artefact exactly where a resampler is supposed to be silent.
fn build_bank(table_phases: usize, cutoff: f64) -> Vec<f32> {
    let half_width = (TAPS / 2) as f64;
    let beta_scale = bessel_i0(KAISER_BETA);
    let mut bank = Vec::with_capacity(table_phases * TAPS);

    for phase in 0..table_phases {
        let fraction = phase as f64 / table_phases as f64;
        let start = bank.len();
        let mut sum = 0.0f64;

        for tap in 0..TAPS {
            // Distance, in input samples, from this tap to the instant being
            // reconstructed. Positive means the tap precedes it.
            let offset = CENTER as f64 - tap as f64 + fraction;
            let value = kernel(offset, half_width, cutoff, beta_scale);
            sum += value;
            bank.push(value as f32);
        }

        if sum.abs() > f64::EPSILON {
            for coefficient in &mut bank[start..] {
                *coefficient = (f64::from(*coefficient) / sum) as f32;
            }
        }
    }

    bank
}

fn kernel(offset: f64, half_width: f64, cutoff: f64, beta_scale: f64) -> f64 {
    let ratio = offset / half_width;
    if ratio.abs() >= 1.0 {
        return 0.0;
    }
    let window = bessel_i0(KAISER_BETA * (1.0 - ratio * ratio).sqrt()) / beta_scale;
    let x = 2.0 * cutoff * offset;
    let sinc = if x.abs() < 1e-12 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    };
    2.0 * cutoff * sinc * window
}

/// Modified Bessel function of the first kind, order zero, by its power series.
/// The Kaiser window only ever needs `0 ..= KAISER_BETA`, where this converges
/// in a handful of terms.
fn bessel_i0(x: f64) -> f64 {
    let quarter_square = x * x / 4.0;
    let mut term = 1.0f64;
    let mut sum = 1.0f64;
    for k in 1..64 {
        term *= quarter_square / (k as f64 * k as f64);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

#[cfg(target_arch = "x86_64")]
mod stereo_48k {
    use std::{arch::x86_64::*, sync::LazyLock};

    use super::{CENTER, TAPS};

    const STEP: usize = 147;
    const PHASES: usize = 160;
    const BATCH: usize = 16;

    pub(super) struct Bank {
        pub(super) phases: Vec<f32>,
        tiles: Box<[Tile]>,
    }

    pub(super) static BANK: LazyLock<Bank> = LazyLock::new(|| {
        let phases = super::build_bank(PHASES, 0.5); // exact cutoff for upsampling
        // A batch advances the phase by a multiple of 16. Only ten starting
        // phases need tiles, shared across streams (~82 KiB altogether).
        let tiles = (0..PHASES).step_by(BATCH).map(|p| Tile::new(p, &phases)).collect();
        Bank { phases, tiles }
    });

    // Each of four vectors covers four consecutive stereo outputs. Their
    // source positions usually advance by one, but may repeat once in those
    // four frames. Shift the coefficients for the lagging lanes by one tap:
    // the inner loop can then read consecutive samples without any gathers.
    #[repr(C, align(32))]
    struct Tile {
        coeff: [[[f32; 8]; 4]; TAPS + 1],
        offset: [usize; 4],
        lag_mask: [[i32; 8]; 4],
    }

    impl Tile {
        fn new(phase: usize, phases: &[f32]) -> Self {
            let mut tile = Self {
                coeff: [[[0.0; 8]; 4]; TAPS + 1],
                offset: [0; 4],
                lag_mask: [[0; 8]; 4],
            };
            for group in 0..4 {
                let start = phase + group * 4 * STEP;
                let base = start / PHASES;
                tile.offset[group] = base * 2;
                for frame in 0..4 {
                    let position = start + frame * STEP;
                    let lag = base + frame - position / PHASES;
                    debug_assert!(lag <= 1);
                    let lanes = frame * 2..frame * 2 + 2;
                    tile.lag_mask[group][lanes.clone()].fill(-(lag as i32));
                    let index = position % PHASES * TAPS;
                    for (tap, &coefficient) in phases[index..index + TAPS].iter().enumerate() {
                        tile.coeff[tap + 1 - lag][group][lanes.clone()].fill(coefficient);
                    }
                }
            }
            tile
        }
    }

    impl Bank {
        #[target_feature(enable = "avx2")]
        pub(super) unsafe fn emit(
            &self,
            history: &[f32],
            cursor: &mut usize,
            phase: &mut usize,
            out: &mut Vec<f32>,
        ) {
            let frames = history.len() / 2;
            while *cursor + TAPS - CENTER <= frames {
                let base = *cursor - CENTER;
                let advance = if *phase % BATCH == 0 && frames - base >= TAPS + BATCH - 1 {
                    let tile = &self.tiles[*phase / BATCH];
                    out.reserve(BATCH * 2); // finish() may supply padding via history, not input
                    // SAFETY: there are TAPS + BATCH - 1 complete stereo frames.
                    // Group g starts at most 4*g frames later (STEP < PHASES),
                    // so even the widest load at tap TAPS-1 stays in history.
                    // reserve() guarantees room for exactly four 8-float stores.
                    unsafe {
                        tile.write(history.as_ptr().add(base * 2), out.as_mut_ptr().add(out.len()));
                        out.set_len(out.len() + BATCH * 2);
                    }
                    STEP * BATCH
                } else {
                    // Unaligned prefix and short suffix, including startup and EOF.
                    // Never save a ready frame for a future SIMD batch.
                    let window = &history[base * 2..(base + TAPS) * 2];
                    let index = *phase * TAPS;
                    let mut pair = [0.0; 2];
                    for (sample, &c) in window.chunks_exact(2).zip(&self.phases[index..index + TAPS]) {
                        pair[0] += sample[0] * c;
                        pair[1] += sample[1] * c;
                    }
                    out.extend_from_slice(&pair);
                    STEP
                };
                let advanced = *phase + advance;
                *cursor += advanced / PHASES;
                *phase = advanced % PHASES;
            }
        }
    }

    impl Tile {
        #[target_feature(enable = "avx2")]
        unsafe fn write(&self, src: *const f32, dst: *mut f32) {
            let mut sum = [_mm256_setzero_ps(); 4];
            let mut lag = [_mm256_setzero_ps(); 4];
            let previous = _mm256_setr_epi32(0, 1, 0, 1, 2, 3, 4, 5);
            // SAFETY: emit checked the input and output spans. Each coefficient
            // vector is 32-byte aligned; sample loads/stores may be unaligned.
            unsafe {
                for group in 0..4 {
                    lag[group] = _mm256_castsi256_ps(_mm256_loadu_si256(self.lag_mask[group].as_ptr().cast()));
                    let x = _mm256_permutevar8x32_ps(_mm256_loadu_ps(src.add(self.offset[group])), previous);
                    let c = _mm256_load_ps(self.coeff[0][group].as_ptr());
                    // Only lagging lanes start here; others start the loop at +0.
                    sum[group] = _mm256_and_ps(lag[group], _mm256_add_ps(sum[group], _mm256_mul_ps(x, c)));
                }
                for tap in 0..TAPS - 1 {
                    for (group, acc) in sum.iter_mut().enumerate() {
                        let x = _mm256_loadu_ps(src.add(self.offset[group] + tap * 2));
                        let c = _mm256_load_ps(self.coeff[tap + 1][group].as_ptr());
                        *acc = _mm256_add_ps(*acc, _mm256_mul_ps(x, c));
                    }
                }
                for group in 0..4 {
                    let x = _mm256_loadu_ps(src.add(self.offset[group] + (TAPS - 1) * 2));
                    let c = _mm256_load_ps(self.coeff[TAPS][group].as_ptr());
                    let last = _mm256_add_ps(sum[group], _mm256_mul_ps(x, c));
                    // Lagging lanes already accumulated their last tap. Every
                    // retained lane did 64 separate multiply/adds, oldest first.
                    _mm256_storeu_ps(dst.add(group * 8), _mm256_blendv_ps(last, sum[group], lag[group]));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IN_RATE: u32 = 44_100;
    const OUT_RATE: u32 = 48_000;
    const CHANNELS: u16 = 2;

    /// Keep the original, general convolution as a separately selected oracle.
    fn scalar_48k() -> Resampler {
        let mut r = Resampler::new(IN_RATE, OUT_RATE, CHANNELS).unwrap();
        r.bank = FilterBank::Scalar(build_bank(160, 0.5));
        r
    }

    fn stereo_noise(frames: usize) -> Vec<f32> {
        let mut seed = 527_946_763u32;
        (0..frames * 2).map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as i32 as f32) / i32::MAX as f32
        }).collect()
    }

    fn assert_bits(actual: &[f32], expected: &[f32]) {
        assert_eq!(actual.len(), expected.len());
        for (n, (a, b)) in actual.iter().zip(expected).enumerate() {
            assert_eq!(a.to_bits(), b.to_bits(), "different bits at sample {n}: {a} != {b}");
        }
    }

    fn assert_state(actual: &Resampler, expected: &Resampler) {
        assert_eq!((actual.cursor, actual.phase), (expected.cursor, expected.phase));
        assert_bits(&actual.history, &expected.history);
    }

    fn compare_stream(
        actual: &mut Resampler, scalar: &mut Resampler, input: &[f32], packets: &[usize], eof: bool,
    ) {
        // Nonempty destinations exercise append semantics, including the SIMD stores.
        let (mut output, mut reference) = (vec![19.75], vec![19.75]);
        let mut frame = 0;
        for &packet in packets.iter().cycle() {
            let next = (frame + packet).min(input.len() / 2);
            let samples = &input[frame * 2..next * 2];
            let before = output.len();
            actual.process(samples, &mut output);
            scalar.process(samples, &mut reference);
            // floor(p * 147 / 160) + 32 must be strictly less than the input
            // received. Check that deadline after *each* call, not just at EOF.
            let ready = next.saturating_sub(32);
            assert_eq!(output.len(), 1 + 2 * (ready * 160).div_ceil(147), "at input frame {next}");
            assert_bits(&output[before..], &reference[before..]);
            assert_state(actual, scalar);
            frame = next;
            if frame == input.len() / 2 {
                break;
            }
        }
        if eof {
            for _ in 0..2 {
                let before = output.len();
                actual.finish(&mut output);
                scalar.finish(&mut reference);
                assert_eq!(output.len(), 1 + 2 * (frame * 160).div_ceil(147));
                assert_bits(&output[before..], &reference[before..]);
                assert_state(actual, scalar);
            }
        } else {
            actual.reset(); // seek with nonzero phase and live history
            scalar.reset();
            assert_state(actual, scalar);
        }
        assert_eq!(output[0], 19.75);
    }

    #[test]
    fn stereo_dispatch_preserves_bits_and_every_frame_deadline() {
        let mut actual = Resampler::new(IN_RATE, OUT_RATE, CHANNELS).unwrap();
        #[cfg(target_arch = "x86_64")]
        assert_eq!(matches!(actual.bank, FilterBank::Stereo48k(_)), is_x86_feature_detected!("avx2"));
        let mut scalar = scalar_48k();
        let noise = stereo_noise(4_097);
        let mut impulses = vec![0.0; noise.len()];
        for boundary in [1, 16, 32, 33, 47, 64, 147, 160, 1024, 4096] {
            for n in [boundary - 1, boundary] {
                impulses[n * 2 + n % 2] = if n % 2 == 0 { 1.0 } else { -0.5 };
            }
        }
        for schedule in [&[1][..], &[0, 1, 31, 0, 2, 15, 16, 17, 160, 409], &[1024], &[4097]] {
            compare_stream(&mut actual, &mut scalar, &noise, schedule, false);
            compare_stream(&mut actual, &mut scalar, &impulses, schedule, true);
            compare_stream(&mut actual, &mut scalar, &noise, schedule, true);
        }
        for frames in [0, 1, 31, 32, 33, 34, 46, 47, 48, 49, 64, 147, 160] {
            compare_stream(&mut actual, &mut scalar, &noise[..frames * 2], &[0, 17], true);
        }
    }

    #[test]
    fn other_rates_and_channel_layouts_keep_the_general_filter() {
        for (input, output, channels) in [(44_100, 48_000, 1), (44_100, 48_000, 6),
            (48_000, 44_100, 2), (44_100, 96_000, 2), (88_200, 96_000, 2)] {
            assert!(matches!(Resampler::new(input, output, channels).unwrap().bank, FilterBank::Scalar(_)));
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    #[ignore = "release-only throughput measurement"]
    fn benchmark_stereo_packets() {
        use std::{hint::black_box, time::Instant};

        assert!(!cfg!(debug_assertions), "run with --release");
        assert!(is_x86_feature_detected!("avx2"), "requires AVX2 for the comparison");
        let source = stereo_noise(16_384);
        for packet in [1, 16, 1024] {
            let measure = |r: &mut Resampler| {
                let mut out = Vec::new();
                let samples = packet * 2;
                r.process(&source[..samples], &mut out); // warm production buffers
                r.reset();
                let iterations = 32 * IN_RATE as usize / packet;
                let mut emitted = 0;
                let start = Instant::now();
                for n in 0..iterations {
                    let offset = n % (source.len() / samples) * samples;
                    out.clear();
                    r.process(black_box(&source[offset..offset + samples]), &mut out);
                    emitted += out.len();
                    black_box(out.as_slice());
                }
                out.clear();
                r.finish(&mut out);
                emitted += out.len();
                black_box(out.as_slice());
                let elapsed = start.elapsed();
                assert_eq!(emitted, 2 * (iterations * packet * 160).div_ceil(147));
                elapsed.as_secs_f64() * 1000.0 * f64::from(OUT_RATE) * 2.0 / emitted as f64
            };
            let mut actual = Resampler::new(IN_RATE, OUT_RATE, CHANNELS).unwrap();
            let mut scalar = scalar_48k();
            let (mut baseline, mut dispatched) = ([0.0; 11], [0.0; 11]);
            for n in 0..baseline.len() {
                if n % 2 == 0 {
                    baseline[n] = measure(&mut scalar);
                    dispatched[n] = measure(&mut actual);
                } else {
                    dispatched[n] = measure(&mut actual);
                    baseline[n] = measure(&mut scalar);
                }
            }
            baseline.sort_by(f64::total_cmp);
            dispatched.sort_by(f64::total_cmp);
            let (old, new) = (baseline[5], dispatched[5]);
            eprintln!("{packet:4} frames/packet: scalar {old:.3}, dispatched {new:.3} ms/output-second ({:.2}x)", old / new);
        }
    }

    #[test]
    fn eof_flush_emits_every_remaining_frame_once_and_resets_history() {
        for (input_rate, output_rate) in [(44_100, 48_000), (48_000, 44_100), (44_100, 96_000)] {
            for frames in [1, 17, 33, 137, 4_410] {
                let mut resampler = Resampler::new(input_rate, output_rate, CHANNELS).unwrap();
                let mut output = Vec::new();
                resampler.process(&vec![0.25; frames * CHANNELS as usize], &mut output);
                resampler.finish(&mut output);
                let expected = (frames * output_rate as usize).div_ceil(input_rate as usize);
                assert_eq!(output.len(), expected * CHANNELS as usize);
                let flushed = output.len();
                resampler.finish(&mut output);
                assert_eq!(output.len(), flushed, "EOF must not emit padding twice");
                let mut silence = Vec::new();
                resampler.process(&vec![0.0; 100 * CHANNELS as usize], &mut silence);
                resampler.finish(&mut silence);
                assert!(silence.iter().all(|sample| *sample == 0.0), "old audio must not bleed into Play");
            }
        }
    }

    fn resample_all(resampler: &mut Resampler, input: &[f32], chunk: usize) -> Vec<f32> {
        let mut out = Vec::new();
        for piece in input.chunks(chunk) {
            resampler.process(piece, &mut out);
        }
        out
    }

    /// A tone is the one signal whose resampled form is known exactly, so the
    /// error against it is the reconstruction error with no alignment or
    /// windowing ambiguity anywhere in the measurement.
    fn error_db(resampled: &[f32], freq: f64) -> f64 {
        let frames = resampled.len() / CHANNELS as usize;
        let skip = OUT_RATE as usize / 10;
        let (mut signal, mut noise) = (0.0f64, 0.0f64);
        for p in skip..frames - skip {
            let want = (std::f64::consts::TAU * freq * p as f64 / f64::from(OUT_RATE)).sin();
            let got = f64::from(resampled[p * CHANNELS as usize]);
            signal += want * want;
            noise += (got - want) * (got - want);
        }
        10.0 * (noise / signal).log10()
    }

    fn tone(freq: f64, seconds: usize) -> Vec<f32> {
        let frames = IN_RATE as usize * seconds;
        let mut out = Vec::with_capacity(frames * CHANNELS as usize);
        for n in 0..frames {
            let s = (std::f64::consts::TAU * freq * n as f64 / f64::from(IN_RATE)).sin() as f32;
            out.push(s);
            out.push(s);
        }
        out
    }

    /// The whole point of the module: reconstruction must sit far below the
    /// codec noise it carries, at every frequency — not just in the bass, which
    /// is the one band linear interpolation also gets right.
    /// For reference, the same measurement of what rodio does instead:
    /// -94.6 dB at 100 Hz, -54.6 dB at 1 kHz, -26.8 dB at 5 kHz, -15.0 dB at
    /// 10 kHz, -8.5 dB at 15 kHz.
    #[test]
    fn reconstruction_error_stays_below_the_codec_floor() {
        for freq in [100.0, 1_000.0, 5_000.0, 10_000.0, 15_000.0, 19_000.0] {
            let mut resampler = Resampler::new(IN_RATE, OUT_RATE, CHANNELS).expect("rates differ");
            let out = resample_all(&mut resampler, &tone(freq, 2), 1024);
            let db = error_db(&out, freq);
            assert!(
                db < -105.0,
                "reconstruction error at {freq} Hz was {db:.1} dB, expected below -105 dB"
            );
        }
    }

    /// 20 kHz sits inside the transition band, where the error above is droop
    /// rather than noise. It has to stay small enough to be meaningless: the
    /// whole point of resampling correctly is not to trade one audible defect
    /// for another.
    #[test]
    fn the_top_of_the_band_is_not_rolled_off() {
        let mut resampler = Resampler::new(IN_RATE, OUT_RATE, CHANNELS).expect("rates differ");
        let out = resample_all(&mut resampler, &tone(20_000.0, 2), 1024);
        let error = 10f64.powf(error_db(&out, 20_000.0) / 20.0);
        let droop_db = -20.0 * (1.0 - error).log10();
        assert!(
            droop_db < 0.05,
            "20 kHz is {droop_db:.3} dB down; the passband must stay flat"
        );
    }

    /// Drift is what sent us here in the first place. The frame count must be
    /// exactly the ratio, and must not depend on how the input was chunked —
    /// decoder packet sizes vary with the material.
    #[test]
    fn output_frame_count_is_exact_and_chunk_independent() {
        let input = tone(1_000.0, 10);
        let frames_in = input.len() / CHANNELS as usize;
        let ideal = frames_in as u64 * u64::from(OUT_RATE) / u64::from(IN_RATE);

        for chunk in [256usize, 1152, 4096, 32_768] {
            let mut resampler = Resampler::new(IN_RATE, OUT_RATE, CHANNELS).expect("rates differ");
            let out = resample_all(&mut resampler, &input, chunk * CHANNELS as usize);
            let frames_out = (out.len() / CHANNELS as usize) as u64;
            // The only shortfall permitted is the look-ahead the filter has not
            // been fed yet: a fixed handful of frames, not a growing fraction.
            let shortfall = ideal.saturating_sub(frames_out);
            assert!(
                shortfall <= TAPS as u64,
                "chunk {chunk}: produced {frames_out} frames, ideal {ideal}"
            );
            assert!(
                frames_out <= ideal,
                "chunk {chunk}: produced more than ideal"
            );
        }
    }

    /// Silence in must be silence out. A bank whose phases did not each sum to
    /// unity would instead ring at the phase-cycle rate.
    #[test]
    fn every_phase_has_unity_gain() {
        let resampler = Resampler::new(IN_RATE, OUT_RATE, CHANNELS).expect("rates differ");
        let phases = match &resampler.bank {
            FilterBank::Scalar(phases) => phases,
            #[cfg(target_arch = "x86_64")]
            FilterBank::Stereo48k(bank) => &bank.phases,
        };
        for phase in 0..resampler.table_phases {
            let sum: f32 = phases[phase * TAPS..(phase + 1) * TAPS]
                .iter()
                .sum();
            assert!(
                (sum - 1.0).abs() < 1e-5,
                "phase {phase} sums to {sum}, which would modulate the signal"
            );
        }
    }

    /// Matching rates must not build a filter at all — running audio through a
    /// resampler that has nothing to do would only add its passband ripple.
    #[test]
    fn matching_rates_need_no_resampler() {
        assert!(Resampler::new(IN_RATE, IN_RATE, CHANNELS).is_none());
    }

    /// Channels must stay independent: a bug in the interleave arithmetic would
    /// blend left into right, which is inaudible on the mono test tones above.
    #[test]
    fn channels_do_not_bleed_into_each_other() {
        let frames = IN_RATE as usize;
        let mut input = Vec::with_capacity(frames * 2);
        for n in 0..frames {
            input.push((std::f64::consts::TAU * 1_000.0 * n as f64 / 44_100.0).sin() as f32);
            input.push(0.0);
        }
        let mut resampler = Resampler::new(IN_RATE, OUT_RATE, CHANNELS).expect("rates differ");
        let out = resample_all(&mut resampler, &input, 1024);

        let right_peak = out
            .iter()
            .skip(1)
            .step_by(2)
            .fold(0.0f32, |peak, s| peak.max(s.abs()));
        assert!(right_peak < 1e-4, "silent channel picked up {right_peak}");
    }

    /// Rates the reference rig will never see must still resample rather than
    /// panic or silently degrade to nothing.
    #[test]
    fn handles_the_other_common_device_rates() {
        for rate in [48_000u32, 88_200, 96_000, 192_000, 32_000, 22_050] {
            let mut resampler =
                Resampler::new(IN_RATE, rate, CHANNELS).expect("rates differ from 44100");
            let mut out = Vec::new();
            resampler.process(&tone(1_000.0, 1), &mut out);
            let frames = (out.len() / CHANNELS as usize) as f64;
            let expected = f64::from(rate);
            // The filter still holds its look-ahead, which is a fixed number of
            // *input* frames and so scales into more output frames the higher
            // the device rate.
            let lookahead = TAPS as f64 * expected / f64::from(IN_RATE);
            assert!(
                frames <= expected && expected - frames <= lookahead,
                "{rate} Hz produced {frames} frames in one second, expected \
                 {expected} less at most {lookahead:.0} frames of look-ahead"
            );
        }
    }
}
