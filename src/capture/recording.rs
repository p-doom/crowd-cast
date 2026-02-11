//! Recording output management for embedded libobs
//!
//! Handles creating and managing recording outputs with proper encoder configuration.
//! Uses HEVC hardware encoding (VideoToolbox on macOS) when available, falling back
//! to H.264 hardware encoding, then software encoding.

use anyhow::Result;
use libobs_simple::output::simple::{
    HardwareCodec, HardwarePreset, OutputFormat, SimpleOutputBuilder,
};
use libobs_wrapper::context::ObsContext;
use libobs_wrapper::data::output::ObsOutputRef;
use libobs_wrapper::utils::ObsPath;
use std::path::PathBuf;
use tracing::{debug, info};

/// Calculate output dimensions with aspect-preserving downscale
///
/// Downscales to max_height while preserving aspect ratio.
/// Ensures dimensions are even (required by most video encoders).
///
/// # Arguments
/// * `base_width` - Source width in pixels
/// * `base_height` - Source height in pixels
/// * `max_height` - Maximum output height (0 = no limit, use native)
///
/// # Returns
/// Tuple of (output_width, output_height), both guaranteed to be even
pub fn calculate_output_dimensions(
    base_width: u32,
    base_height: u32,
    max_height: u32,
) -> (u32, u32) {
    // If max_height is 0 or source is already at/below max, use native (but ensure even)
    if max_height == 0 || base_height <= max_height {
        return (make_even(base_width), make_even(base_height));
    }

    // Calculate aspect-preserving dimensions
    let aspect = base_width as f64 / base_height as f64;
    let output_height = max_height;
    let output_width = (output_height as f64 * aspect).round() as u32;

    (make_even(output_width), make_even(output_height))
}

/// Ensure a value is even (required by most video encoders)
fn make_even(v: u32) -> u32 {
    if v % 2 == 0 {
        v
    } else {
        v + 1
    }
}

/// Recording state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecordingState {
    #[default]
    Stopped,
    Recording,
    Paused,
}

/// Video codec preference for recording
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VideoCodecPreference {
    /// Prefer HEVC (H.265) with hardware encoding when available
    /// Falls back to H.264 hardware, then software encoding
    #[default]
    HevcPreferred,
    /// Prefer H.264 with hardware encoding
    /// More compatible but larger file sizes
    H264Preferred,
    /// Prefer AV1 with hardware encoding when available
    /// Best compression but limited hardware support
    Av1Preferred,
}

/// Recording configuration
#[derive(Debug, Clone)]
pub struct RecordingConfig {
    /// Video bitrate in Kbps
    pub video_bitrate: u32,
    /// Audio bitrate in Kbps (only used if enable_audio is true)
    pub audio_bitrate: u32,
    /// Whether to capture audio (disabled by default)
    pub enable_audio: bool,
    /// Preferred video codec
    pub codec_preference: VideoCodecPreference,
    /// Hardware encoder quality preset
    pub quality_preset: HardwarePreset,
    /// Output format
    pub format: OutputFormat,
    /// Maximum output height in pixels (width auto-calculated to preserve aspect ratio)
    /// Set to 0 to use native resolution
    pub max_output_height: u32,
    /// Frames per second
    pub fps: u32,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            // 3 Mbps, assuming 720p30 screen capture with HEVC
            // Good balance between storage efficiency and text legibility
            video_bitrate: 3000,
            // 160 Kbps - good quality for system audio (if enabled)
            audio_bitrate: 160,
            // Audio disabled by default - video only
            enable_audio: false,
            // Prefer HEVC for better compression
            codec_preference: VideoCodecPreference::HevcPreferred,
            // Balanced quality - good tradeoff between speed and quality
            quality_preset: HardwarePreset::Balanced,
            // Hybrid MP4 - recoverable and widely compatible
            format: OutputFormat::HybridMP4,
            // 720p max height
            max_output_height: 720,
            // 30 FPS
            fps: 30,
        }
    }
}

impl RecordingConfig {
    /// Create config optimized for high quality recording (with audio)
    /// Uses native resolution (no downscaling)
    pub fn high_quality() -> Self {
        Self {
            video_bitrate: 15000,
            audio_bitrate: 192,
            enable_audio: true,
            codec_preference: VideoCodecPreference::HevcPreferred,
            quality_preset: HardwarePreset::Quality,
            format: OutputFormat::HybridMP4,
            // 0 = native resolution
            max_output_height: 0,
            fps: 30,
        }
    }

    /// Create config optimized for smaller file sizes (video only)
    /// Uses 720p with minimum viable bitrate for legible text
    pub fn compact() -> Self {
        Self {
            video_bitrate: 2500,
            audio_bitrate: 128,
            enable_audio: false,
            codec_preference: VideoCodecPreference::HevcPreferred,
            quality_preset: HardwarePreset::Speed,
            format: OutputFormat::HybridMP4,
            max_output_height: 720,
            fps: 30,
        }
    }

    /// Create config optimized for maximum compatibility (video only)
    /// Uses H.264 which requires higher bitrate than HEVC
    pub fn compatible() -> Self {
        Self {
            video_bitrate: 4000,
            audio_bitrate: 160,
            enable_audio: false,
            codec_preference: VideoCodecPreference::H264Preferred,
            quality_preset: HardwarePreset::Balanced,
            format: OutputFormat::Mpeg4,
            max_output_height: 720,
            fps: 30,
        }
    }

    /// Enable audio recording
    pub fn with_audio(mut self) -> Self {
        self.enable_audio = true;
        self
    }

    /// Disable audio recording (video only)
    pub fn without_audio(mut self) -> Self {
        self.enable_audio = false;
        self
    }
}

/// Manages a recording output
pub struct RecordingOutput {
    output: ObsOutputRef,
    state: RecordingState,
    output_path: PathBuf,
}

impl RecordingOutput {
    /// Create a new recording output with the specified configuration
    ///
    /// This will automatically select the best available encoder:
    /// - macOS: VideoToolbox HEVC/H.264
    /// - Windows/Linux: NVENC, AMF, QSV (in order of availability)
    /// - Fallback: x264 software encoding
    pub fn new(
        context: ObsContext,
        output_path: PathBuf,
        config: &RecordingConfig,
    ) -> Result<Self> {
        info!(
            "Creating recording output: {:?} (codec: {:?}, bitrate: {} Kbps)",
            output_path, config.codec_preference, config.video_bitrate
        );

        let codec = match config.codec_preference {
            VideoCodecPreference::HevcPreferred => HardwareCodec::HEVC,
            VideoCodecPreference::H264Preferred => HardwareCodec::H264,
            VideoCodecPreference::Av1Preferred => HardwareCodec::AV1,
        };

        // Build the output with hardware encoder selection
        // Convert PathBuf to ObsPath
        let output_path_str = output_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid output path (non-UTF8): {:?}", output_path))?;
        let obs_path = ObsPath::new(output_path_str);

        // Build the output with hardware encoder selection
        // Note: Audio encoder is always created (required by OBS outputs), but actual
        // audio capture is controlled at the source level via ScreenCaptureSource.
        // When config.enable_audio is false, no audio sources are added, so the
        // audio track will be silent.
        let output = SimpleOutputBuilder::new(context, "recording", obs_path)
            .video_bitrate(config.video_bitrate)
            .audio_bitrate(config.audio_bitrate)
            .hardware_encoder(codec, config.quality_preset)
            .format(config.format)
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to create recording output: {}", e))?;

        info!(
            "Recording output configured successfully (audio capture: {})",
            if config.enable_audio {
                "enabled"
            } else {
                "disabled (silent track)"
            }
        );
        debug!(
            "Using format: {:?}, quality preset: {:?}",
            config.format, config.quality_preset
        );

        Ok(Self {
            output,
            state: RecordingState::Stopped,
            output_path,
        })
    }

    /// Create a new recording output with default configuration (HEVC preferred)
    pub fn new_default(context: ObsContext, output_path: PathBuf) -> Result<Self> {
        Self::new(context, output_path, &RecordingConfig::default())
    }

    /// Start recording
    pub fn start(&mut self) -> Result<()> {
        if self.state == RecordingState::Recording {
            debug!("Recording already started");
            return Ok(());
        }

        info!("Starting recording to {:?}", self.output_path);
        self.output
            .start()
            .map_err(|e| anyhow::anyhow!("Failed to start recording: {}", e))?;

        self.state = RecordingState::Recording;
        Ok(())
    }

    /// Stop recording
    pub fn stop(&mut self) -> Result<PathBuf> {
        if self.state == RecordingState::Stopped {
            debug!("Recording already stopped");
            return Ok(self.output_path.clone());
        }

        info!("Stopping recording");
        self.output
            .stop()
            .map_err(|e| anyhow::anyhow!("Failed to stop recording: {}", e))?;

        self.state = RecordingState::Stopped;
        Ok(self.output_path.clone())
    }

    /// Get current recording state
    pub fn state(&self) -> RecordingState {
        self.state
    }

    /// Get the output file path
    pub fn output_path(&self) -> &PathBuf {
        &self.output_path
    }

    /// Check if recording is active
    pub fn is_recording(&self) -> bool {
        self.state == RecordingState::Recording
    }

    /// Check if the output is currently active (started successfully)
    pub fn is_active(&self) -> Result<bool> {
        self.output
            .is_active()
            .map_err(|e| anyhow::anyhow!("Failed to check output status: {}", e))
    }

    /// Pause recording
    pub fn pause(&mut self) -> Result<()> {
        if self.state != RecordingState::Recording {
            debug!("Cannot pause - not recording");
            return Ok(());
        }

        info!("Pausing recording");
        self.output
            .pause(true)
            .map_err(|e| anyhow::anyhow!("Failed to pause recording: {}", e))?;

        self.state = RecordingState::Paused;
        Ok(())
    }

    /// Resume recording
    pub fn resume(&mut self) -> Result<()> {
        if self.state != RecordingState::Paused {
            debug!("Cannot resume - not paused");
            return Ok(());
        }

        info!("Resuming recording");
        self.output
            .pause(false)
            .map_err(|e| anyhow::anyhow!("Failed to resume recording: {}", e))?;

        self.state = RecordingState::Recording;
        Ok(())
    }

    /// Check if recording is paused
    pub fn is_paused(&self) -> bool {
        self.state == RecordingState::Paused
    }
}

/// Builder for RecordingOutput with fluent API
pub struct RecordingOutputBuilder {
    context: ObsContext,
    output_path: PathBuf,
    config: RecordingConfig,
}

impl RecordingOutputBuilder {
    pub fn new(context: ObsContext, output_path: PathBuf) -> Self {
        Self {
            context,
            output_path,
            config: RecordingConfig::default(),
        }
    }

    /// Set video bitrate in Kbps
    pub fn video_bitrate(mut self, bitrate: u32) -> Self {
        self.config.video_bitrate = bitrate;
        self
    }

    /// Set audio bitrate in Kbps
    pub fn audio_bitrate(mut self, bitrate: u32) -> Self {
        self.config.audio_bitrate = bitrate;
        self
    }

    /// Prefer HEVC codec (default)
    pub fn prefer_hevc(mut self) -> Self {
        self.config.codec_preference = VideoCodecPreference::HevcPreferred;
        self
    }

    /// Prefer H.264 codec
    pub fn prefer_h264(mut self) -> Self {
        self.config.codec_preference = VideoCodecPreference::H264Preferred;
        self
    }

    /// Prefer AV1 codec
    pub fn prefer_av1(mut self) -> Self {
        self.config.codec_preference = VideoCodecPreference::Av1Preferred;
        self
    }

    /// Set quality preset
    pub fn quality_preset(mut self, preset: HardwarePreset) -> Self {
        self.config.quality_preset = preset;
        self
    }

    /// Set output format
    pub fn format(mut self, format: OutputFormat) -> Self {
        self.config.format = format;
        self
    }

    /// Use high quality preset
    pub fn high_quality(mut self) -> Self {
        self.config = RecordingConfig::high_quality();
        self
    }

    /// Use compact preset
    pub fn compact(mut self) -> Self {
        self.config = RecordingConfig::compact();
        self
    }

    /// Use compatible preset
    pub fn compatible(mut self) -> Self {
        self.config = RecordingConfig::compatible();
        self
    }

    /// Build the recording output
    pub fn build(self) -> Result<RecordingOutput> {
        RecordingOutput::new(self.context, self.output_path, &self.config)
    }
}
