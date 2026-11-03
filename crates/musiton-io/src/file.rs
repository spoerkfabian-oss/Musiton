//! Audio files: decode (WAV, FLAC, MP3, OGG via symphonia), resample (rubato), write WAV.

use std::fs::File;
use std::path::{Path, PathBuf};

use musiton_core::audio::AudioData;
use rubato::Resampler;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// File extensions offered for import.
pub const AUDIO_EXTENSIONS: [&str; 6] = ["wav", "flac", "mp3", "ogg", "aif", "aiff"];

/// Errors reading, converting or writing audio files.
#[derive(Debug, thiserror::Error)]
pub enum AudioFileError {
    /// File system error.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// The file could not be decoded.
    #[error("cannot read audio file: {0}")]
    Decode(String),
    /// Resampling failed.
    #[error("sample rate conversion failed: {0}")]
    Resample(String),
    /// Writing the WAV file failed.
    #[error("cannot write audio file: {0}")]
    Write(String),
}

/// `true` if `path` has an importable audio extension.
pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Decodes a whole audio file into memory.
pub fn decode_file(path: &Path) -> Result<AudioData, AudioFileError> {
    let file = File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let dec = |e: SymError| AudioFileError::Decode(e.to_string());
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(dec)?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| AudioFileError::Decode("no audio track".into()))?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| AudioFileError::Decode("no audio parameters".into()))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(dec)?;
    let track_id = track.id;

    let mut out = AudioData::default();
    let mut interleaved: Vec<f32> = Vec::new();
    while let Some(packet) = format.next_packet().map_err(dec)? {
        if packet.track_id != track_id {
            continue;
        }
        let buf = match decoder.decode(&packet) {
            Ok(b) => b,
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(dec(e)),
        };
        let spec = buf.spec();
        let ch = spec.channels().count().max(1);
        if out.channels.is_empty() {
            out.sample_rate = spec.rate();
            out.channels = vec![Vec::new(); ch];
        }
        interleaved.resize(buf.samples_interleaved(), 0.0);
        buf.copy_to_slice_interleaved(&mut interleaved);
        for frame in interleaved.chunks_exact(ch) {
            for (dst, &s) in out.channels.iter_mut().zip(frame) {
                dst.push(s);
            }
        }
    }
    if out.frames() == 0 {
        return Err(AudioFileError::Decode("file contains no audio".into()));
    }
    Ok(out)
}

/// Converts `audio` to `target_rate` (high-quality FFT resampler). No-op if equal.
pub fn resample(audio: AudioData, target_rate: u32) -> Result<AudioData, AudioFileError> {
    if audio.sample_rate == target_rate || audio.frames() == 0 {
        return Ok(audio);
    }
    let err = |e: &dyn std::fmt::Display| AudioFileError::Resample(e.to_string());
    let channels = audio.num_channels();
    let frames = audio.frames();
    let mut rs = rubato::Fft::<f32>::new(
        audio.sample_rate as usize,
        target_rate as usize,
        1024,
        channels,
        rubato::FixedSync::Both,
    )
    .map_err(|e| err(&e))?;
    let out_len = rs.process_all_needed_output_len(frames);
    let mut out = vec![vec![0.0_f32; out_len]; channels];
    let input =
        audioadapter_buffers::direct::SequentialSliceOfVecs::new(&audio.channels, channels, frames)
            .map_err(|e| err(&e))?;
    let mut output =
        audioadapter_buffers::direct::SequentialSliceOfVecs::new_mut(&mut out, channels, out_len)
            .map_err(|e| err(&e))?;
    let (_, produced) = rs
        .process_all_into_buffer(&input, &mut output, frames, None)
        .map_err(|e| err(&e))?;
    for ch in &mut out {
        ch.truncate(produced);
    }
    Ok(AudioData {
        sample_rate: target_rate,
        channels: out,
    })
}

/// Writes `audio` as a 32-bit float WAV file.
pub fn write_wav(audio: &AudioData, path: &Path) -> Result<(), AudioFileError> {
    let err = |e: hound::Error| AudioFileError::Write(e.to_string());
    let spec = hound::WavSpec {
        channels: audio.num_channels() as u16,
        sample_rate: audio.sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec).map_err(err)?;
    for i in 0..audio.frames() {
        for ch in &audio.channels {
            w.write_sample(ch.get(i).copied().unwrap_or(0.0))
                .map_err(err)?;
        }
    }
    w.finalize().map_err(err)
}

/// An audio file copied into a project.
#[derive(Debug)]
pub struct Imported {
    /// Path relative to the project folder, e.g. `audio/kick.wav`.
    pub rel_path: String,
    /// The audio at the project rate.
    pub audio: AudioData,
}

/// Decodes `src`, converts it to `project_rate` and stores it as WAV in `<project_dir>/audio/`
/// (a free file name is chosen if the name is taken).
pub fn import_audio(
    src: &Path,
    project_dir: &Path,
    project_rate: u32,
) -> Result<Imported, AudioFileError> {
    let audio = resample(decode_file(src)?, project_rate)?;
    let dir = project_dir.join("audio");
    std::fs::create_dir_all(&dir)?;
    let stem = src
        .file_stem()
        .map_or("audio".to_owned(), |s| s.to_string_lossy().into_owned());
    let target = free_name(&dir, &stem);
    write_wav(&audio, &target)?;
    let file = target
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    Ok(Imported {
        rel_path: format!("audio/{file}"),
        audio,
    })
}

pub(crate) fn free_name(dir: &Path, stem: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.wav"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|i| dir.join(format!("{stem}-{i}.wav")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("musiton-io-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sine(rate: u32, secs: f32, hz: f32) -> AudioData {
        let n = (rate as f32 * secs) as usize;
        let s: Vec<f32> = (0..n)
            .map(|i| 0.5 * (std::f32::consts::TAU * hz * i as f32 / rate as f32).sin())
            .collect();
        AudioData {
            sample_rate: rate,
            channels: vec![s.clone(), s],
        }
    }

    fn crossings(x: &[f32]) -> usize {
        x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count()
    }

    #[test]
    fn i16_wav_decodes() {
        let dir = tmp("i16");
        let path = dir.join("t.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..1000 {
            w.write_sample(if i < 500 { 16_384i16 } else { -16_384 })
                .unwrap();
        }
        w.finalize().unwrap();
        let a = decode_file(&path).unwrap();
        assert_eq!(
            (a.sample_rate, a.num_channels(), a.frames()),
            (44_100, 1, 1000)
        );
        assert!((a.channel(0)[10] - 0.5).abs() < 1e-3);
        assert!((a.channel(0)[900] + 0.5).abs() < 1e-3);
    }

    #[test]
    fn import_resamples_and_copies_into_project() {
        let dir = tmp("import");
        let src = dir.join("tone.wav");
        write_wav(&sine(44_100, 1.0, 441.0), &src).unwrap();
        let project = dir.join("Song");
        let a = import_audio(&src, &project, 48_000).unwrap();
        assert_eq!(a.rel_path, "audio/tone.wav");
        assert_eq!(a.audio.sample_rate, 48_000);
        assert!(
            (a.audio.frames() as i64 - 48_000).abs() <= 2,
            "{}",
            a.audio.frames()
        );
        assert!((439..=442).contains(&crossings(a.audio.channel(0))));
        let peak = a.audio.channel(1)[1000..47_000]
            .iter()
            .fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 0.01, "peak {peak}");
        let back = decode_file(&project.join(&a.rel_path)).unwrap();
        assert_eq!(back, a.audio);
        // Same name again gets a new file.
        assert_eq!(
            import_audio(&src, &project, 48_000).unwrap().rel_path,
            "audio/tone-2.wav"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        let dir = tmp("garbage");
        let path = dir.join("x.wav");
        std::fs::write(&path, b"definitely not audio").unwrap();
        assert!(decode_file(&path).is_err());
        assert!(is_audio_file(Path::new("a/B.FLAC")));
        assert!(!is_audio_file(Path::new("a/b.txt")));
    }
}
