use std::{io, time::Duration};

use symphonia::core::{
    audio::SampleBuffer,
    codecs::{Decoder, DecoderOptions, CODEC_TYPE_MP3, CODEC_TYPE_VORBIS},
    errors::Error,
    formats::{FormatOptions, SeekMode, SeekTo},
    io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions},
    meta::{MetadataOptions, StandardTagKey, Value},
    probe::{Hint, ProbeResult},
};

use super::{AudioDecoder, AudioPacket, AudioPacketPosition, DecoderError, DecoderResult};

use crate::{NUM_CHANNELS, SAMPLE_RATE, player::NormalisationData, symphonia_util};

pub struct SymphoniaDecoder {
    probe_result: ProbeResult,
    decoder: Box<dyn Decoder>,
    sample_buffer: Option<SampleBuffer<f64>>,
    seek_frame: Option<u64>,
    pending: Option<(AudioPacketPosition, AudioPacket)>,
}

#[derive(Default)]
pub(crate) struct LocalFileMetadata {
    pub name: Option<String>,
    pub language: Option<String>,
    pub album: Option<String>,
    pub artists: Option<String>,
    pub album_artists: Option<String>,
    pub number: Option<u32>,
    pub disc_number: Option<u32>,
}

impl SymphoniaDecoder {
    pub fn new<R>(input: R, hint: Hint) -> DecoderResult<Self>
    where
        R: MediaSource + 'static,
    {
        let mss_opts = MediaSourceStreamOptions {
            buffer_len: librespot_audio::AudioFetchParams::get().minimum_download_size,
        };
        let mss = MediaSourceStream::new(Box::new(input), mss_opts);

        let format_opts = FormatOptions {
            enable_gapless: true,
            ..Default::default()
        };
        let metadata_opts: MetadataOptions = Default::default();

        let probe_result =
            symphonia::default::get_probe().format(&hint, mss, &format_opts, &metadata_opts)?;

        let format = &probe_result.format;

        let track = format.default_track().ok_or_else(|| {
            DecoderError::SymphoniaDecoder("Could not retrieve default track".into())
        })?;

        let decoder_opts: DecoderOptions = Default::default();

        let decoder = symphonia::default::get_codecs().make(&track.codec_params, &decoder_opts)?;

        let rate = decoder.codec_params().sample_rate.ok_or_else(|| {
            DecoderError::SymphoniaDecoder("Could not retrieve sample rate".into())
        })?;

        // TODO: The official client supports local files with sample rates other than 44,100 kHz.
        // To play these accurately, we need to either resample the input audio, or introduce a way
        // to change the player's current sample rate (likely by closing and re-opening the sink
        // with new parameters).
        if rate != SAMPLE_RATE {
            return Err(DecoderError::SymphoniaDecoder(format!(
                "Unsupported sample rate: {rate}"
            )));
        }

        let channels = decoder.codec_params().channels.ok_or_else(|| {
            DecoderError::SymphoniaDecoder("Could not retrieve channel configuration".into())
        })?;
        if channels.count() != NUM_CHANNELS as usize {
            return Err(DecoderError::SymphoniaDecoder(format!(
                "Unsupported number of channels: {channels}"
            )));
        }

        Ok(Self {
            probe_result,
            decoder,
            // We set the sample buffer when decoding the first full packet,
            // whose duration is also the ideal sample buffer size.
            sample_buffer: None,
            seek_frame: None,
            pending: None,
        })
    }

    pub fn normalisation_data(&mut self) -> Option<NormalisationData> {
        let metadata = symphonia_util::get_latest_metadata(&mut self.probe_result)?;
        let tags = metadata.current()?.tags();

        if tags.is_empty() {
            None
        } else {
            let mut data = NormalisationData::default();

            for tag in tags {
                if let Value::Float(value) = tag.value {
                    match tag.std_key {
                        Some(StandardTagKey::ReplayGainAlbumGain) => data.album_gain_db = value,
                        Some(StandardTagKey::ReplayGainAlbumPeak) => data.album_peak = value,
                        Some(StandardTagKey::ReplayGainTrackGain) => data.track_gain_db = value,
                        Some(StandardTagKey::ReplayGainTrackPeak) => data.track_peak = value,
                        _ => (),
                    }
                }
            }

            Some(data)
        }
    }

    pub(crate) fn local_file_metadata(&mut self) -> Option<LocalFileMetadata> {
        let metadata = symphonia_util::get_latest_metadata(&mut self.probe_result)?;
        let tags = metadata.current()?.tags();
        let mut metadata = LocalFileMetadata::default();

        for tag in tags {
            if let Value::String(value) = &tag.value {
                match tag.std_key {
                    // We could possibly use mem::take here to avoid cloning, but that risks leaving
                    // the audio item metadata in a bad state.
                    Some(StandardTagKey::TrackTitle) => metadata.name = Some(value.clone()),
                    Some(StandardTagKey::Language) => metadata.language = Some(value.clone()),
                    Some(StandardTagKey::Artist) => metadata.artists = Some(value.clone()),
                    Some(StandardTagKey::AlbumArtist) => {
                        metadata.album_artists = Some(value.clone())
                    }
                    Some(StandardTagKey::Album) => metadata.album = Some(value.clone()),
                    Some(StandardTagKey::TrackNumber) => {
                        metadata.number = match value.parse::<u32>() {
                            Ok(value) => Some(value),
                            Err(e) => {
                                warn!(
                                    "Failed to parse local file's track number tag '{value}': {e}"
                                );
                                None
                            }
                        }
                    }
                    Some(StandardTagKey::DiscNumber) => {
                        metadata.disc_number = match value.parse::<u32>() {
                            Ok(value) => Some(value),
                            Err(e) => {
                                warn!(
                                    "Failed to parse local file's disc number tag '{value}': {e}"
                                );
                                None
                            }
                        }
                    }
                    _ => (),
                }
            } else if let Value::UnsignedInt(value) = &tag.value {
                match tag.std_key {
                    Some(StandardTagKey::TrackNumber) => metadata.number = Some(*value as u32),
                    Some(StandardTagKey::DiscNumber) => metadata.disc_number = Some(*value as u32),
                    _ => (),
                }
            } else if let Value::SignedInt(value) = &tag.value {
                match tag.std_key {
                    Some(StandardTagKey::TrackNumber) => metadata.number = Some(*value as u32),
                    Some(StandardTagKey::DiscNumber) => metadata.disc_number = Some(*value as u32),
                    _ => (),
                }
            }
        }

        Some(metadata)
    }

    fn ts_to_frame(&self, ts: u64) -> u64 {
        self.decoder.codec_params().time_base.map_or(ts, |base| {
            (u128::from(ts) * u128::from(base.numer) * u128::from(SAMPLE_RATE)
                / u128::from(base.denom)) as u64
        })
    }
}

impl AudioDecoder for SymphoniaDecoder {
    fn seek(&mut self, position_ms: u32) -> Result<u32, DecoderError> {
        let requested = (u64::from(position_ms) * u64::from(SAMPLE_RATE) + 500) / 1_000;
        let params = self.decoder.codec_params();
        let target = params.n_frames.map_or(requested, |frames| requested.min(self.ts_to_frame(frames)));
        // Rebuild decoder history before trimming to the exact target frame.
        let preroll = if params.codec == CODEC_TYPE_VORBIS {
            // Vorbis overlap needs a preceding maximum-size block.
            params.extra_data.as_deref().and_then(|header| header.get(28))
                .map_or(0, |sizes| 1u64 << (sizes >> 4))
        } else if params.codec == CODEC_TYPE_MP3 {
            // At the supported 44.1 kHz, MPEG-1 Layer III can reference a
            // 511-byte bit reservoir across eight minimum-bitrate frames.
            // Two more frames rebuild the overlap and synthesis history.
            10 * 1_152
        } else {
            0
        };
        let start = target.saturating_sub(preroll);
        self.probe_result.format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time: Duration::from_secs_f64(start as f64 / f64::from(SAMPLE_RATE)).into(),
                track_id: None,
            },
        )?;
        self.decoder.reset();
        self.pending = None;
        self.seek_frame = Some(target);
        self.pending = self.next_packet()?;
        let actual = self.pending.as_ref().map_or(target, |(position, _)| position.source_frame);
        Ok(((actual * 1_000 + u64::from(SAMPLE_RATE) / 2) / u64::from(SAMPLE_RATE)) as u32)
    }

    fn next_packet(&mut self) -> DecoderResult<Option<(AudioPacketPosition, AudioPacket)>> {
        if let Some(packet) = self.pending.take() {
            return Ok(Some(packet));
        }
        let mut skipped = false;

        loop {
            let packet = match self.probe_result.format.next_packet() {
                Ok(packet) => packet,
                Err(Error::IoError(err)) => {
                    if err.kind() == io::ErrorKind::UnexpectedEof {
                        return Ok(None);
                    } else {
                        return Err(DecoderError::SymphoniaDecoder(err.to_string()));
                    }
                }
                Err(err) => {
                    return Err(err.into());
                }
            };

            let source_frame = self.ts_to_frame(packet.ts());
            let position_ms = (source_frame * 1_000 / u64::from(SAMPLE_RATE)) as u32;

            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    let sample_buffer = match self.sample_buffer.as_mut() {
                        Some(buffer) => buffer,
                        None => {
                            let spec = *decoded.spec();
                            let duration = decoded.capacity() as u64;
                            self.sample_buffer.insert(SampleBuffer::new(duration, spec))
                        }
                    };

                    sample_buffer.copy_interleaved_ref(decoded);
                    let samples = sample_buffer.samples();
                    let frames = samples.len() / NUM_CHANNELS as usize;
                    if frames == 0 {
                        continue;
                    }
                    let trim = self.seek_frame.map_or(0, |target|
                        target.saturating_sub(source_frame).min(frames as u64) as usize);
                    if trim == frames {
                        continue;
                    }
                    self.seek_frame = None;
                    let source_frame = source_frame + trim as u64;
                    let packet_position = AudioPacketPosition {
                        position_ms: (source_frame * 1_000 / u64::from(SAMPLE_RATE)) as u32,
                        source_frame,
                        revision: 0,
                        skipped,
                    };
                    let samples = AudioPacket::Samples(samples[trim * NUM_CHANNELS as usize..].to_vec());
                    return Ok(Some((packet_position, samples)));
                }
                Err(Error::DecodeError(_)) => {
                    // The packet failed to decode due to corrupted or invalid data, get a new
                    // packet and try again.
                    warn!("Skipping malformed audio packet at {position_ms} ms");
                    skipped = true;
                    continue;
                }
                Err(err) => return Err(err.into()),
            }
        }
    }
}
