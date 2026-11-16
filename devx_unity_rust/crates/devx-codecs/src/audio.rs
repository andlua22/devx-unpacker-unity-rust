//! Decoders and exporters for Unity `AudioClip` (`class_id == 83`) objects,
//! including `FSB5` sound bank unpacking via `fsbex` (Ogg Vorbis / PCM -> WAV),
//! container playback/decoding via `symphonia`, WAV encoding via `hound`, and waveform envelopes.

use crate::{CodecError, CodecResult};
use devx_plugin_api::DecodedAudio;
use devx_unity_io::{ResourceStreamStore, extract_bytes_field, extract_i32_field, extract_i64_field};
use fsbex::{AudioFormat, Bank};
use hound::{SampleFormat, WavSpec, WavWriter};
use std::io::Cursor;
use symphonia::core::codecs::{CodecParameters, audio::AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use unity_asset_binary::object::UnityObject;
use unity_asset_decode::audio::AudioClipConverter;
use unity_asset_decode::unity_version::UnityVersion;

/// One extracted sub-stream inside an `FSB5` bank (most Unity clips hold 1 stream).
#[derive(Debug, Clone)]
pub struct FsbSubStream {
    pub index: usize,
    pub name: Option<String>,
    pub extension: String,
    pub bytes: Vec<u8>,
    pub decoded: Option<DecodedAudio>,
}

/// Complete decoded audio payload plus waveform envelope for UI visualization.
#[derive(Debug, Clone)]
pub struct DecodedAudioClip {
    pub name: String,
    pub primary: DecodedAudio,
    pub sub_streams: Vec<FsbSubStream>,
    pub waveform_peaks: Vec<(f32, f32)>,
}

/// Decodes a Unity `AudioClip` (`class_id == 83`), resolving `.resource` / `.resS` streams and unpacking FSB5 banks.
pub fn decode_audio_clip(
    obj: &UnityObject,
    unity_version: &str,
    streams: &ResourceStreamStore,
) -> CodecResult<DecodedAudioClip> {
    let props = obj.class.properties();
    let mut name = props
        .get("m_Name")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
        .or_else(|| obj.name())
        .unwrap_or_else(|| "AudioClip".to_owned());

    let mut channels_hint = props
        .get("m_Channels")
        .and_then(extract_i32_field)
        .unwrap_or(0)
        .max(0) as u16;
    let mut freq_hint = props
        .get("m_Frequency")
        .and_then(extract_i32_field)
        .unwrap_or(0)
        .max(0) as u32;
    let mut bits_hint = props
        .get("m_BitsPerSample")
        .and_then(extract_i32_field)
        .unwrap_or(16)
        .max(8) as u16;
    let compression_hint = props
        .get("m_CompressionFormat")
        .and_then(extract_i32_field)
        .unwrap_or(-1);

    let mut payload = props
        .get("m_AudioData")
        .and_then(extract_bytes_field)
        .unwrap_or_default();

    let mut stream_source = String::new();
    let mut stream_offset = 0u64;
    let mut stream_size = 0u32;

    if let Some(res) = props.get("m_Resource").and_then(|v| v.as_object()) {
        if let Some(s) = res.get("m_Source").and_then(|v| v.as_str()) {
            stream_source = s.to_owned();
        }
        if let Some(off) = res.get("m_Offset").and_then(extract_i64_field) {
            stream_offset = off.max(0) as u64;
        }
        if let Some(sz) = res.get("m_Size").and_then(extract_i64_field) {
            stream_size = sz.max(0) as u32;
        }
    }

    if payload.is_empty() && props.is_empty() {
        let ver = UnityVersion::parse_version(unity_version).unwrap_or_default();
        if let Ok(clip) = AudioClipConverter::new(ver).from_unity_object(obj) {
            if !clip.name.is_empty() {
                name = clip.name.clone();
            }
            let ap = clip.properties();
            if ap.channels > 0 {
                channels_hint = ap.channels as u16;
            }
            if ap.sample_rate > 0 {
                freq_hint = ap.sample_rate as u32;
            }
            if ap.bits_per_sample > 0 {
                bits_hint = ap.bits_per_sample as u16;
            }
            if payload.is_empty() && !clip.stream_info.path.is_empty() && clip.stream_info.size > 0 {
                stream_source = clip.stream_info.path.clone();
                stream_offset = clip.stream_info.offset;
                stream_size = clip.stream_info.size;
            }
            payload = clip.data;
        }
    }

    if payload.is_empty() && !stream_source.is_empty() && stream_size > 0 {
        let Some(slice) = streams.read_slice(&stream_source, stream_offset, stream_size) else {
            return Err(CodecError::MissingStream(stream_source));
        };
        payload = slice.to_vec();
    }

    if payload.is_empty() {
        return Err(CodecError::MalformedData(format!(
            "AudioClip '{name}' has no embedded or streamed audio bytes"
        )));
    }

    if payload.starts_with(b"FSB5") {
        return decode_fsb5_bank(&name, &payload);
    }

    if let Some((ext, fmt_name)) = detect_container_hint(&payload) {
        match decode_container_bytes(&payload, Some(ext), fmt_name) {
            Ok(mut audio) => {
                if audio.sample_rate == 0 && freq_hint > 0 {
                    audio.sample_rate = freq_hint;
                }
                if audio.channels == 0 && channels_hint > 0 {
                    audio.channels = channels_hint;
                }
                audio.raw_container = Some((ext.to_owned(), payload));
                let peaks = compute_waveform_peaks(&audio.samples_i16, audio.channels, 256);
                return Ok(DecodedAudioClip {
                    name,
                    primary: audio,
                    sub_streams: Vec::new(),
                    waveform_peaks: peaks,
                });
            }
            Err(_) if compression_hint == 0 => {}
            Err(err) => return Err(err),
        }
    }

    // Headerless raw PCM fallback (`AudioCompressionFormat::PCM == 0`).
    if (compression_hint == 0 || compression_hint == -1) && freq_hint > 0 && channels_hint > 0 {
        let samples_i16 = raw_pcm_to_i16(&payload, bits_hint);
        let duration_secs = samples_i16.len() as f32 / (freq_hint as f32 * channels_hint as f32);
        let mut primary = DecodedAudio {
            sample_rate: freq_hint,
            channels: channels_hint,
            samples_i16,
            source_format: format!("Raw PCM {bits_hint}-bit"),
            duration_secs,
            raw_container: None,
        };
        if let Ok(wav) = encode_wav(&primary) {
            primary.raw_container = Some(("wav".to_owned(), wav));
        }
        let peaks = compute_waveform_peaks(&primary.samples_i16, primary.channels, 256);
        return Ok(DecodedAudioClip {
            name,
            primary,
            sub_streams: Vec::new(),
            waveform_peaks: peaks,
        });
    }

    // Preserve unknown compressed payload without failing UI metadata view if undecodable.
    Err(CodecError::UnsupportedFormat(format!(
        "AudioClip '{name}' unrecognized container ({} bytes)",
        payload.len()
    )))
}

fn decode_fsb5_bank(name: &str, bytes: &[u8]) -> CodecResult<DecodedAudioClip> {
    let bank = Bank::new(Cursor::new(bytes))
        .map_err(|e| CodecError::MalformedData(format!("FSB5 header parse failed: {e}")))?;
    let bank_format = bank.format();

    let mut sub_streams = Vec::new();
    for (idx, stream) in bank.into_iter().enumerate() {
        let stream_name = stream.name().map(str::to_owned);
        let sr = u32::from(stream.sample_rate());
        let ch = u8::from(stream.channels()) as u16;
        let (ext, fmt_label) = match bank_format {
            AudioFormat::Vorbis => ("ogg", "FSB5 (Vorbis)"),
            AudioFormat::Pcm8
            | AudioFormat::Pcm16
            | AudioFormat::Pcm24
            | AudioFormat::Pcm32
            | AudioFormat::PcmFloat => ("wav", "FSB5 (PCM)"),
            other => {
                sub_streams.push(FsbSubStream {
                    index: idx,
                    name: stream_name,
                    extension: "fsb".to_owned(),
                    bytes: Vec::new(),
                    decoded: Some(DecodedAudio {
                        sample_rate: sr,
                        channels: ch,
                        samples_i16: Vec::new(),
                        source_format: format!("FSB5 ({other:?})"),
                        duration_secs: 0.0,
                        raw_container: None,
                    }),
                });
                continue;
            }
        };

        let mut encoded = Vec::new();
        if stream.write(&mut encoded).is_ok() {
            let mut decoded = decode_container_bytes(&encoded, Some(ext), fmt_label).ok();
            if let Some(ref mut a) = decoded {
                a.raw_container = Some((ext.to_owned(), encoded.clone()));
            }
            sub_streams.push(FsbSubStream {
                index: idx,
                name: stream_name,
                extension: ext.to_owned(),
                bytes: encoded,
                decoded,
            });
        }
    }

    let primary = sub_streams
        .iter()
        .find_map(|s| s.decoded.clone())
        .ok_or_else(|| {
            CodecError::UnsupportedFormat(format!(
                "FSB5 bank '{name}' format {bank_format:?} has no decodable streams"
            ))
        })?;
    let peaks = compute_waveform_peaks(&primary.samples_i16, primary.channels, 256);

    Ok(DecodedAudioClip {
        name: name.to_owned(),
        primary,
        sub_streams,
        waveform_peaks: peaks,
    })
}

fn detect_container_hint(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(b"OggS") {
        Some(("ogg", "Ogg Vorbis"))
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE") {
        Some(("wav", "WAV"))
    } else if bytes.starts_with(b"fLaC") {
        Some(("flac", "FLAC"))
    } else if bytes.starts_with(b"ID3")
        || (bytes.len() >= 2 && bytes[0] == 0xFF && (bytes[1] & 0xE0) == 0xE0)
    {
        if bytes.len() >= 2 && bytes[0] == 0xFF && (bytes[1] & 0xF6) == 0xF0 {
            Some(("aac", "AAC ADTS"))
        } else {
            Some(("mp3", "MP3"))
        }
    } else {
        None
    }
}

/// Decodes an encoded audio container (`WAV`, `OGG`, `MP3`, `AAC`, `FLAC`) via Symphonia 0.6.
pub fn decode_container_bytes(
    bytes: &[u8],
    ext_hint: Option<&str>,
    format_label: &str,
) -> CodecResult<DecodedAudio> {
    let mut hint = Hint::new();
    if let Some(ext) = ext_hint {
        hint.with_extension(ext);
    }

    let cursor = Cursor::new(bytes.to_vec());
    let mss = MediaSourceStream::new(Box::new(cursor), MediaSourceStreamOptions::default());

    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| CodecError::MalformedData(format!("audio probe failed: {e}")))?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| CodecError::MalformedData("no audio track found".to_owned()))?;
    let track_id = track.id;
    let audio_params = match &track.codec_params {
        Some(CodecParameters::Audio(p)) => p.clone(),
        _ => {
            return Err(CodecError::MalformedData(
                "selected track has no audio codec parameters".to_owned(),
            ));
        }
    };

    let mut sample_rate = audio_params.sample_rate.unwrap_or(44_100);
    let mut channels = audio_params
        .channels
        .as_ref()
        .map(|c| c.count() as u16)
        .unwrap_or(1);

    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&audio_params, &AudioDecoderOptions::default())
        .map_err(|e| CodecError::UnsupportedFormat(format!("audio decoder init failed: {e}")))?;

    let mut samples_i16 = Vec::new();
    let mut pkt_scratch: Vec<i16> = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(Some(pkt)) => pkt,
            Ok(None) => break,
            Err(SymphoniaError::ResetRequired) => break,
            Err(SymphoniaError::IoError(ref io_err))
                if io_err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(_) => break,
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = decoded.spec();
                sample_rate = spec.rate();
                channels = spec.channels().count().max(1) as u16;
                pkt_scratch.clear();
                decoded.copy_to_vec_interleaved(&mut pkt_scratch);
                samples_i16.extend_from_slice(&pkt_scratch);
            }
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(_) => break,
        }
    }

    let duration_secs = if sample_rate > 0 && channels > 0 {
        samples_i16.len() as f32 / (sample_rate as f32 * channels as f32)
    } else {
        0.0
    };

    Ok(DecodedAudio {
        sample_rate,
        channels,
        samples_i16,
        source_format: format_label.to_owned(),
        duration_secs,
        raw_container: None,
    })
}

fn raw_pcm_to_i16(bytes: &[u8], bits: u16) -> Vec<i16> {
    match bits {
        8 => bytes.iter().map(|&b| (b as i16 - 128) * 256).collect(),
        32 => bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| {
                let f = f32::from_le_bytes(*c).clamp(-1.0, 1.0);
                (f * i16::MAX as f32) as i16
            })
            .collect(),
        _ => bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| i16::from_le_bytes(*c))
            .collect(),
    }
}

/// Encodes decoded PCM `i16` samples into a standard RIFF WAVE file.
pub fn encode_wav(audio: &DecodedAudio) -> CodecResult<Vec<u8>> {
    let spec = WavSpec {
        channels: audio.channels.max(1),
        sample_rate: audio.sample_rate.max(8_000),
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut buf = Vec::new();
    {
        let mut writer = WavWriter::new(Cursor::new(&mut buf), spec)
            .map_err(|e| CodecError::MalformedData(format!("WAV header error: {e}")))?;
        for &s in &audio.samples_i16 {
            writer
                .write_sample(s)
                .map_err(|e| CodecError::MalformedData(format!("WAV sample write error: {e}")))?;
        }
        writer
            .finalize()
            .map_err(|e| CodecError::MalformedData(format!("WAV finalize error: {e}")))?;
    }
    Ok(buf)
}

/// Computes normalized `(-1..=1)` min/max envelope buckets for fast egui waveform rendering.
pub fn compute_waveform_peaks(samples_i16: &[i16], channels: u16, buckets: usize) -> Vec<(f32, f32)> {
    let ch = channels.max(1) as usize;
    let total_frames = samples_i16.len() / ch;
    if total_frames == 0 || buckets == 0 {
        return Vec::new();
    }
    let count = buckets.min(total_frames);
    let mut out = Vec::with_capacity(count);
    for b in 0..count {
        let start = b * total_frames / count;
        let end = ((b + 1) * total_frames / count).max(start + 1).min(total_frames);
        let mut min_v = 1.0f32;
        let mut max_v = -1.0f32;
        for f in start..end {
            let s = samples_i16[f * ch] as f32 / 32768.0;
            min_v = min_v.min(s);
            max_v = max_v.max(s);
        }
        out.push((min_v, max_v));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_decodes_wav_roundtrip() {
        let audio = DecodedAudio {
            sample_rate: 44_100,
            channels: 1,
            samples_i16: (0..441)
                .map(|i| ((i as f32 * 0.1).sin() * 16_000.0) as i16)
                .collect(),
            source_format: "TestPCM".into(),
            duration_secs: 0.01,
            raw_container: None,
        };
        let wav = encode_wav(&audio).expect("wav encode");
        assert!(wav.starts_with(b"RIFF"));

        let decoded = decode_container_bytes(&wav, Some("wav"), "WAV").expect("wav decode");
        assert_eq!(decoded.sample_rate, 44_100);
        assert_eq!(decoded.channels, 1);
        assert_eq!(decoded.samples_i16.len(), 441);

        let peaks = compute_waveform_peaks(&decoded.samples_i16, 1, 32);
        assert_eq!(peaks.len(), 32);
    }
}
