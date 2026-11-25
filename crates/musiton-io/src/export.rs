//! Writing rendered audio to WAV (16/24-bit, 32-bit float), FLAC (16/24-bit), or MP3 and
//! Ogg Vorbis through the external `ffmpeg` program (see [`crate::video`]).

use std::path::Path;

use flacenc::bitsink::ByteSink;
use flacenc::component::BitRepr;
use flacenc::error::Verify;

use crate::file::AudioFileError;

/// Output file formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExportFormat {
    /// WAV, 16-bit integer.
    Wav16,
    /// WAV, 24-bit integer.
    #[default]
    Wav24,
    /// WAV, 32-bit float (lossless copy of the mix).
    Wav32Float,
    /// FLAC, 16-bit.
    Flac16,
    /// FLAC, 24-bit.
    Flac24,
    /// MP3, 320 kbit/s (needs FFmpeg).
    Mp3,
    /// Ogg Vorbis, quality 6 (about 192 kbit/s, needs FFmpeg).
    Ogg,
}

impl ExportFormat {
    /// All formats with display names.
    pub const ALL: [(ExportFormat, &'static str); 7] = [
        (ExportFormat::Wav16, "WAV 16-bit"),
        (ExportFormat::Wav24, "WAV 24-bit"),
        (ExportFormat::Wav32Float, "WAV 32-bit float"),
        (ExportFormat::Flac16, "FLAC 16-bit"),
        (ExportFormat::Flac24, "FLAC 24-bit"),
        (ExportFormat::Mp3, "MP3 320 kbit/s"),
        (ExportFormat::Ogg, "OGG Vorbis"),
    ];

    /// Integer bit depth (`None` for float).
    pub fn bits(self) -> Option<u32> {
        match self {
            ExportFormat::Wav16 | ExportFormat::Flac16 => Some(16),
            ExportFormat::Wav24 | ExportFormat::Flac24 => Some(24),
            ExportFormat::Wav32Float | ExportFormat::Mp3 | ExportFormat::Ogg => None,
        }
    }

    /// File extension.
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Flac16 | ExportFormat::Flac24 => "flac",
            ExportFormat::Mp3 => "mp3",
            ExportFormat::Ogg => "ogg",
            ExportFormat::Wav16 | ExportFormat::Wav24 | ExportFormat::Wav32Float => "wav",
        }
    }
}

/// How to write the file.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ExportOptions {
    /// Output format.
    pub format: ExportFormat,
    /// Scale so the loudest sample sits at −0.1 dBFS.
    pub normalize: bool,
    /// Add TPDF dither when reducing to 16 bit.
    pub dither: bool,
}

/// Peak level after normalizing.
const NORMALIZE_PEAK: f32 = 0.988_553; // −0.1 dBFS

/// Gain that normalizing would apply to `channels` (1.0 if silent).
pub fn normalize_gain(channels: &[Vec<f32>]) -> f32 {
    let peak = channels
        .iter()
        .flatten()
        .fold(0.0_f32, |m, s| m.max(s.abs()));
    if peak > 0.0 {
        NORMALIZE_PEAK / peak
    } else {
        1.0
    }
}

/// Small deterministic noise source for dither (so exports are reproducible).
struct Rng(u32);

impl Rng {
    fn uniform(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x as f32 / u32::MAX as f32 - 0.5
    }
}

/// Converts to integers of `bits` bits, with TPDF dither (±1 LSB) if `dither`.
pub fn quantize(channels: &[Vec<f32>], bits: u32, gain: f32, dither: bool) -> Vec<i32> {
    let scale = (1i64 << (bits - 1)) as f32;
    let (min, max) = (-scale, scale - 1.0);
    let mut rng = Rng(0x9E37_79B9);
    let frames = channels.first().map_or(0, Vec::len);
    let mut out = Vec::with_capacity(frames * channels.len());
    for i in 0..frames {
        for ch in channels {
            let x = ch.get(i).copied().unwrap_or(0.0) * gain * scale;
            let d = if dither {
                rng.uniform() + rng.uniform()
            } else {
                0.0
            };
            out.push((x + d).round().clamp(min, max) as i32);
        }
    }
    out
}

/// Writes planar `channels` to `path` in the chosen format.
pub fn write_export(
    path: &Path,
    channels: &[Vec<f32>],
    sample_rate: u32,
    opts: ExportOptions,
) -> Result<(), AudioFileError> {
    let gain = if opts.normalize {
        normalize_gain(channels)
    } else {
        1.0
    };
    let n_ch = channels.len().max(1);
    let werr = |e: hound::Error| AudioFileError::Write(e.to_string());
    match opts.format {
        ExportFormat::Wav32Float => write_float_wav(path, channels, sample_rate, gain),
        ExportFormat::Wav16 | ExportFormat::Wav24 => {
            let bits = opts.format.bits().unwrap_or(24);
            let spec = hound::WavSpec {
                channels: n_ch as u16,
                sample_rate,
                bits_per_sample: bits as u16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut w = hound::WavWriter::create(path, spec).map_err(werr)?;
            for s in quantize(channels, bits, gain, opts.dither && bits == 16) {
                w.write_sample(s).map_err(werr)?;
            }
            w.finalize().map_err(werr)
        }
        ExportFormat::Flac16 | ExportFormat::Flac24 => {
            let bits = opts.format.bits().unwrap_or(24);
            let samples = quantize(channels, bits, gain, opts.dither && bits == 16);
            let ferr = |e: &dyn std::fmt::Debug| AudioFileError::Write(format!("{e:?}"));
            let config = flacenc::config::Encoder::default()
                .into_verified()
                .map_err(|e| ferr(&e))?;
            let source = flacenc::source::MemSource::from_samples(
                &samples,
                n_ch,
                bits as usize,
                sample_rate as usize,
            );
            let stream = flacenc::encode_with_fixed_block_size(&config, source, 4096)
                .map_err(|e| ferr(&e))?;
            let mut sink = ByteSink::new();
            stream.write(&mut sink).map_err(|e| ferr(&e))?;
            let mut bytes = sink.as_slice().to_vec();
            // flacenc stores the (shorter) last block as the minimum block size; for a
            // fixed-block-size stream the spec wants min = max, and some decoders
            // (symphonia) otherwise treat the file as variable-block-size.
            // STREAMINFO: "fLaC" (4) + block header (4) + min (2) + max (2).
            if let (Some(max), true) = (bytes.get(10..12).map(|m| [m[0], m[1]]), bytes.len() > 12) {
                if let Some(min) = bytes.get_mut(8..10) {
                    min.copy_from_slice(&max);
                }
            }
            std::fs::write(path, bytes)?;
            Ok(())
        }
        ExportFormat::Mp3 => encode_lossy(path, channels, sample_rate, gain, MP3_ARGS),
        ExportFormat::Ogg => encode_lossy(path, channels, sample_rate, gain, OGG_ARGS),
    }
}

/// Writes a 32-bit float WAV, scaled by `gain`.
fn write_float_wav(
    path: &Path,
    channels: &[Vec<f32>],
    sample_rate: u32,
    gain: f32,
) -> Result<(), AudioFileError> {
    let werr = |e: hound::Error| AudioFileError::Write(e.to_string());
    let spec = hound::WavSpec {
        channels: channels.len().max(1) as u16,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec).map_err(werr)?;
    let frames = channels.first().map_or(0, Vec::len);
    for i in 0..frames {
        for ch in channels {
            w.write_sample(ch.get(i).copied().unwrap_or(0.0) * gain)
                .map_err(werr)?;
        }
    }
    w.finalize().map_err(werr)
}

/// FFmpeg encoder settings for MP3 and Ogg Vorbis.
const MP3_ARGS: &[&str] = &["-c:a", "libmp3lame", "-b:a", "320k"];
const OGG_ARGS: &[&str] = &["-c:a", "libvorbis", "-q:a", "6"];

/// Writes a temporary 32-bit float WAV and lets FFmpeg encode it to `path`.
fn encode_lossy(
    path: &Path,
    channels: &[Vec<f32>],
    sample_rate: u32,
    gain: f32,
    codec: &[&str],
) -> Result<(), AudioFileError> {
    let exe = crate::video::ffmpeg().map_err(|e| AudioFileError::Write(e.to_string()))?;
    let tmp = std::env::temp_dir().join(format!(
        "musiton-encode-{}-{}.wav",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let result = write_float_wav(&tmp, channels, sample_rate, gain).and_then(|()| {
        let out = crate::video::command(exe)
            .arg("-y")
            .arg("-i")
            .arg(&tmp)
            .args(codec)
            .arg(path)
            .output()?;
        if out.status.success() {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&out.stderr);
            Err(AudioFileError::Write(format!(
                "FFmpeg could not encode the file: {}",
                msg.trim()
            )))
        }
    });
    // The temporary file may already be gone; nothing to do then.
    let _ = std::fs::remove_file(&tmp);
    result
}

/// Makes temporary file names unique within this process.
static TEMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::decode_file;

    fn signal() -> Vec<Vec<f32>> {
        let l: Vec<f32> = (0..20_000).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
        let r: Vec<f32> = l.iter().map(|x| -x * 0.25).collect();
        vec![l, r]
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("musiton-export-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    #[test]
    fn float_wav_is_bit_identical() {
        let s = signal();
        let p = tmp("f.wav");
        write_export(
            &p,
            &s,
            48_000,
            ExportOptions {
                format: ExportFormat::Wav32Float,
                ..Default::default()
            },
        )
        .unwrap();
        let back = decode_file(&p).unwrap();
        assert_eq!(back.channels, s);
    }

    #[test]
    fn mp3_and_ogg_sound_like_the_mix() {
        if crate::video::ffmpeg().is_err() {
            eprintln!("ffmpeg not available; skipping");
            return;
        }
        let s = signal();
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
        for format in [ExportFormat::Mp3, ExportFormat::Ogg] {
            let p = tmp(&format!("lossy.{}", format.extension()));
            let opts = ExportOptions {
                format,
                ..Default::default()
            };
            write_export(&p, &s, 48_000, opts).unwrap();
            let back = decode_file(&p).unwrap();
            assert_eq!(back.channels.len(), 2, "{format:?}");
            // Encoders add a little silence at the start; compare levels, not samples.
            for (orig, dec) in s.iter().zip(&back.channels) {
                let ratio = rms(&dec[2_000..18_000]) / rms(&orig[2_000..18_000]);
                assert!(
                    (0.9..1.1).contains(&ratio),
                    "{format:?}: level ratio {ratio}"
                );
            }
        }
    }

    #[test]
    fn integer_formats_round_trip_within_one_step() {
        let s = signal();
        for (format, bits) in [
            (ExportFormat::Wav16, 16),
            (ExportFormat::Wav24, 24),
            (ExportFormat::Flac16, 16),
            (ExportFormat::Flac24, 24),
        ] {
            let p = tmp(&format!("x.{}", format.extension()));
            write_export(
                &p,
                &s,
                44_100,
                ExportOptions {
                    format,
                    normalize: false,
                    dither: true,
                },
            )
            .unwrap();
            let back = decode_file(&p).unwrap_or_else(|e| panic!("{format:?}: {e}"));
            assert_eq!(back.sample_rate, 44_100);
            let lsb = 1.0 / (1u32 << (bits - 1)) as f32;
            // TPDF dither adds up to ±1 LSB at 16 bit.
            let tol = if bits == 16 { 1.6 * lsb } else { 0.6 * lsb };
            for (ch, original) in s.iter().enumerate() {
                let worst = back.channels[ch]
                    .iter()
                    .zip(original)
                    .fold(0.0_f32, |m, (a, b)| m.max((a - b).abs()));
                assert!(worst <= tol, "{format:?} ch{ch}: {worst} > {tol}");
            }
        }
    }

    #[test]
    fn normalize_brings_the_peak_to_minus_0_1_db() {
        let s = signal();
        let p = tmp("n.wav");
        write_export(
            &p,
            &s,
            48_000,
            ExportOptions {
                format: ExportFormat::Wav32Float,
                normalize: true,
                dither: false,
            },
        )
        .unwrap();
        let back = decode_file(&p).unwrap();
        let peak = back
            .channels
            .iter()
            .flatten()
            .fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!((peak - NORMALIZE_PEAK).abs() < 1e-5);
        assert_eq!(normalize_gain(&[vec![0.0; 4]]), 1.0);
    }
}
