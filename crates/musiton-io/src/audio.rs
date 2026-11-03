//! Audio output via `cpal` (WASAPI on Windows, ALSA on Linux).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, FromSample, SampleFormat, SizedSample, StreamConfig};
use musiton_engine::rt::no_alloc;
use musiton_engine::{EngineHandle, InputSource};

/// Sample rates offered in the settings dialog (filtered by what the device supports).
pub const STANDARD_SAMPLE_RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];
/// Buffer sizes offered in the settings dialog (filtered by what the device supports).
pub const STANDARD_BUFFER_SIZES: [u32; 6] = [64, 128, 256, 512, 1024, 2048];
/// Size of the conversion buffer between engine (f32) and device format, in frames.
const SCRATCH_FRAMES: usize = 4096;
/// Largest block the engine processes at once (graphs are built for this size).
pub const ENGINE_MAX_BLOCK: usize = 1024;

/// Errors from the audio backend.
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    /// The system has no audio output device.
    #[error("no audio output device found")]
    NoDevice,
    /// The saved device is not connected.
    #[error("audio device not found: {0}")]
    DeviceNotFound(String),
    /// The device can't run at the requested sample rate.
    #[error("the device does not support {0} Hz")]
    UnsupportedSampleRate(u32),
    /// The device uses a sample format Musiton can't write.
    #[error("unsupported sample format: {0}")]
    UnsupportedFormat(String),
    /// Error reported by the audio driver.
    #[error("audio driver error: {0}")]
    Backend(#[from] cpal::Error),
}

/// An output device as shown in the settings dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDeviceInfo {
    /// Stable identifier (persist this).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// `true` for the system default device.
    pub is_default: bool,
    /// Sample rates from [`STANDARD_SAMPLE_RATES`] the device supports.
    pub sample_rates: Vec<u32>,
    /// Buffer sizes from [`STANDARD_BUFFER_SIZES`] the device supports.
    pub buffer_sizes: Vec<u32>,
}

/// What the user asked for; `None` means "device default".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputRequest {
    /// Device id from [`OutputDeviceInfo::id`].
    pub device_id: Option<String>,
    /// Sample rate in Hz.
    pub sample_rate: Option<u32>,
    /// Buffer size in frames.
    pub buffer_size: Option<u32>,
}

/// What is actually running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveOutput {
    /// Device name.
    pub device_name: String,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Buffer size in frames, if the driver reports it.
    pub buffer_size: Option<u32>,
    /// Number of output channels.
    pub channels: u16,
}

/// A running output stream. Audio stops when this is dropped.
pub struct AudioOutput {
    _stream: cpal::Stream,
    info: ActiveOutput,
    failed: Arc<AtomicBool>,
}

impl AudioOutput {
    /// What is actually running.
    pub fn info(&self) -> &ActiveOutput {
        &self.info
    }

    /// `true` once the driver reported a fatal error (e.g. device unplugged).
    pub fn has_failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
}

/// Lists all output devices of the default host.
pub fn list_output_devices() -> Result<Vec<OutputDeviceInfo>, AudioError> {
    let host = cpal::default_host();
    let default_id = host
        .default_output_device()
        .and_then(|d| d.id().ok())
        .map(|id| id.to_string());
    let mut list = Vec::new();
    for device in host.output_devices()? {
        let Ok(id) = device.id().map(|id| id.to_string()) else {
            continue;
        };
        let name = device
            .description()
            .map(|d| d.name().to_owned())
            .unwrap_or_else(|_| id.clone());
        let ranges: Vec<(u32, u32)> = device
            .supported_output_configs()
            .map(|it| {
                it.map(|r| (r.min_sample_rate(), r.max_sample_rate()))
                    .collect()
            })
            .unwrap_or_default();
        let buffer_range =
            device
                .default_output_config()
                .ok()
                .and_then(|c| match *c.buffer_size() {
                    cpal::SupportedBufferSize::Range { min, max } => Some((min, max)),
                    cpal::SupportedBufferSize::Unknown => None,
                });
        list.push(OutputDeviceInfo {
            is_default: default_id.as_deref() == Some(id.as_str()),
            id,
            name,
            sample_rates: supported_sample_rates(&ranges),
            buffer_sizes: supported_buffer_sizes(buffer_range),
        });
    }
    Ok(list)
}

/// An input device as shown in the settings dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDeviceInfo {
    /// Stable identifier (persist this).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// `true` for the system default input.
    pub is_default: bool,
    /// Number of input channels.
    pub channels: u16,
}

/// Lists all input devices of the default host.
pub fn list_input_devices() -> Result<Vec<InputDeviceInfo>, AudioError> {
    let host = cpal::default_host();
    let default_id = host
        .default_input_device()
        .and_then(|d| d.id().ok())
        .map(|id| id.to_string());
    let mut list = Vec::new();
    for device in host.input_devices()? {
        let Ok(id) = device.id().map(|id| id.to_string()) else {
            continue;
        };
        let name = device
            .description()
            .map(|d| d.name().to_owned())
            .unwrap_or_else(|_| id.clone());
        let channels = device.default_input_config().map_or(0, |c| c.channels());
        list.push(InputDeviceInfo {
            is_default: default_id.as_deref() == Some(id.as_str()),
            id,
            name,
            channels,
        });
    }
    Ok(list)
}

/// A running input stream. Recording input stops when this is dropped.
pub struct AudioInputStream {
    _stream: cpal::Stream,
    /// Device name.
    pub device_name: String,
    /// Number of input channels.
    pub channels: u16,
    /// Samples that didn't fit into the ring (the engine fell behind).
    pub overflows: Arc<std::sync::atomic::AtomicU64>,
}

/// Opens an input device at `sample_rate` (must equal the output rate). The returned
/// [`InputSource`] is handed to the engine with `EngineCommand::SetInput`.
pub fn start_input(
    device_id: Option<&str>,
    sample_rate: u32,
    buffer_size: Option<u32>,
) -> Result<(AudioInputStream, InputSource), AudioError> {
    let host = cpal::default_host();
    let device = match device_id {
        Some(id) => {
            let parsed = id
                .parse()
                .map_err(|_| AudioError::DeviceNotFound(id.to_owned()))?;
            host.device_by_id(&parsed)
                .ok_or_else(|| AudioError::DeviceNotFound(id.to_owned()))?
        }
        None => host.default_input_device().ok_or(AudioError::NoDevice)?,
    };
    let device_name = device
        .description()
        .map(|d| d.name().to_owned())
        .unwrap_or_else(|_| "Unknown device".to_owned());
    let default = device.default_input_config()?;
    let channels = default.channels();
    let format = device
        .supported_input_configs()?
        .filter(|r| r.channels() == channels)
        .filter(|r| (r.min_sample_rate()..=r.max_sample_rate()).contains(&sample_rate))
        .map(|r| r.sample_format())
        .max_by_key(|f| format_rank(*f))
        .ok_or(AudioError::UnsupportedSampleRate(sample_rate))?;
    let overflows = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let try_build =
        |buffer_size: BufferSize| -> Result<(cpal::Stream, rtrb::Consumer<f32>), AudioError> {
            let (tx, rx) =
                rtrb::RingBuffer::new(sample_rate as usize * usize::from(channels).max(1));
            let config = StreamConfig {
                channels,
                sample_rate,
                buffer_size,
            };
            let o = overflows.clone();
            let stream = match format {
                SampleFormat::F32 => build_input::<f32>(&device, config, tx, o),
                SampleFormat::I32 => build_input::<i32>(&device, config, tx, o),
                SampleFormat::I16 => build_input::<i16>(&device, config, tx, o),
                SampleFormat::U16 => build_input::<u16>(&device, config, tx, o),
                other => Err(AudioError::UnsupportedFormat(other.to_string())),
            }?;
            Ok((stream, rx))
        };
    let requested = buffer_size.map_or(BufferSize::Default, BufferSize::Fixed);
    let (stream, rx) = match try_build(requested) {
        Ok(ok) => ok,
        Err(e) if requested != BufferSize::Default => {
            tracing::warn!("input buffer size rejected ({e}), using the device default");
            try_build(BufferSize::Default)?
        }
        Err(e) => return Err(e),
    };
    stream.play()?;
    tracing::info!(device = %device_name, channels, ?format, "audio input started");
    Ok((
        AudioInputStream {
            _stream: stream,
            device_name,
            channels,
            overflows,
        },
        InputSource {
            samples: rx,
            channels: usize::from(channels),
        },
    ))
}

/// Standard rates covered by any of the given `(min, max)` ranges.
pub fn supported_sample_rates(ranges: &[(u32, u32)]) -> Vec<u32> {
    STANDARD_SAMPLE_RATES
        .into_iter()
        .filter(|r| ranges.iter().any(|&(lo, hi)| (lo..=hi).contains(r)))
        .collect()
}

/// Standard buffer sizes within `range`; all of them if the driver doesn't say.
pub fn supported_buffer_sizes(range: Option<(u32, u32)>) -> Vec<u32> {
    STANDARD_BUFFER_SIZES
        .into_iter()
        .filter(|s| range.is_none_or(|(lo, hi)| (lo..=hi).contains(s)))
        .collect()
}

/// Opens the requested device and starts a new engine on it.
/// Returns the stream and the handle to control the engine.
pub fn start_output(request: &OutputRequest) -> Result<(AudioOutput, EngineHandle), AudioError> {
    let host = cpal::default_host();
    let device = match &request.device_id {
        Some(id) => {
            let parsed = id
                .parse()
                .map_err(|_| AudioError::DeviceNotFound(id.clone()))?;
            host.device_by_id(&parsed)
                .ok_or_else(|| AudioError::DeviceNotFound(id.clone()))?
        }
        None => host.default_output_device().ok_or(AudioError::NoDevice)?,
    };
    let device_name = device
        .description()
        .map(|d| d.name().to_owned())
        .unwrap_or_else(|_| "Unknown device".to_owned());

    let default = device.default_output_config()?;
    let channels = default.channels();
    let sample_rate = request.sample_rate.unwrap_or(default.sample_rate());
    let format = device
        .supported_output_configs()?
        .filter(|r| r.channels() == channels)
        .filter(|r| (r.min_sample_rate()..=r.max_sample_rate()).contains(&sample_rate))
        .map(|r| r.sample_format())
        .max_by_key(|f| format_rank(*f))
        .ok_or(AudioError::UnsupportedSampleRate(sample_rate))?;

    let failed = Arc::new(AtomicBool::new(false));
    let mut config = StreamConfig {
        channels,
        sample_rate,
        buffer_size: request
            .buffer_size
            .map_or(BufferSize::Default, BufferSize::Fixed),
    };
    let (stream, handle) = match build_stream(&device, config, format, &failed) {
        Ok(s) => s,
        Err(e) if config.buffer_size != BufferSize::Default => {
            tracing::warn!("fixed buffer size rejected ({e}), using the device default");
            config.buffer_size = BufferSize::Default;
            build_stream(&device, config, format, &failed)?
        }
        Err(e) => return Err(e),
    };
    stream.play()?;
    let info = ActiveOutput {
        device_name,
        sample_rate,
        buffer_size: stream.buffer_size().ok(),
        channels,
    };
    tracing::info!(?info, ?format, "audio output started");
    Ok((
        AudioOutput {
            _stream: stream,
            info,
            failed,
        },
        handle,
    ))
}

fn build_input<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mut tx: rtrb::Producer<f32>,
    overflows: Arc<std::sync::atomic::AtomicU64>,
) -> Result<cpal::Stream, AudioError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            no_alloc(|| {
                let mut lost = 0;
                for &s in data {
                    if tx.push(<f32 as FromSample<T>>::from_sample_(s)).is_err() {
                        lost += 1;
                    }
                }
                if lost > 0 {
                    overflows.fetch_add(lost, Ordering::Relaxed);
                }
            });
        },
        |_err: cpal::Error| {},
        None,
    )?;
    Ok(stream)
}

/// Preference order for sample formats (higher is better).
fn format_rank(f: SampleFormat) -> u8 {
    match f {
        SampleFormat::F32 => 4,
        SampleFormat::I32 => 3,
        SampleFormat::I16 => 2,
        SampleFormat::U16 => 1,
        _ => 0,
    }
}

fn build_stream(
    device: &cpal::Device,
    config: StreamConfig,
    format: SampleFormat,
    failed: &Arc<AtomicBool>,
) -> Result<(cpal::Stream, EngineHandle), AudioError> {
    match format {
        SampleFormat::F32 => build_typed::<f32>(device, config, failed),
        SampleFormat::I32 => build_typed::<i32>(device, config, failed),
        SampleFormat::I16 => build_typed::<i16>(device, config, failed),
        SampleFormat::U16 => build_typed::<u16>(device, config, failed),
        other => Err(AudioError::UnsupportedFormat(other.to_string())),
    }
}

fn build_typed<T>(
    device: &cpal::Device,
    config: StreamConfig,
    failed: &Arc<AtomicBool>,
) -> Result<(cpal::Stream, EngineHandle), AudioError>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels);
    let sample_rate = config.sample_rate as f32;
    let (handle, mut engine) = musiton_engine::create(sample_rate, ENGINE_MAX_BLOCK);
    let shared = handle.shared();
    let mut scratch = vec![0.0_f32; SCRATCH_FRAMES * channels.max(1)];
    let mut meter = LoadMeter::default();
    let cb_shared = shared.clone();
    let err_shared = shared.clone();
    let err_failed = failed.clone();

    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            no_alloc(|| {
                let start = Instant::now();
                let chunk_len = scratch.len();
                for chunk in data.chunks_mut(chunk_len) {
                    let Some(buf) = scratch.get_mut(..chunk.len()) else {
                        chunk.fill(T::EQUILIBRIUM);
                        continue;
                    };
                    engine.process(buf, channels);
                    for (o, &s) in chunk.iter_mut().zip(buf.iter()) {
                        *o = T::from_sample(s);
                    }
                }
                let frames = data.len() / channels.max(1);
                let budget = frames as f32 / sample_rate;
                if budget > 0.0 {
                    let load = start.elapsed().as_secs_f32() / budget;
                    if load > 1.0 {
                        cb_shared.add_xrun();
                    }
                    cb_shared.set_cpu_load(meter.update(load));
                }
            });
        },
        move |err: cpal::Error| match err.kind() {
            cpal::ErrorKind::Xrun => err_shared.add_xrun(),
            cpal::ErrorKind::RealtimeDenied => {}
            _ => err_failed.store(true, Ordering::Relaxed),
        },
        None,
    )?;
    Ok((stream, handle))
}

/// Peak-hold style smoothing: rises instantly, falls slowly.
#[derive(Debug, Default)]
struct LoadMeter {
    value: f32,
}

impl LoadMeter {
    fn update(&mut self, load: f32) -> f32 {
        self.value = if load > self.value {
            load
        } else {
            self.value + (load - self.value) * 0.05
        };
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_are_filtered_by_ranges() {
        assert_eq!(supported_sample_rates(&[(48_000, 48_000)]), vec![48_000]);
        assert_eq!(
            supported_sample_rates(&[(44_100, 96_000)]),
            vec![44_100, 48_000, 88_200, 96_000]
        );
        assert!(supported_sample_rates(&[]).is_empty());
    }

    #[test]
    fn buffer_sizes_are_filtered_by_range() {
        assert_eq!(
            supported_buffer_sizes(Some((100, 600))),
            vec![128, 256, 512]
        );
        assert_eq!(supported_buffer_sizes(None), STANDARD_BUFFER_SIZES.to_vec());
    }

    #[test]
    fn load_meter_rises_fast_and_falls_slowly() {
        let mut m = LoadMeter::default();
        assert_eq!(m.update(0.8), 0.8);
        let after = m.update(0.0);
        assert!(after > 0.7 && after < 0.8);
    }
}
