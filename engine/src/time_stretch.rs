use librespot_playback::{NUM_CHANNELS, SAMPLE_RATE};
use renderer_engine::protocol::{TimeRange, TrackEdit};

const CHANNELS: usize = NUM_CHANNELS as usize;
/// OLA analysis window (Chromium AudioRendererAlgorithm: 20 ms).
const OLA_WINDOW_MS: i64 = 20;
/// Candidate-start search span (Chromium: 30 ms).
const SEARCH_INTERVAL_MS: i64 = 30;
/// Half-width, in frames, of the exclusion band around the previously chosen
/// block; re-picking it repeats audio and reads as buzz. Heuristic constant
/// from Chromium.
const EXCLUDE_HALF_FRAMES: i64 = 80;
/// Chromium's `kSearchDecimation`. The broad pass evaluates every fifth
/// *candidate position* at full correlation resolution, interpolates the peak,
/// and then sweeps ±`SEARCH_DECIMATION` around it exhaustively. Chromium's own
/// note on the value: it is "a compromise between complexity reduction and
/// search accuracy", with no proof of optimality, and larger factors made "the
/// rate of missing the optimal index ... significant".
///
/// Only candidate *positions* are decimated. The correlation itself is never
/// subsampled: an earlier revision of this port also stepped over samples
/// inside the similarity measure, which aliases the similarity curve and can
/// hand back an offset that is not the true peak. See `Search::similarity`.
const SEARCH_DECIMATION: usize = 5;
/// Chromium's `kEpsilon` in `MultiChannelSimilarityMeasure`. It sits *inside*
/// the square root, so a silent block normalises by 1e-6 rather than by the
/// clamped 1e-12 an outside-the-root guard would give.
const SIMILARITY_EPSILON: f32 = 1.0e-12;

#[derive(Clone, Debug)]
pub struct PipelineConfig {
    pub edit: Option<TrackEdit>,
    pub speed: f32,
    pub position_ms: u32,
    /// One-based audible pass through a finite loop. Explicit selections and
    /// user seeks derive it from position; non-seek player handovers preserve
    /// the current pass and an internal loop jump increments it.
    pub loop_pass: u32,
}

impl PipelineConfig {
    pub fn active(&self) -> bool {
        self.edit.as_ref().is_some_and(|edit| !edit.is_empty()) || self.speed != 1.0
    }
}

pub struct AudioPipeline {
    edit: Option<SampleEdit>,
    stretcher: Option<Wsola>,
    filtered: Vec<f32>,
    loop_sent: bool,
}

impl AudioPipeline {
    pub fn new(config: &PipelineConfig) -> Self {
        Self {
            edit: config
                .edit
                .as_ref()
                .filter(|edit| !edit.is_empty())
                .map(|edit| SampleEdit::new(edit, config.position_ms, config.loop_pass)),
            stretcher: (config.speed != 1.0).then(|| Wsola::new(config.speed)),
            filtered: Vec::new(),
            loop_sent: false,
        }
    }

    /// Changes only the rate: sample-edit cursors and finite-loop state stay
    /// attached to the decoder stream rather than restarting at a UI position.
    pub fn set_speed(&mut self, speed: f32, pending: &mut Vec<f32>) {
        match self.stretcher.as_mut() {
            Some(stretcher) if speed == 1.0 => {
                stretcher.finish_into_passthrough(pending);
                self.stretcher = None;
            }
            Some(stretcher) => stretcher.set_speed(speed),
            None if speed != 1.0 => {
                let mut stretcher = Wsola::new(speed);
                stretcher.continuous_start = true;
                self.stretcher = Some(stretcher);
            }
            None => {}
        }
    }

    pub fn is_passthrough(&self) -> bool {
        self.edit.is_none() && self.stretcher.is_none()
    }

    pub fn process(&mut self, source_frame: u64, input: &[f32], output: &mut Vec<f32>) -> Option<u32> {
        output.clear();
        let loop_to = if let Some(edit) = &mut self.edit {
            if let Some(stretcher) = &mut self.stretcher {
                self.filtered.clear();
                let loop_to = edit.process(source_frame, input, &mut self.filtered);
                stretcher.process(&self.filtered, output);
                if loop_to.is_some() {
                    stretcher.finish(output);
                }
                loop_to
            } else {
                edit.process(source_frame, input, output)
            }
        } else if let Some(stretcher) = &mut self.stretcher {
            stretcher.process(input, output);
            None
        } else {
            output.extend_from_slice(input);
            None
        };

        if self.loop_sent {
            None
        } else if loop_to.is_some() {
            self.loop_sent = true;
            loop_to
        } else {
            None
        }
    }
    /// Completes a natural stream boundary. Unlike `reset_buffers`, this makes
    /// the stretcher's delayed overlap region audible before resetting it.
    pub fn finish(&mut self, output: &mut Vec<f32>) {
        output.clear();
        if let Some(stretcher) = &mut self.stretcher {
            stretcher.finish(output);
        }
    }

    pub fn reset_buffers(&mut self) {
        self.filtered.clear();
        self.loop_sent = false;
        if let Some(stretcher) = &mut self.stretcher {
            stretcher.reset();
        }
    }
}

struct SampleEdit {
    frame: u64,
    cuts: Vec<SampleRange>,
    cut_index: usize,
    loop_range: Option<SampleLoop>,
}

#[derive(Clone, Copy)]
struct SampleRange {
    start: u64,
    end: u64,
}

#[derive(Clone, Copy)]
struct SampleLoop {
    range: SampleRange,
    play_count: u32,
    pass: u32,
}

impl SampleEdit {
    fn new(edit: &TrackEdit, position_ms: u32, loop_pass: u32) -> Self {
        let cuts: Vec<_> = edit.cuts.iter().copied().map(sample_range).collect();
        let frame = ms_to_frame(position_ms);
        let cut_index = cuts.partition_point(|cut| cut.end <= frame);
        Self {
            frame,
            cuts,
            cut_index,
            loop_range: edit.loop_range.map(|loop_range| {
                let pass = if position_ms >= loop_range.end_ms {
                    loop_range.play_count.max(1)
                } else {
                    loop_pass.max(1)
                };
                SampleLoop {
                    range: sample_range(TimeRange {
                        start_ms: loop_range.start_ms,
                        end_ms: loop_range.end_ms,
                    }),
                    play_count: loop_range.play_count,
                    pass,
                }
            }),
        }
    }

    fn process(&mut self, source_frame: u64, input: &[f32], output: &mut Vec<f32>) -> Option<u32> {
        if self.frame != source_frame {
            self.frame = source_frame;
            self.cut_index = self.cuts.partition_point(|cut| cut.end <= source_frame);
        }
        debug_assert!(input.len().is_multiple_of(CHANNELS));
        output.reserve(input.len());
        for source in input.chunks_exact(CHANNELS) {
            if let Some(loop_range) = self.loop_range {
                if loop_range.pass < loop_range.play_count && self.frame >= loop_range.range.end {
                    return Some(frame_to_ms(loop_range.range.start));
                }
            }
            while self
                .cuts
                .get(self.cut_index)
                .is_some_and(|cut| self.frame >= cut.end)
            {
                self.cut_index += 1;
            }
            let cut = self
                .cuts
                .get(self.cut_index)
                .is_some_and(|cut| self.frame >= cut.start && self.frame < cut.end);
            if !cut {
                output.extend_from_slice(source);
            }
            self.frame += 1;
        }
        if let Some(loop_range) = self.loop_range {
            if loop_range.pass < loop_range.play_count && self.frame >= loop_range.range.end {
                return Some(frame_to_ms(loop_range.range.start));
            }
        }
        None
    }
}

fn sample_range(range: TimeRange) -> SampleRange {
    SampleRange {
        start: ms_to_frame(range.start_ms),
        end: ms_to_frame(range.end_ms),
    }
}

fn ms_to_frame(ms: u32) -> u64 {
    (u64::from(ms) * u64::from(SAMPLE_RATE) + 500) / 1_000
}

fn frames_for_ms(ms: i64) -> usize {
    ((ms * SAMPLE_RATE as i64 + 500) / 1_000) as usize
}

/// Copies `frames` interleaved frames starting at absolute frame `abs_start`
/// from the rolling input buffer, zero-filling anything before the start of
/// the stream. Free function so callers can mutably borrow other `Wsola`
/// scratch buffers without borrow-splitting fights.
///
/// This is Chromium's `PeekAudioWithZeroPrepend`: a block that *straddles* the
/// stream start keeps its real tail and only its leading frames are zeroed. An
/// earlier revision returned an all-zero block for any straddling read, which
/// disagreed with the similarity measure — that one did prepend only the
/// missing frames — so the block that won the search was not the block that
/// got overlap-added.
fn extract(input: &[f32], input_start: usize, abs_start: i64, frames: usize, dest: &mut Vec<f32>) {
    dest.clear();
    dest.resize(frames * CHANNELS, 0.0);
    let rel = abs_start - input_start as i64;
    let (prepend, start) = if rel < 0 {
        (((-rel) as usize).min(frames), 0)
    } else {
        (0, rel as usize)
    };
    let total = input.len() / CHANNELS;
    if start >= total {
        return;
    }
    let copy = (frames - prepend).min(total - start);
    if copy > 0 {
        dest[prepend * CHANNELS..(prepend + copy) * CHANNELS]
            .copy_from_slice(&input[start * CHANNELS..(start + copy) * CHANNELS]);
    }
}

fn frame_to_ms(frame: u64) -> u32 {
    ((frame * 1_000 + u64::from(SAMPLE_RATE) / 2) / u64::from(SAMPLE_RATE)).min(u64::from(u32::MAX))
        as u32
}

/// Streaming WSOLA stretcher ported from Chromium's AudioRendererAlgorithm
/// (media/filters/audio_renderer_algorithm.cc + wsola_internals.cc, BSD
/// 3-Clause, Copyright The Chromium Authors). Same structure: a 20 ms OLA
/// window at half-overlap with a periodic-Hann crossfade, a 30 ms search
/// region for the block that best continues the output, an exclusion band
/// around the previously chosen block (re-picking it buzzes), a
/// position-decimated search over full-resolution correlations against
/// precomputed moving block energies, and a target-to-optimal transition blend
/// so the natural continuation always leads. Adapted to a push-based packet
/// stream;
/// both channels share one search and one set of weights, so phase cannot
/// drift between them. The exact 1.0 transport path never constructs this
/// type.
pub struct Wsola {
    speed: f32,
    /// OLA analysis window, frames (20 ms, even).
    window: usize,
    /// Emitted frames per iteration; also the crossfade length (half window).
    hop: usize,
    /// Number of candidate start positions in the search region (30 ms).
    num_candidates: usize,
    /// Offset of the search-region center from its first frame.
    center_offset: i64,
    /// Periodic Hann over `hop`, amplitude-complementary halves.
    hann: Vec<f32>,
    /// Target-to-optimal transition weights over `2 * window`.
    transition: Vec<f32>,
    input: Vec<f32>,
    /// Absolute frame index of `input[0]` since stream start.
    input_start: usize,
    /// Real source frames accepted since the last reset (padding used to flush
    /// EOF is deliberately excluded).
    input_frames: usize,
    /// Frames already returned to the caller.
    emitted_frames: usize,
    /// Output-time position, in output frames.
    output_time: f64,
    /// Integrated nominal source clock. Multiplying all past output by the
    /// newest speed would move the search backwards on a live rate change.
    source_time: f64,
    rate_origin_input: f64,
    rate_origin_output: f64,
    /// Absolute frame index of the next target block (natural continuation).
    target_idx: i64,
    /// Absolute frame index of the search region start.
    search_idx: i64,
    /// Rolling overlap-add staging: completed frames, then the staged second
    /// half of the last block awaiting its blend.
    staged: Vec<f32>,
    staged_complete: usize,
    continuous_start: bool,
    /// Scratch blocks: target continuation, searched optimal, blended result.
    target_buf: Vec<f32>,
    opt_buf: Vec<f32>,
    work_buf: Vec<f32>,
    /// The search region, materialised contiguously once per search so the
    /// correlation inner loop is a straight walk over two slices and the
    /// candidate energies can roll across it. Chromium's `search_block_`.
    search_buf: Vec<f32>,
    /// Energy of every candidate block in `search_buf`, interleaved as
    /// `[candidate][channel]`. Chromium's `energy_candidate_blocks`.
    candidate_energy: Vec<f32>,
    /// Scalar multiply-accumulates performed inside the search. Kept as a
    /// cheap integer so tests can bound the work instead of timing noisy CI
    /// hosts.
    search_macs: usize,
    search_count: usize,
    /// What every search chose, next to what an exhaustive full-resolution
    /// sweep of the same region would have chosen. The coarse pass is a
    /// heuristic, so this is the only way a test can pin how close to the true
    /// optimum it stays.
    #[cfg(test)]
    search_audit: Vec<SearchAudit>,
}

/// One search's outcome measured against an exhaustive reference. Both
/// similarities are full-resolution, so they are directly comparable even when
/// the two passes land on different offsets.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
struct SearchAudit {
    chosen_similarity: f32,
    best_similarity: f32,
}

impl Wsola {
    pub fn new(speed: f32) -> Self {
        assert!(speed.is_finite() && speed > 0.0);
        let window = frames_for_ms(OLA_WINDOW_MS);
        let window = window + (window & 1); // even, so hop is exact
        let hop = window / 2;
        let num_candidates = frames_for_ms(SEARCH_INTERVAL_MS);
        let center_offset = (num_candidates / 2 + (window / 2 - 1)) as i64;

        // Periodic Hann over the full window: its two halves are
        // amplitude-complementary at 50% overlap (COLA) — the first half
        // fades in the incoming block, the second half fades out the staged
        // one.
        let mut hann = vec![0.0f32; window];
        for (n, weight) in hann.iter_mut().enumerate() {
            *weight = 0.5 - 0.5 * (std::f32::consts::TAU * n as f32 / window as f32).cos();
        }
        // Transition weights across `2 * window`: the optimal block rises
        // from 0 while the target block falls from 1.
        let mut transition = vec![0.0f32; window * 2];
        for k in 0..window {
            let rise = 0.5 - 0.5 * (std::f32::consts::PI * k as f32 / window as f32).cos();
            transition[k] = rise;
            transition[window + k] = 1.0 - rise;
        }

        Self {
            speed,
            window,
            hop,
            num_candidates,
            center_offset,
            hann,
            transition,
            input: Vec::new(),
            input_start: 0,
            input_frames: 0,
            emitted_frames: 0,
            output_time: 0.0,
            source_time: 0.0,
            rate_origin_input: 0.0,
            rate_origin_output: 0.0,
            target_idx: 0,
            search_idx: -center_offset,
            staged: Vec::new(),
            staged_complete: 0,
            continuous_start: false,
            target_buf: vec![0.0; window * CHANNELS],
            opt_buf: vec![0.0; window * CHANNELS],
            work_buf: vec![0.0; window * CHANNELS],
            search_buf: vec![0.0; (num_candidates + window - 1) * CHANNELS],
            candidate_energy: vec![0.0; num_candidates * CHANNELS],
            search_macs: 0,
            search_count: 0,
            #[cfg(test)]
            search_audit: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        self.input.clear();
        self.input_start = 0;
        self.input_frames = 0;
        self.emitted_frames = 0;
        self.output_time = 0.0;
        self.source_time = 0.0;
        self.rate_origin_input = 0.0;
        self.rate_origin_output = 0.0;
        self.target_idx = 0;
        self.search_idx = -self.center_offset;
        self.staged.clear();
        self.staged_complete = 0;
        self.continuous_start = false;
        self.search_macs = 0;
        self.search_count = 0;
        #[cfg(test)]
        self.search_audit.clear();
    }


    pub fn set_speed(&mut self, speed: f32) {
        let source = self.rate_origin_input
            + (self.emitted_frames as f64 - self.rate_origin_output) * self.speed as f64;
        self.rate_origin_input = source;
        self.rate_origin_output = self.emitted_frames as f64;
        self.speed = speed;
        self.source_time = source
            + (self.output_time - self.emitted_frames as f64) * speed as f64;
        self.search_idx = self.source_time.round() as i64 - self.center_offset;
    }

    fn wanted_frames(&self) -> usize {
        (self.rate_origin_output
            + (self.input_frames as f64 - self.rate_origin_input) / self.speed as f64)
            .round()
            .max(0.0) as usize
    }

    /// Retire WSOLA at the already-emitted source clock, not at the decoder
    /// head. Keep its real lookahead and blend the staged continuation into it;
    /// after this one bounded handover exact 1x has no stretcher at all.
    fn finish_into_passthrough(&mut self, output: &mut Vec<f32>) {
        let source = self.rate_origin_input
            + (self.emitted_frames as f64 - self.rate_origin_output) * self.speed as f64;
        let start = (source.round().max(0.0) as usize).max(self.input_start);
        let offset = (start - self.input_start) * CHANNELS;
        let remaining = self.input_frames.saturating_sub(start);
        let blend = self.hop.min(remaining).min(self.staged.len() / CHANNELS);
        for frame in 0..remaining {
            for channel in 0..CHANNELS {
                let raw = self.input[offset + frame * CHANNELS + channel];
                let sample = if frame < blend {
                    let weight = self.hann[frame];
                    self.staged[frame * CHANNELS + channel] * (1.0 - weight) + raw * weight
                } else {
                    raw
                };
                output.push(sample);
            }
        }
    }
    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        debug_assert!(input.len().is_multiple_of(CHANNELS));
        self.input_frames += input.len() / CHANNELS;
        self.input.extend_from_slice(input);
        if self.continuous_start && self.input.len() >= self.hop * CHANNELS {
            self.staged.extend_from_slice(&self.input[..self.hop * CHANNELS]);
            self.continuous_start = false;
        }
        while self.can_perform() {
            self.iterate();
        }
        self.emit_complete(output);
    }

    pub fn finish(&mut self, output: &mut Vec<f32>) {
        let wanted = self.wanted_frames();
        while self.emitted_frames + self.staged.len() / CHANNELS < wanted {
            let required_end = (self.target_idx + self.window as i64)
                .max(self.search_idx + self.search_size() as i64)
                .max(self.input_start as i64);
            let required_frames = (required_end - self.input_start as i64) as usize;
            self.input.resize(required_frames * CHANNELS, 0.0);
            if !self.can_perform() {
                break;
            }
            self.iterate();
        }
        let remaining = wanted.saturating_sub(self.emitted_frames) * CHANNELS;
        output.extend_from_slice(&self.staged[..remaining.min(self.staged.len())]);
        self.reset();
    }

    fn emit_complete(&mut self, output: &mut Vec<f32>) {
        // At 3x/4x the first analysis hop can exceed the duration of a short
        // packet. Retain that excess until enough real source arrives; EOF
        // must never be longer merely because a packet happened to end early.
        let frames = self.staged_complete.min(
            self.wanted_frames().saturating_sub(self.emitted_frames),
        );
        let done = frames * CHANNELS;
        output.extend_from_slice(&self.staged[..done]);
        self.staged.drain(..done);
        self.emitted_frames += frames;
        self.staged_complete -= frames;
    }

    /// Chromium's `CanPerformWsola`: both blocks must end inside the buffered
    /// input. Neither index has to be non-negative — a block that reaches
    /// before the start of the stream is zero-prepended by `extract`.
    fn can_perform(&self) -> bool {
        let avail = self.input.len() / CHANNELS;
        let origin = self.input_start as i64;
        self.target_idx - origin + self.window as i64 <= avail as i64
            && self.search_idx - origin + self.search_size() as i64 <= avail as i64
    }

    fn search_size(&self) -> usize {
        self.num_candidates + self.window - 1
    }

    /// One overlap-add step: find the optimal block, blend it against the
    /// target continuation, and stage `hop` completed frames.
    fn iterate(&mut self) {
        let window = self.window;
        let hop = self.hop;

        let optimal_abs = if self.target_within_search_region() {
            // The natural continuation sits inside the search region: no
            // search needed, the target block IS the optimal block.
            extract(
                &self.input,
                self.input_start,
                self.target_idx,
                window,
                &mut self.work_buf,
            );
            self.target_idx
        } else {
            self.extract_target(self.target_idx, window);

            // Exclude the band around the previously chosen block: matching
            // it again repeats audio and reads as buzz. Heuristic constant
            // from Chromium.
            let exclude_center = self.target_idx - hop as i64 - self.search_idx;
            let best = self.find_optimal_candidate((
                exclude_center - EXCLUDE_HALF_FRAMES,
                exclude_center + EXCLUDE_HALF_FRAMES,
            ));
            let optimal_abs = self.search_idx + best as i64;
            extract(
                &self.input,
                self.input_start,
                optimal_abs,
                window,
                &mut self.opt_buf,
            );

            // Transition blend: the target (best continuation of what was
            // just emitted) leads with falling weight while the optimal block
            // rises, smoothing the join between dissimilar blocks.
            for frame in 0..window {
                let w_opt = self.transition[frame];
                let w_target = self.transition[window + frame];
                for channel in 0..CHANNELS {
                    let index = frame * CHANNELS + channel;
                    self.work_buf[index] =
                        self.opt_buf[index] * w_opt + self.target_buf[index] * w_target;
                }
            }
            optimal_abs
        };

        // Overlap-add into staging: blend the staged second half against the
        // block's first half, then stage the block's second half verbatim.
        let need = (self.staged_complete + window) * CHANNELS;
        if self.staged.len() < need {
            self.staged.resize(need, 0.0);
        }
        let base = self.staged_complete * CHANNELS;
        for n in 0..hop {
            let w_out = self.hann[hop + n];
            let w_in = self.hann[n];
            for channel in 0..CHANNELS {
                let index = base + n * CHANNELS + channel;
                self.staged[index] =
                    self.staged[index] * w_out + self.work_buf[n * CHANNELS + channel] * w_in;
            }
        }
        let source = hop * CHANNELS;
        let destination = base + source;
        self.staged[destination..destination + source]
            .copy_from_slice(&self.work_buf[source..window * CHANNELS]);
        self.staged_complete += hop;

        // Next target is one hop ahead of the chosen block; the search region
        // re-centers on the nominal continuation of the output clock.
        self.target_idx = optimal_abs + hop as i64;
        self.output_time += hop as f64;
        self.source_time += hop as f64 * self.speed as f64;
        self.search_idx = self.source_time.round() as i64 - self.center_offset;

        // Drop input no candidate can reach again.
        // Also retain un-emitted real lookahead for a future exact-1x
        // handover, even when a high-rate analysis hop runs beyond it.
        let emitted_source = self.rate_origin_input
            + (self.emitted_frames as f64 - self.rate_origin_output) * self.speed as f64;
        let earliest = self.target_idx.min(self.search_idx)
            .min(emitted_source.floor() as i64);
        if earliest > self.input_start as i64 {
            let drop = ((earliest - self.input_start as i64) as usize) * CHANNELS;
            self.input.drain(..drop);
            self.input_start = earliest as usize;
        }
    }

    fn target_within_search_region(&self) -> bool {
        self.target_idx >= self.search_idx
            && self.target_idx + self.window as i64 <= self.search_idx + self.search_size() as i64
    }

    /// Copies `frames` interleaved frames starting at absolute frame
    /// `abs_start`, zero-filling anything before the start of the stream.
    fn extract_target(&mut self, abs_start: i64, frames: usize) {
        extract(
            &self.input,
            self.input_start,
            abs_start,
            frames,
            &mut self.target_buf,
        );
    }

    /// Chromium's `internal::OptimalIndex`: precompute the energies both
    /// passes normalise by, take a position-decimated pass with quadratic peak
    /// interpolation, then sweep ±`SEARCH_DECIMATION` around the winner at
    /// full resolution.
    fn find_optimal_candidate(&mut self, exclude: (i64, i64)) -> usize {
        self.search_count += 1;

        // Materialise the search region once. Chromium fills `search_block_`
        // the same way, and having it contiguous is what makes both the
        // rolling energies and the correlation inner loop cheap.
        extract(
            &self.input,
            self.input_start,
            self.search_idx,
            self.search_size(),
            &mut self.search_buf,
        );

        let mut macs = 0usize;
        let mut energy_target = [0.0f32; CHANNELS];
        for frame in 0..self.window {
            for (channel, energy) in energy_target.iter_mut().enumerate() {
                let sample = self.target_buf[frame * CHANNELS + channel];
                *energy += sample * sample;
            }
        }
        macs += self.window * CHANNELS;
        moving_block_energies(
            &self.search_buf,
            self.window,
            self.num_candidates,
            &mut self.candidate_energy,
            &mut macs,
        );

        let search = Search {
            target: &self.target_buf,
            region: &self.search_buf,
            energy_target,
            energy_candidates: &self.candidate_energy,
            block_frames: self.window,
            num_candidates: self.num_candidates,
        };
        let coarse = decimated_search(&search, exclude, &mut macs);
        let low = coarse.saturating_sub(SEARCH_DECIMATION);
        let high = (coarse + SEARCH_DECIMATION).min(search.num_candidates - 1);
        // Chromium falls back to candidate 0 when every candidate in the sweep
        // is excluded; the coarse winner is already known to be a good match,
        // so prefer it over an arbitrary index.
        let refined = full_search(&search, low, high, exclude, &mut macs);
        let chosen = refined.map_or(coarse, |(candidate, _)| candidate);

        #[cfg(test)]
        {
            let mut ignored = 0;
            let last = search.num_candidates - 1;
            let exhaustive = full_search(&search, 0, last, exclude, &mut ignored);
            self.search_audit.push(SearchAudit {
                chosen_similarity: refined
                    .map_or_else(|| search.similarity(chosen, &mut ignored), |(_, s)| s),
                best_similarity: exhaustive.map_or(f32::NEG_INFINITY, |(_, s)| s),
            });
        }

        self.search_macs += macs;
        chosen
    }

    #[cfg(test)]
    fn search_macs(&self) -> usize {
        self.search_macs
    }
}

/// Chromium's `internal::MultiChannelMovingBlockEnergies`: the energy of every
/// candidate block in one rolling pass — the first block by direct summation,
/// each later one by sliding a sample out and a sample in.
///
/// This is the piece the original port dropped, and dropping it is what made
/// the search look expensive: recomputing both energies per candidate turns a
/// single dot product into three, so the per-candidate cost tripled. A later
/// revision then bought that back by subsampling the correlation, trading
/// accuracy for speed that this function recovers for free — the whole sweep
/// costs O(region) instead of O(block) per candidate.
///
/// The running sum is carried in `f64` where Chromium carries it in `f32`.
/// Sliding a squared sample in and out ~1300 times accumulates rounding drift
/// that ends up scaling the similarity of later candidates against that of
/// earlier ones; the wider accumulator removes it for one extra add per slide.
fn moving_block_energies(
    region: &[f32],
    block_frames: usize,
    num_blocks: usize,
    energy: &mut [f32],
    macs: &mut usize,
) {
    debug_assert_eq!(energy.len(), num_blocks * CHANNELS);
    for channel in 0..CHANNELS {
        let mut sum = 0.0f64;
        for frame in 0..block_frames {
            let sample = f64::from(region[frame * CHANNELS + channel]);
            sum += sample * sample;
        }
        energy[channel] = sum.max(0.0) as f32;
        for block in 1..num_blocks {
            let leaving = f64::from(region[(block - 1) * CHANNELS + channel]);
            let entering = f64::from(region[(block - 1 + block_frames) * CHANNELS + channel]);
            sum = sum - leaving * leaving + entering * entering;
            energy[block * CHANNELS + channel] = sum.max(0.0) as f32;
        }
    }
    *macs += (block_frames + 2 * num_blocks.saturating_sub(1)) * CHANNELS;
}

/// One WSOLA search: the target block, the materialised search region, and the
/// precomputed energies that normalise every candidate.
struct Search<'a> {
    target: &'a [f32],
    region: &'a [f32],
    energy_target: [f32; CHANNELS],
    energy_candidates: &'a [f32],
    block_frames: usize,
    num_candidates: usize,
}

impl Search<'_> {
    /// Chromium's `MultiChannelDotProduct` followed by
    /// `MultiChannelSimilarityMeasure`: a full-resolution dot product per
    /// channel, normalised by the precomputed energies.
    ///
    /// Every sample of the block is correlated, on the coarse pass as much as
    /// on the refinement pass. Stepping over samples here would decimate the
    /// correlation in the *lag* dimension, which aliases the similarity curve:
    /// for periodic material the aliased peak can sit a full period away from
    /// the true one, and the block that gets overlap-added then does not
    /// actually continue the output. Chromium never does this, and with the
    /// energies precomputed there is nothing to gain from it.
    fn similarity(&self, candidate: usize, macs: &mut usize) -> f32 {
        let samples = self.block_frames * CHANNELS;
        let base = candidate * CHANNELS;
        let target = &self.target[..samples];
        let region = &self.region[base..base + samples];

        // Accumulate `LANES` interleaved samples at a time. `LANES` is a
        // multiple of `CHANNELS`, so every lane only ever sees one channel and
        // folding the lanes down at the end recovers a per-channel dot
        // product. The shape matters: a frame-at-a-time loop over an
        // interleaved buffer does not vectorise, and this correlation is the
        // whole cost of the search. Chromium hand-writes SSE and AVX2 kernels
        // (`MultiChannelDotProduct_SSE`/`_AVX2`) for precisely this loop, and
        // its lane-wise accumulation reassociates the sum exactly the way this
        // does — so this is closer to what Chromium actually computes than a
        // strictly sequential sum would be.
        const LANES: usize = 8;
        const _: () = assert!(LANES % CHANNELS == 0);
        let mut lanes = [0.0f32; LANES];
        let target_chunks = target.chunks_exact(LANES);
        let region_chunks = region.chunks_exact(LANES);
        let target_tail = target_chunks.remainder();
        let region_tail = region_chunks.remainder();
        for (target, region) in target_chunks.zip(region_chunks) {
            for (lane, (target, region)) in lanes.iter_mut().zip(target.iter().zip(region)) {
                *lane += target * region;
            }
        }
        let mut dot = [0.0f32; CHANNELS];
        for (lane, value) in lanes.iter().enumerate() {
            dot[lane % CHANNELS] += value;
        }
        for (index, (target, region)) in target_tail.iter().zip(region_tail).enumerate() {
            dot[index % CHANNELS] += target * region;
        }
        *macs += samples;

        let energies = &self.energy_candidates[base..base + CHANNELS];
        let mut measure = 0.0f32;
        for channel in 0..CHANNELS {
            let product = self.energy_target[channel] * energies[channel];
            measure += dot[channel] / (product + SIMILARITY_EPSILON).sqrt();
        }
        measure
    }
}

fn in_interval(candidate: i64, exclude: (i64, i64)) -> bool {
    candidate >= exclude.0 && candidate <= exclude.1
}

/// Chromium's `internal::QuadraticInterpolation`. Fits `a x^2 + b x + c` to
/// `f(-1), f(0), f(1)` and returns `(extremum, value)`.
fn quadratic_interpolation(y: [f32; 3]) -> (f32, f32) {
    let a = 0.5 * (y[2] + y[0]) - y[1];
    let b = 0.5 * (y[2] - y[0]);
    let c = y[1];
    if a == 0.0 {
        // Colinear within floating-point error: the middle point is the peak.
        (0.0, y[1])
    } else {
        let extremum = -b / (2.0 * a);
        (extremum, a * extremum * extremum + b * extremum + c)
    }
}

/// Chromium's `internal::DecimatedSearch`: sample the similarity curve every
/// `SEARCH_DECIMATION` candidates and return the best interpolated *local
/// maximum*, not merely the best sampled point. Quadratic interpolation across
/// the three-point neighbourhood recovers a sub-decimation estimate of where
/// the peak actually sits, which is what lets the following full-resolution
/// sweep be only ±`SEARCH_DECIMATION` wide.
fn decimated_search(search: &Search<'_>, exclude: (i64, i64), macs: &mut usize) -> usize {
    let last = search.num_candidates - 1;
    let mut similarity = [0.0f32; 3];
    similarity[0] = search.similarity(0, macs);
    let mut best_similarity = similarity[0];
    let mut optimal = 0usize;

    let mut n = SEARCH_DECIMATION;
    if n >= search.num_candidates {
        return 0;
    }
    similarity[1] = search.similarity(n, macs);

    n += SEARCH_DECIMATION;
    if n >= search.num_candidates {
        // Nothing left to sample: pick the better of the two we have.
        return if similarity[1] > similarity[0] {
            SEARCH_DECIMATION
        } else {
            0
        };
    }

    while n < search.num_candidates {
        similarity[2] = search.similarity(n, macs);

        if (similarity[1] > similarity[0] && similarity[1] >= similarity[2])
            || (similarity[1] >= similarity[0] && similarity[1] > similarity[2])
        {
            let (offset, value) = quadratic_interpolation(similarity);
            // The local-maximum test guarantees a <= 0, which bounds `offset`
            // to ±0.5; the clamp is only a guard against a degenerate fit.
            let candidate = ((n - SEARCH_DECIMATION) as i64
                + (offset * SEARCH_DECIMATION as f32 + 0.5) as i64)
                .clamp(0, last as i64);
            if value > best_similarity && !in_interval(candidate, exclude) {
                optimal = candidate as usize;
                best_similarity = value;
            }
        } else if n + SEARCH_DECIMATION >= search.num_candidates
            && similarity[2] > best_similarity
            && !in_interval(n as i64, exclude)
        {
            // End point with no local maximum before it: accept it.
            optimal = n;
            best_similarity = similarity[2];
        }

        similarity[0] = similarity[1];
        similarity[1] = similarity[2];
        n += SEARCH_DECIMATION;
    }
    optimal
}

/// Chromium's `internal::FullSearch` over `[low, high]`, at full correlation
/// resolution. Returns the winning candidate and its similarity, or `None`
/// when the exclusion band swallows the whole range; Chromium silently returns
/// candidate 0 there instead.
///
/// Chromium seeds its running best with `std::numeric_limits<float>::min()`,
/// the smallest positive normal rather than the lowest finite value, so a
/// range whose candidates all correlate *negatively* leaves its answer at
/// candidate 0 regardless of what the sweep measured. Seeding with negative
/// infinity instead makes the least-bad candidate win, which is what the
/// function is documented to return.
fn full_search(
    search: &Search<'_>,
    low: usize,
    high: usize,
    exclude: (i64, i64),
    macs: &mut usize,
) -> Option<(usize, f32)> {
    let mut best: Option<(usize, f32)> = None;
    for candidate in low..=high {
        if in_interval(candidate as i64, exclude) {
            continue;
        }
        let similarity = search.similarity(candidate, macs);
        if best.is_none_or(|(_, best)| similarity > best) {
            best = Some((candidate, similarity));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer_engine::protocol::LoopRange;

    fn stereo_tone(frames: usize, frequency: f32, right_scale: f32) -> Vec<f32> {
        let mut samples = Vec::with_capacity(frames * CHANNELS);
        for frame in 0..frames {
            let value =
                (std::f32::consts::TAU * frequency * frame as f32 / SAMPLE_RATE as f32).sin();
            samples.push(value);
            samples.push(value * right_scale);
        }
        samples
    }

    fn stretch_in_packets(input: &[f32], speed: f32, packet_frames: &[usize]) -> Vec<f32> {
        let mut stretcher = Wsola::new(speed);
        let mut output = Vec::new();
        let mut frame = 0;
        let total = input.len() / CHANNELS;
        let mut packet = 0;
        while frame < total {
            let count = packet_frames[packet % packet_frames.len()].min(total - frame);
            stretcher.process(
                &input[frame * CHANNELS..(frame + count) * CHANNELS],
                &mut output,
            );
            frame += count;
            packet += 1;
        }
        stretcher.finish(&mut output);
        output
    }

    fn pipeline_in_packets(
        edit: TrackEdit,
        speed: f32,
        position_ms: u32,
        input: &[f32],
        packet_frames: &[usize],
    ) -> (Vec<f32>, Option<u32>) {
        let mut pipeline = AudioPipeline::new(&PipelineConfig {
            edit: Some(edit),
            speed,
            position_ms,
            loop_pass: 1,
        });
        let mut output = Vec::new();
        let mut packet_output = Vec::new();
        let mut frame = 0;
        let total = input.len() / CHANNELS;
        let mut packet = 0;
        let mut loop_to = None;
        while frame < total && loop_to.is_none() {
            let count = packet_frames[packet % packet_frames.len()].min(total - frame);
            loop_to = pipeline.process(
                ms_to_frame(position_ms) + frame as u64,
                &input[frame * CHANNELS..(frame + count) * CHANNELS],
                &mut packet_output,
            );
            output.extend_from_slice(&packet_output);
            frame += count;
            packet += 1;
        }
        if loop_to.is_none() {
            pipeline.finish(&mut packet_output);
            output.extend_from_slice(&packet_output);
        }
        (output, loop_to)
    }

    fn source_ramp(start_frame: usize, frames: usize) -> Vec<f32> {
        (start_frame..start_frame + frames)
            .flat_map(|frame| [frame as f32, -(frame as f32)])
            .collect()
    }

    fn positive_crossing_frequency(samples: &[f32]) -> f32 {
        let mut crossings = 0usize;
        let mut first = 0.0;
        let mut last = 0.0;
        let frames = samples.len() / CHANNELS;
        for frame in 1..frames {
            let before = f64::from(samples[(frame - 1) * CHANNELS]);
            let after = f64::from(samples[frame * CHANNELS]);
            if before <= 0.0 && after > 0.0 {
                let crossing = (frame - 1) as f64 - before / (after - before);
                if crossings == 0 {
                    first = crossing;
                }
                last = crossing;
                crossings += 1;
            }
        }
        if crossings < 2 {
            return 0.0;
        }
        // Measure complete periods rather than counting a phase-dependent
        // partial period at each edge. A one-second 4x output otherwise has
        // 1 Hz count quantization even when its actual pitch is unchanged.
        ((crossings - 1) as f64 * f64::from(SAMPLE_RATE) / (last - first)) as f32
    }

    #[test]
    fn duration_changes_by_the_inverse_speed_ratio() {
        let input = stereo_tone(SAMPLE_RATE as usize * 8, 440.0, 1.0);
        for speed in [0.75, 1.25, 1.75] {
            let output = stretch_in_packets(&input, speed, &[128, 576, 1_024, 287]);
            let expected = input.len() as f64 / f64::from(speed);
            let error = (output.len() as f64 - expected).abs() / expected;
            assert!(
                error < 0.02,
                "speed {speed} duration error was {:.2}%",
                error * 100.0
            );
        }
    }

    #[test]
    fn a_tone_keeps_its_pitch_when_speed_changes() {
        let input = stereo_tone(SAMPLE_RATE as usize * 8, 440.0, 1.0);
        for speed in [0.7, 1.4, 2.0] {
            let output = stretch_in_packets(&input, speed, &[256, 1_152, 2_048]);
            let measured = positive_crossing_frequency(&output);
            assert!(
                (measured - 440.0).abs() < 0.5,
                "speed {speed} measured {measured} Hz"
            );
        }
    }

    #[test]
    fn stereo_channels_remain_sample_aligned() {
        let input = stereo_tone(SAMPLE_RATE as usize * 4, 997.0, -0.5);
        let output = stretch_in_packets(&input, 1.37, &[128, 512, 997]);
        for frame in output.chunks_exact(CHANNELS) {
            assert!((frame[1] + frame[0] * 0.5).abs() < 1.0e-6);
        }
    }

    #[test]
    fn packet_boundaries_do_not_change_the_stream() {
        let input = stereo_tone(SAMPLE_RATE as usize * 5, 733.0, 0.25);
        let whole = stretch_in_packets(&input, 0.83, &[input.len() / CHANNELS]);
        let packeted = stretch_in_packets(&input, 0.83, &[128, 576, 1_024, 333]);
        assert_eq!(whole, packeted);
    }

    #[test]
    fn reset_discards_every_sample_from_the_old_position() {
        let old = stereo_tone(SAMPLE_RATE as usize, 220.0, 1.0);
        let fresh = stereo_tone(SAMPLE_RATE as usize * 2, 880.0, 1.0);
        let mut stretcher = Wsola::new(1.5);
        let mut discarded = Vec::new();
        stretcher.process(&old, &mut discarded);
        stretcher.reset();
        let mut output = Vec::new();
        stretcher.process(&fresh, &mut output);
        stretcher.finish(&mut output);
        // Measured +0.45 Hz after a mid-stream reset; ±2 Hz is ~4x headroom.
        assert!((positive_crossing_frequency(&output) - 880.0).abs() < 2.0);
    }

    #[test]
    fn cuts_are_exact_and_packet_boundary_independent_at_one_x() {
        let edit = TrackEdit {
            cuts: vec![
                TimeRange {
                    start_ms: 10,
                    end_ms: 30,
                },
                TimeRange {
                    start_ms: 50,
                    end_ms: 70,
                },
                TimeRange {
                    start_ms: 90,
                    end_ms: 100,
                },
            ],
            loop_range: None,
        };
        let frames = ms_to_frame(100) as usize;
        let input = source_ramp(0, frames);
        let expected: Vec<_> = input
            .chunks_exact(CHANNELS)
            .enumerate()
            .filter(|(frame, _)| {
                !edit.cuts.iter().any(|cut| {
                    *frame >= ms_to_frame(cut.start_ms) as usize
                        && *frame < ms_to_frame(cut.end_ms) as usize
                })
            })
            .flat_map(|(_, frame)| frame.iter().copied())
            .collect();

        let (whole, _) = pipeline_in_packets(edit.clone(), 1.0, 0, &input, &[frames]);
        let (packet_aligned, _) =
            pipeline_in_packets(edit.clone(), 1.0, 0, &input, &[ms_to_frame(10) as usize]);
        let (irregular, _) = pipeline_in_packets(edit, 1.0, 0, &input, &[127, 441, 997]);
        assert_eq!(whole, expected);
        assert_eq!(packet_aligned, expected);
        assert_eq!(irregular, expected);
    }

    #[test]
    fn edited_duration_is_source_minus_cuts_at_every_supported_speed_extreme() {
        let edit = TrackEdit {
            cuts: vec![
                TimeRange {
                    start_ms: 20,
                    end_ms: 80,
                },
                TimeRange {
                    start_ms: 120,
                    end_ms: 150,
                },
                TimeRange {
                    start_ms: 180,
                    end_ms: 200,
                },
            ],
            loop_range: None,
        };
        let source_frames = ms_to_frame(200) as usize;
        let removed_frames: usize = edit
            .cuts
            .iter()
            .map(|cut| (ms_to_frame(cut.end_ms) - ms_to_frame(cut.start_ms)) as usize)
            .sum();
        let audible_frames = source_frames - removed_frames;
        let input = stereo_tone(source_frames, 440.0, -0.25);

        for speed in [0.5, 1.0, 2.0] {
            let (output, loop_to) =
                pipeline_in_packets(edit.clone(), speed, 0, &input, &[882, 2_205, 311]);
            assert_eq!(loop_to, None);
            assert_eq!(
                output.len() / CHANNELS,
                (audible_frames as f64 / f64::from(speed)).round() as usize,
                "speed {speed} must apply only after cuts and retain the natural EOF tail"
            );
        }
    }

    #[test]
    fn source_time_seek_inside_a_cut_resumes_at_the_cut_end() {
        let edit = TrackEdit {
            cuts: vec![
                TimeRange {
                    start_ms: 40,
                    end_ms: 70,
                },
                TimeRange {
                    start_ms: 90,
                    end_ms: 110,
                },
            ],
            loop_range: None,
        };
        let position_ms = 50;
        let start = ms_to_frame(position_ms) as usize;
        let end = ms_to_frame(130) as usize;
        let input = source_ramp(start, end - start);
        let (output, _) = pipeline_in_packets(edit, 1.0, position_ms, &input, &[127, 1_024, 333]);
        let audible_source_frames: Vec<_> = output
            .chunks_exact(CHANNELS)
            .map(|frame| frame[0] as usize)
            .collect();

        assert_eq!(
            audible_source_frames.first().copied(),
            Some(ms_to_frame(70) as usize)
        );
        assert!(
            audible_source_frames
                .iter()
                .all(|frame| *frame < ms_to_frame(90) as usize
                    || *frame >= ms_to_frame(110) as usize)
        );
        assert_eq!(audible_source_frames.last().copied(), Some(end - 1));
    }

    #[test]
    fn loop_boundary_uses_original_time_after_cuts_and_speed() {
        let edit = TrackEdit {
            cuts: vec![TimeRange {
                start_ms: 5,
                end_ms: 10,
            }],
            loop_range: Some(LoopRange {
                start_ms: 20,
                end_ms: 55,
                play_count: 2,
            }),
        };
        let start = ms_to_frame(20) as usize;
        let input = stereo_tone(ms_to_frame(80) as usize - start, 523.25, 1.0);
        let audible_frames = (ms_to_frame(55) - ms_to_frame(20)) as usize;

        for speed in [0.5, 1.0, 2.0] {
            let (output, loop_to) =
                pipeline_in_packets(edit.clone(), speed, 20, &input, &[127, 882, 2_048]);
            assert_eq!(loop_to, Some(20));
            assert_eq!(
                output.len() / CHANNELS,
                (audible_frames as f64 / f64::from(speed)).round() as usize,
                "speed {speed} must flush exactly through the original-time loop end"
            );
        }
    }

    #[test]
    fn reset_buffers_rearms_a_loop_marker_for_the_resumed_pass() {
        let edit = TrackEdit {
            cuts: Vec::new(),
            loop_range: Some(LoopRange {
                start_ms: 20,
                end_ms: 40,
                play_count: 2,
            }),
        };
        let mut pipeline = AudioPipeline::new(&PipelineConfig {
            edit: Some(edit),
            speed: 1.0,
            position_ms: 0,
            loop_pass: 1,
        });
        let mut output = Vec::new();
        let first = source_ramp(0, ms_to_frame(80) as usize);
        assert_eq!(pipeline.process(0, &first, &mut output), Some(20));

        pipeline.reset_buffers();
        output.clear();
        let resumed = source_ramp(
            ms_to_frame(40) as usize,
            (ms_to_frame(80) - ms_to_frame(40)) as usize,
        );
        assert_eq!(pipeline.process(ms_to_frame(40), &resumed, &mut output), Some(20));
    }

    #[test]
    fn finite_loop_emits_exactly_the_requested_number_of_passes() {
        for play_count in [2, 3] {
            let edit = TrackEdit {
                cuts: Vec::new(),
                loop_range: Some(LoopRange {
                    start_ms: 20,
                    end_ms: 40,
                    play_count,
                }),
            };
            let total_frames = ms_to_frame(80) as usize;
            let loop_start = ms_to_frame(20) as usize;
            let input = source_ramp(0, total_frames);

            let mut first = SampleEdit::new(&edit, 0, 1);
            let mut output = Vec::new();
            assert_eq!(first.process(0, &input, &mut output), Some(20));

            for pass in 2..=play_count {
                let mut repeated = SampleEdit::new(&edit, 20, pass);
                let repeated_input = source_ramp(loop_start, total_frames - loop_start);
                output.clear();
                let marker = repeated.process(loop_start as u64, &repeated_input, &mut output);
                if pass < play_count {
                    assert_eq!(marker, Some(20));
                } else {
                    assert_eq!(marker, None);
                    assert_eq!(output.len(), (total_frames - loop_start) * CHANNELS);
                }
            }
        }
    }

    #[test]
    fn starting_at_or_after_loop_end_never_emits_a_loop_marker() {
        let edit = TrackEdit {
            cuts: Vec::new(),
            loop_range: Some(LoopRange {
                start_ms: 20,
                end_ms: 40,
                play_count: 3,
            }),
        };
        let total_frames = ms_to_frame(80) as usize;

        for position_ms in [40, 41, 80] {
            let start_frame = ms_to_frame(position_ms) as usize;
            let input = source_ramp(start_frame, total_frames - start_frame);
            let (output, loop_to) =
                pipeline_in_packets(edit.clone(), 1.0, position_ms, &input, &[127, 882, 2_048]);

            assert_eq!(loop_to, None, "position {position_ms} must be a final pass");
            assert_eq!(
                output.len() / CHANNELS,
                total_frames - start_frame,
                "position {position_ms} must retain the natural EOF tail"
            );
        }
    }

    /// Runs the 2x path over `input` and returns the stretcher so the caller
    /// can inspect what its searches did.
    fn searched(input: &[f32], speed: f32) -> Wsola {
        let mut stretcher = Wsola::new(speed);
        let mut output = Vec::new();
        stretcher.process(input, &mut output);
        assert!(stretcher.search_count > 0, "speed {speed} must search");
        stretcher
    }

    /// A harmonic stack: a fundamental plus two partials, the right channel
    /// scaled and the phase offset per partial. Unlike a single sine this has
    /// a similarity curve with one clearly dominant peak per period rather
    /// than a hundred numerically indistinguishable ones, so "did the search
    /// find the right block" is a well-posed question.
    fn stereo_harmonics(frames: usize, fundamental: f32, right_scale: f32) -> Vec<f32> {
        let mut samples = Vec::with_capacity(frames * CHANNELS);
        for frame in 0..frames {
            let phase = std::f32::consts::TAU * fundamental * frame as f32 / SAMPLE_RATE as f32;
            let value = phase.sin() + 0.6 * (2.0 * phase + 0.7).sin() + 0.35 * (3.0 * phase).sin();
            samples.push(value);
            samples.push(value * right_scale);
        }
        samples
    }

    /// The quality property, and the one correlation subsampling breaks.
    ///
    /// Position decimation samples the *true* similarity curve sparsely, so
    /// the refinement pass can walk back to the peak; the worst it costs is a
    /// slightly different offset of equal merit. Subsampling the correlation
    /// instead corrupts the measured value at every position, so the coarse
    /// pass hunts in the wrong neighbourhood entirely and refinement cannot
    /// recover. Comparing offsets alone cannot tell those apart — periodic
    /// material has many equally good offsets — so compare what actually
    /// matters: the full-resolution similarity of the block we picked against
    /// the best any block in the region could have scored.
    ///
    /// The measure sums a normalised per-channel correlation over `CHANNELS`,
    /// so 2.0 is a perfect stereo match and the tolerance below is 0.5% of
    /// full scale. Measured worst case across these cases is 0.0036.
    #[test]
    fn the_search_picks_a_block_as_good_as_an_exhaustive_search_would() {
        for (fundamental, speed) in [(220.0, 2.0), (147.0, 0.6), (330.0, 1.37)] {
            let input = stereo_harmonics(SAMPLE_RATE as usize * 2, fundamental, 0.7);
            let stretcher = searched(&input, speed);
            let worst = stretcher
                .search_audit
                .iter()
                .max_by(|a, b| {
                    let gap = |audit: &SearchAudit| audit.best_similarity - audit.chosen_similarity;
                    gap(a).total_cmp(&gap(b))
                })
                .copied()
                .expect("a search was recorded");
            let gap = worst.best_similarity - worst.chosen_similarity;
            assert!(
                gap < 0.01,
                "{fundamental} Hz at {speed}x: worst search gave up {gap} of \
                 similarity versus an exhaustive sweep ({worst:?})"
            );
        }
    }

    /// Matching the exhaustive result is only interesting if we are not simply
    /// doing the exhaustive search. Chromium's structure costs one dot product
    /// per sampled position against precomputed energies, so the whole search
    /// must stay well under the cost of correlating every candidate.
    #[test]
    fn the_search_costs_a_fraction_of_an_exhaustive_sweep() {
        let input = stereo_tone(SAMPLE_RATE as usize * 2, 440.0, 0.7);
        let stretcher = searched(&input, 2.0);

        let exhaustive = stretcher.num_candidates * stretcher.window * CHANNELS;
        let average = stretcher.search_macs() / stretcher.search_count;
        // Positions are decimated by 5 and refined over 11, so the dot
        // products alone are ~(1/5 + 11/1323) of exhaustive; the rolling
        // energies add ~1.5% on top. Assert half as a stable ceiling.
        assert!(
            average * 2 < exhaustive,
            "search averaged {average} multiply-accumulates versus \
             {exhaustive} for an exhaustive sweep"
        );
    }


    #[test]
    fn finish_preserves_the_exact_delayed_eof_duration() {
        let frames = frames_for_ms(37);
        let input = stereo_tone(frames, 611.0, -0.25);
        for speed in [0.5, 0.75, 1.25, 2.0] {
            let output = stretch_in_packets(&input, speed, &[127, 293]);
            assert_eq!(
                output.len() / CHANNELS,
                (frames as f64 / speed as f64).round() as usize,
                "speed {speed} dropped or invented the WSOLA EOF tail"
            );
        }
    }

    #[test]
    fn high_rates_keep_pitch_duration_and_packet_independence() {
        let input = stereo_tone(SAMPLE_RATE as usize * 4, 440.0, -0.25);
        for speed in [3.0, 4.0] {
            let whole = stretch_in_packets(&input, speed, &[input.len() / CHANNELS]);
            let packets = stretch_in_packets(&input, speed, &[1, 128, 576, 1_024, 333]);
            assert_eq!(whole, packets, "speed {speed} changed at packet boundaries");
            assert_eq!(packets.len() / CHANNELS,
                (input.len() as f64 / CHANNELS as f64 / speed as f64).round() as usize);
            let measured = positive_crossing_frequency(&packets);
            assert!((measured - 440.0).abs() < 1.0,
                "speed {speed} changed the tone's pitch: measured {measured} Hz");
            for frame in packets.chunks_exact(CHANNELS) {
                assert!((frame[1] + frame[0] * 0.25).abs() < 1.0e-6);
            }
        }
    }

    #[test]
    fn high_rate_short_eof_never_emits_a_full_hop_instead_of_its_duration() {
        for frames in [1, 8, 300, 1_024, 1_632, 2_205, 4_410] {
            let input = stereo_tone(frames, 611.0, 1.0);
            for speed in [3.0, 4.0] {
                let output = stretch_in_packets(&input, speed, &[127, 293]);
                assert_eq!(output.len() / CHANNELS,
                    (frames as f64 / speed as f64).round() as usize,
                    "{frames} source frames at {speed}x");
            }
        }
    }

    #[test]
    fn changing_rate_integrates_old_then_new_source_time_without_restarting() {
        let frames = SAMPLE_RATE as usize * 4;
        let input = stereo_tone(frames, 440.0, 0.5);
        let mut stretcher = Wsola::new(1.5);
        let mut output = Vec::new();
        stretcher.process(&input[..SAMPLE_RATE as usize * 2 * CHANNELS], &mut output);
        let old_frames = output.len() / CHANNELS;
        stretcher.set_speed(4.0);
        stretcher.process(&input[SAMPLE_RATE as usize * 2 * CHANNELS..], &mut output);
        stretcher.finish(&mut output);
        let expected = (old_frames as f64
            + (frames as f64 - old_frames as f64 * 1.5) / 4.0).round() as usize;
        assert_eq!(output.len() / CHANNELS, expected);
        assert!((positive_crossing_frequency(&output) - 440.0).abs() < 1.0);
    }

    #[test]
    fn live_rate_changes_keep_edit_cursor_and_finite_loop_pass() {
        let mut pipeline = AudioPipeline::new(&PipelineConfig {
            edit: Some(TrackEdit {
                cuts: vec![TimeRange { start_ms: 30, end_ms: 40 }],
                loop_range: Some(LoopRange { start_ms: 10, end_ms: 100, play_count: 3 }),
                ..TrackEdit::default()
            }),
            speed: 1.0,
            position_ms: 0,
            loop_pass: 2,
        });
        let mut output = Vec::new();
        let before = frames_for_ms(20);
        assert_eq!(pipeline.process(0, &source_ramp(0, before), &mut output), None);
        pipeline.set_speed(4.0, &mut output);
        let after = frames_for_ms(90);
        assert_eq!(pipeline.process(before as u64, &source_ramp(before, after), &mut output), Some(10));
        let edit = pipeline.edit.as_ref().unwrap();
        assert_eq!(edit.frame, ms_to_frame(100));
        assert_eq!(edit.loop_range.unwrap().pass, 2);
        assert_eq!(edit.cut_index, 1);
    }

    #[test]
    fn rapid_rate_reversals_preserve_stereo_duration_and_packet_independence() {
        let input = stereo_tone(SAMPLE_RATE as usize, 440.0, -0.25);
        let render = |packet_frames: usize| {
            let mut stretcher = Wsola::new(4.0);
            let mut output = Vec::new();
            let mut source_clock = 0.0;
            let mut previous_frames = 0;
            let mut previous_speed = 4.0;
            let mut final_speed = 4.0;
            for (index, segment) in input.chunks(1_632 * CHANNELS).enumerate() {
                let emitted = output.len() / CHANNELS;
                source_clock += (emitted - previous_frames) as f64 * previous_speed;
                previous_frames = emitted;
                final_speed = [4.0, 0.5, 3.0, 0.5, 1.37][index % 5];
                previous_speed = f64::from(final_speed);
                stretcher.set_speed(final_speed);
                for packet in segment.chunks(packet_frames * CHANNELS) {
                    stretcher.process(packet, &mut output);
                }
            }
            stretcher.finish(&mut output);
            let expected = (previous_frames as f64
                + (input.len() as f64 / CHANNELS as f64 - source_clock)
                    / f64::from(final_speed)).round() as usize;
            assert_eq!(output.len() / CHANNELS, expected);
            for frame in output.chunks_exact(CHANNELS) {
                assert!(frame.iter().all(|sample| sample.is_finite()));
                assert!((frame[1] + frame[0] * 0.25).abs() < 1.0e-6);
            }
            output
        };
        let whole_segments = render(1_632);
        for packet_frames in [1, 127, 1_024] {
            assert_eq!(render(packet_frames), whole_segments);
        }
    }

    #[test]
    fn returning_to_exact_one_preserves_real_lookahead_then_passes_samples_unchanged() {
        let mut pipeline = AudioPipeline::new(&PipelineConfig {
            edit: None, speed: 4.0, position_ms: 0, loop_pass: 1,
        });
        let input = stereo_tone(frames_for_ms(200), 440.0, -0.5);
        let mut output = Vec::new();
        pipeline.process(0, &input, &mut output);
        let consumed = output.len() / CHANNELS * 4;
        let mut pending = Vec::new();
        pipeline.set_speed(1.0, &mut pending);
        assert_eq!(pending.len() / CHANNELS, input.len() / CHANNELS - consumed);
        assert!(pipeline.is_passthrough());
        let next = source_ramp(input.len() / CHANNELS, 127);
        pipeline.process((input.len() / CHANNELS) as u64, &next, &mut output);
        assert_eq!(output, next);
    }

    #[test]
    fn synthetic_vorbis_seeks_deliver_exact_pcm_for_cuts_and_repeated_loops() {
        assert_encoded_seeks(include_bytes!("../../vendor/librespot-playback/src/decoder/fixtures/seek-tone.ogg"));
    }

    #[test]
    fn synthetic_mp3_seeks_deliver_exact_pcm_for_cuts_and_repeated_loops() {
        assert_encoded_seeks(include_bytes!("../../vendor/librespot-playback/src/decoder/fixtures/seek-tone.mp3"));
    }

    fn assert_encoded_seeks(fixture: &[u8]) {
        use librespot_playback::decoder::{AudioDecoder, SymphoniaDecoder};
        use symphonia::core::probe::Hint;
        let make_decoder = || SymphoniaDecoder::new(
            std::io::Cursor::new(fixture.to_vec()), Hint::new(),
        ).unwrap();
        let mut reference_decoder = make_decoder();
        reference_decoder.seek(0).unwrap();
        let mut reference = Vec::new();
        while let Some((position, packet)) = reference_decoder.next_packet().unwrap() {
            assert_eq!(position.source_frame, (reference.len() / CHANNELS) as u64);
            reference.extend(packet.samples().unwrap().iter().map(|sample| *sample as f32));
        }
        assert_eq!(reference.len() / CHANNELS, SAMPLE_RATE as usize * 2);

        let mut decoder = make_decoder();
        for target_ms in [0, 1, 10, 37, 100, 123, 500, 1_000, 1_011, 37, 1] {
            assert_eq!(decoder.seek(target_ms).unwrap(), target_ms);
            let target = ms_to_frame(target_ms);
            let (position, packet) = decoder.next_packet().unwrap().unwrap();
            assert_eq!(position.source_frame, target, "target={target_ms}");
            let samples: Vec<f32> = packet.samples().unwrap().iter().map(|sample| *sample as f32).collect();
            assert!(!samples.is_empty(), "empty decoder priming packets are not PCM");
            let offset = target as usize * CHANNELS;
            assert_eq!(samples, reference[offset..offset + samples.len()],
                "seek preroll must reconstruct the same PCM as continuous decode");
            let mut pipeline = AudioPipeline::new(&PipelineConfig {
                edit: Some(TrackEdit {
                    cuts: vec![TimeRange { start_ms: target_ms, end_ms: target_ms + 1 }],
                    loop_range: None,
                }),
                speed: 1.0, position_ms: target_ms, loop_pass: 1,
            });
            let mut output = Vec::new();
            pipeline.process(position.source_frame, &samples, &mut output);
            let removed = ((ms_to_frame(target_ms + 1) - target) as usize).min(samples.len() / CHANNELS);
            assert_eq!(output, samples[removed * CHANNELS..]);
        }

        for pass in 1..3 {
            decoder.seek(37).unwrap();
            let mut pipeline = AudioPipeline::new(&PipelineConfig {
                edit: Some(TrackEdit {
                    cuts: Vec::new(),
                    loop_range: Some(LoopRange { start_ms: 37, end_ms: 100, play_count: 3 }),
                }),
                speed: 1.0, position_ms: 37, loop_pass: pass,
            });
            let mut rendered = Vec::new();
            let mut output = Vec::new();
            loop {
                let (position, packet) = decoder.next_packet().unwrap().unwrap();
                let samples: Vec<f32> = packet.samples().unwrap().iter().map(|sample| *sample as f32).collect();
                let marker = pipeline.process(position.source_frame, &samples, &mut output);
                rendered.extend_from_slice(&output);
                if let Some(marker) = marker {
                    assert_eq!(marker, 37);
                    break;
                }
            }
            assert_eq!(rendered,
                reference[ms_to_frame(37) as usize * CHANNELS..ms_to_frame(100) as usize * CHANNELS]);
        }
    }

    #[test]
    fn packet_source_frames_rebase_cuts_instead_of_assuming_requested_milliseconds() {
        let mut pipeline = AudioPipeline::new(&PipelineConfig {
            edit: Some(TrackEdit {
                cuts: vec![TimeRange { start_ms: 100, end_ms: 120 }],
                loop_range: None,
            }),
            speed: 1.0, position_ms: 100, loop_pass: 1,
        });
        let actual_start = ms_to_frame(110) + 7;
        let input = source_ramp(actual_start as usize, 1_323);
        let mut output = Vec::new();
        pipeline.process(actual_start, &input, &mut output);
        let cut_frames = (ms_to_frame(120) - actual_start) as usize;
        assert_eq!(output, input[cut_frames * CHANNELS..]);
        pipeline.reset_buffers();
        let next = source_ramp(ms_to_frame(130) as usize, 127);
        pipeline.process(ms_to_frame(130), &next, &mut output);
        assert_eq!(output, next);
    }
}
