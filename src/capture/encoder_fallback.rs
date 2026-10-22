//! Windows: fall back to another video encoder when the first one fails to start
//! (#175, #159).
//!
//! The wrapper picks the first *listed* hardware encoder, but listed is not the same as
//! working: on some Intel laptops QSV HEVC is listed and then rejected by the driver at
//! start (`MFX_ERR_UNSUPPORTED`), so recording never started. When `obs_output_start`
//! fails we rebuild the output with the next option, in this order:
//!   1. today's pick, with today's settings (so working machines behave exactly as before);
//!   2. if that pick is QSV: the same encoder with lookahead off (`latency = "ultra-low"`,
//!      `bframes = 0`);
//!   3. the same vendor's H.264 hardware encoder, default settings;
//!   4. x264 `veryfast`, CBR at the configured bitrate.
//! Options whose encoder is not listed are skipped. The option that works is kept for the
//! rest of the process, and options that failed before it in the same attempt are skipped
//! from then on. When every option fails nothing is remembered and the caller sees the same
//! error as before.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use anyhow::{Context, Result};
use libobs_simple::output::simple::{hardware_encoder_candidates, HardwareCodec};
use libobs_wrapper::context::ObsContext;
use libobs_wrapper::encoders::{ObsContextEncoders, ObsVideoEncoderType};
use libobs_wrapper::utils::ObsString;
use tracing::{error, info, warn};

use super::obs_log;
use super::recording::{RecordingConfig, RecordingOutput, VideoCodecPreference};

pub(super) const X264_ID: &str = "obs_x264";
const QSV_H264_ID: &str = "obs_qsv11_v2";
/// Upper bound on encoder options tried per recording start.
const MAX_ATTEMPTS: usize = 4;

/// One way to configure the recording's video encoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum EncoderOption {
    /// Today's builder path, untouched. `id` is what that path selects.
    Default { id: String },
    /// A QSV encoder with lookahead and B-frames off.
    QsvLookaheadOff { id: String },
    /// A hardware H.264 encoder with the builder's default settings.
    VendorH264 { id: String },
    /// x264 `veryfast`, CBR at the configured bitrate.
    X264VeryFast,
}

impl EncoderOption {
    /// Stable identity for the sticky/blame bookkeeping.
    pub(super) fn key(&self) -> String {
        match self {
            EncoderOption::Default { id } => format!("default:{}", id),
            EncoderOption::QsvLookaheadOff { id } => format!("qsv_lookahead_off:{}", id),
            EncoderOption::VendorH264 { id } => format!("h264:{}", id),
            EncoderOption::X264VeryFast => "x264_veryfast".to_string(),
        }
    }

    // Only the debug-build failure injection reads this outside tests.
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    pub(super) fn encoder_id(&self) -> &str {
        match self {
            EncoderOption::Default { id }
            | EncoderOption::QsvLookaheadOff { id }
            | EncoderOption::VendorH264 { id } => id,
            EncoderOption::X264VeryFast => X264_ID,
        }
    }

    fn describe(&self) -> String {
        match self {
            EncoderOption::Default { id } => format!("{} (default settings)", id),
            EncoderOption::QsvLookaheadOff { id } => {
                format!("{} (latency=ultra-low, bframes=0)", id)
            }
            EncoderOption::VendorH264 { id } => format!("{} (H.264 fallback)", id),
            EncoderOption::X264VeryFast => format!("{} (veryfast, CBR)", X264_ID),
        }
    }
}

/// `recording.video_encoder` from config.toml.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VideoEncoderOverride {
    Auto,
    X264,
    QsvH264,
}

/// Parse `recording.video_encoder`. Missing, "auto" and unknown values are `Auto`; the bool
/// is true for an unknown (non-empty) value so the caller can warn about it.
pub(super) fn parse_video_encoder_override(raw: Option<&str>) -> (VideoEncoderOverride, bool) {
    let Some(raw) = raw else {
        return (VideoEncoderOverride::Auto, false);
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => (VideoEncoderOverride::Auto, false),
        "x264" => (VideoEncoderOverride::X264, false),
        "qsv_h264" => (VideoEncoderOverride::QsvH264, false),
        _ => (VideoEncoderOverride::Auto, true),
    }
}

/// Today's pick: the first listed candidate, else x264 (mirrors the wrapper's selection).
pub(super) fn default_pick(candidates: &[String], available: &[String]) -> String {
    candidates
        .iter()
        .find(|c| available.contains(c))
        .cloned()
        .unwrap_or_else(|| X264_ID.to_string())
}

fn is_qsv(id: &str) -> bool {
    id.starts_with("obs_qsv11")
}

/// The same vendor's H.264 encoder for a hardware encoder id, if it has one.
fn vendor_h264(id: &str) -> Option<&'static str> {
    match id {
        "obs_qsv11_hevc" | "obs_qsv11_av1" | "obs_qsv11_v2" | "obs_qsv11" => Some(QSV_H264_ID),
        "obs_qsv11_hevc_soft" | "obs_qsv11_av1_soft" | "obs_qsv11_soft_v2" | "obs_qsv11_soft" => {
            Some("obs_qsv11_soft_v2")
        }
        "obs_nvenc_hevc_tex" | "obs_nvenc_av1_tex" | "obs_nvenc_h264_tex" => {
            Some("obs_nvenc_h264_tex")
        }
        "obs_nvenc_hevc_soft" | "obs_nvenc_av1_soft" | "obs_nvenc_h264_soft" => {
            Some("obs_nvenc_h264_soft")
        }
        "jim_hevc_nvenc" | "jim_av1_nvenc" | "jim_nvenc" => Some("jim_nvenc"),
        "h265_texture_amf" | "av1_texture_amf" | "h264_texture_amf" => Some("h264_texture_amf"),
        _ => None,
    }
}

/// Build the ordered option chain for this machine (before sticky/blame state is applied).
pub(super) fn build_chain(
    default_id: &str,
    available: &[String],
    override_: VideoEncoderOverride,
) -> Vec<EncoderOption> {
    let listed = |id: &str| available.iter().any(|a| a == id);
    let mut chain = Vec::new();

    match override_ {
        VideoEncoderOverride::Auto => {}
        VideoEncoderOverride::X264 => chain.push(EncoderOption::X264VeryFast),
        VideoEncoderOverride::QsvH264 => {
            if listed(QSV_H264_ID) {
                chain.push(EncoderOption::VendorH264 {
                    id: QSV_H264_ID.to_string(),
                });
            }
        }
    }

    chain.push(EncoderOption::Default {
        id: default_id.to_string(),
    });
    if is_qsv(default_id) {
        chain.push(EncoderOption::QsvLookaheadOff {
            id: default_id.to_string(),
        });
    }
    if let Some(h264) = vendor_h264(default_id) {
        // Same id as today's pick means the same settings as option 1: nothing new to try.
        if h264 != default_id && listed(h264) {
            chain.push(EncoderOption::VendorH264 {
                id: h264.to_string(),
            });
        }
    }
    chain.push(EncoderOption::X264VeryFast);

    let mut seen = HashSet::new();
    chain.retain(|o| seen.insert(o.key()));
    chain
}

/// What earlier starts in this process taught us.
#[derive(Debug, Default)]
pub(super) struct FallbackState {
    /// The option that last started successfully; tried first.
    sticky: Option<String>,
    /// Options that failed in an attempt where a later option then worked.
    bad: HashSet<String>,
}

impl FallbackState {
    /// Apply what we learned: the sticky option first, known-bad options dropped, capped.
    pub(super) fn order(&self, chain: Vec<EncoderOption>) -> Vec<EncoderOption> {
        let mut ordered: Vec<EncoderOption> = chain
            .iter()
            .filter(|o| !self.bad.contains(&o.key()))
            .cloned()
            .collect();
        if ordered.is_empty() {
            // Only possible if the encoder list changed under us; start from scratch.
            ordered = chain;
        }
        if let Some(sticky) = &self.sticky {
            if let Some(pos) = ordered.iter().position(|o| &o.key() == sticky) {
                let o = ordered.remove(pos);
                ordered.insert(0, o);
            }
        }
        ordered.truncate(MAX_ATTEMPTS);
        ordered
    }

    /// Blame rule: options are only marked bad when a later option in the same attempt
    /// worked. A failure that hits every option (disk, path) never reaches here.
    pub(super) fn record_success(&mut self, failed_before: &[String], winner: &str) {
        for key in failed_before {
            if key != winner {
                self.bad.insert(key.clone());
            }
        }
        self.bad.remove(winner);
        self.sticky = Some(winner.to_string());
    }

    fn bad_keys(&self) -> Vec<String> {
        let mut v: Vec<String> = self.bad.iter().cloned().collect();
        v.sort();
        v
    }
}

static STATE: OnceLock<Mutex<FallbackState>> = OnceLock::new();
static OVERRIDE: OnceLock<VideoEncoderOverride> = OnceLock::new();

fn state() -> std::sync::MutexGuard<'static, FallbackState> {
    STATE
        .get_or_init(|| Mutex::new(FallbackState::default()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// Called once at startup with `recording.video_encoder` from config.toml.
pub fn set_video_encoder_override(raw: Option<&str>) {
    let (parsed, unknown) = parse_video_encoder_override(raw);
    if unknown {
        warn!(
            "Unknown recording.video_encoder {:?} in config.toml; using \"auto\" (valid: auto, x264, qsv_h264)",
            raw.unwrap_or_default()
        );
    } else if parsed != VideoEncoderOverride::Auto {
        info!("recording.video_encoder override: {:?}", parsed);
    }
    let _ = OVERRIDE.set(parsed);
}

fn hardware_codec(pref: VideoCodecPreference) -> HardwareCodec {
    match pref {
        VideoCodecPreference::HevcPreferred => HardwareCodec::HEVC,
        VideoCodecPreference::H264Preferred => HardwareCodec::H264,
        VideoCodecPreference::Av1Preferred => HardwareCodec::AV1,
    }
}

fn type_id(t: ObsVideoEncoderType) -> String {
    ObsString::from(t).to_string()
}

/// Debug builds only: `CROWD_CAST_DEBUG_FAIL_ENCODERS=obs_qsv11_hevc,...` makes options with
/// those encoder ids (or option keys, e.g. `default:obs_qsv11_hevc`) fail to start, so the
/// fallback path can be exercised on any machine. Compiled out of release builds.
#[cfg(debug_assertions)]
fn debug_forced_failure(option: &EncoderOption) -> bool {
    let Ok(list) = std::env::var("CROWD_CAST_DEBUG_FAIL_ENCODERS") else {
        return false;
    };
    let key = option.key();
    list.split(',')
        .map(str::trim)
        .any(|x| !x.is_empty() && (x == option.encoder_id() || x == key))
}

/// Build and start one option. Errors carry the same context as the pre-fallback path
/// ("Failed to create recording output" / "Failed to start recording"). A built output
/// that fails to start is released, not left in the OBS context.
fn attempt(
    context: &ObsContext,
    output_path: &Path,
    config: &RecordingConfig,
    option: &EncoderOption,
) -> Result<RecordingOutput> {
    let _window = obs_log::encoder_start_window();
    let mut recording =
        RecordingOutput::new_for_encoder_option(context.clone(), output_path.to_path_buf(), config, option)
            .context("Failed to create recording output")?;

    #[cfg(debug_assertions)]
    if debug_forced_failure(option) {
        recording.release_unstarted(context);
        return Err(anyhow::anyhow!(
            "Failed to start recording: simulated by CROWD_CAST_DEBUG_FAIL_ENCODERS"
        ))
        .context("Failed to start recording");
    }

    match recording.start() {
        Ok(()) => Ok(recording),
        Err(e) => {
            recording.release_unstarted(context);
            Err(e).context("Failed to start recording")
        }
    }
}

/// Windows replacement for `RecordingOutput::new` + `start`, with the encoder fallback chain.
pub(super) fn start_recording_output(
    context: &ObsContext,
    output_path: &Path,
    config: &RecordingConfig,
) -> Result<RecordingOutput> {
    let available: Vec<String> = match context.available_video_encoders() {
        Ok(list) => list
            .into_iter()
            .map(|b| type_id(b.get_encoder_id().clone()))
            .collect(),
        Err(e) => {
            // Cannot plan a chain; do exactly what we did before.
            warn!("Could not list video encoders ({}); starting without fallback", e);
            let mut recording =
                RecordingOutput::new(context.clone(), output_path.to_path_buf(), config)
                    .context("Failed to create recording output")?;
            recording.start().context("Failed to start recording")?;
            return Ok(recording);
        }
    };

    let candidates: Vec<String> = hardware_encoder_candidates(hardware_codec(config.codec_preference))
        .into_iter()
        .map(type_id)
        .collect();
    let default_id = default_pick(&candidates, &available);
    let override_ = OVERRIDE.get().copied().unwrap_or(VideoEncoderOverride::Auto);
    let chain = build_chain(&default_id, &available, override_);
    let (ordered, skipped) = {
        let st = state();
        (st.order(chain), st.bad_keys())
    };

    let started = Instant::now();
    let total = ordered.len();
    let mut failures: Vec<(EncoderOption, anyhow::Error)> = Vec::new();

    for option in ordered {
        match attempt(context, output_path, config, &option) {
            Ok(recording) => {
                let failed_keys: Vec<String> = failures.iter().map(|(o, _)| o.key()).collect();
                state().record_success(&failed_keys, &option.key());
                let elapsed_ms = started.elapsed().as_millis();
                if failures.is_empty() && skipped.is_empty() {
                    info!(
                        "Video encoder started: {} in {} ms",
                        option.describe(),
                        elapsed_ms
                    );
                } else if failures.is_empty() {
                    info!(
                        "Video encoder started: {} in {} ms (kept from an earlier fallback; skipping {:?})",
                        option.describe(),
                        elapsed_ms,
                        skipped
                    );
                } else {
                    let why: Vec<String> = failures
                        .iter()
                        .map(|(o, e)| format!("{}: {:#}", o.describe(), e))
                        .collect();
                    warn!(
                        "Video encoder started: {} after {} failed option(s) in {} ms; fell back from [{}]",
                        option.describe(),
                        failures.len(),
                        elapsed_ms,
                        why.join("; ")
                    );
                }
                return Ok(recording);
            }
            Err(e) => {
                warn!(
                    "Video encoder {} failed to start: {:#}",
                    option.describe(),
                    e
                );
                failures.push((option, e));
            }
        }
    }

    error!(
        "Recording could not start with any of {} video encoder option(s) in {} ms",
        total,
        started.elapsed().as_millis()
    );
    // Same error as the pre-fallback path: the first option's.
    match failures.into_iter().next() {
        Some((_, e)) => Err(e),
        None => Err(anyhow::anyhow!("Failed to start recording")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn hevc_candidates() -> Vec<String> {
        hardware_encoder_candidates(HardwareCodec::HEVC)
            .into_iter()
            .map(type_id)
            .collect()
    }

    fn keys(chain: &[EncoderOption]) -> Vec<String> {
        chain.iter().map(|o| o.key()).collect()
    }

    #[test]
    fn qsv_only_box() {
        let avail = ids(&["obs_qsv11_hevc", "obs_qsv11_v2", "obs_qsv11_av1", "obs_x264"]);
        let d = default_pick(&hevc_candidates(), &avail);
        assert_eq!(d, "obs_qsv11_hevc");
        let chain = build_chain(&d, &avail, VideoEncoderOverride::Auto);
        assert_eq!(
            keys(&chain),
            ids(&[
                "default:obs_qsv11_hevc",
                "qsv_lookahead_off:obs_qsv11_hevc",
                "h264:obs_qsv11_v2",
                "x264_veryfast"
            ])
        );
    }

    #[test]
    fn nvenc_box() {
        let avail = ids(&["obs_nvenc_hevc_tex", "obs_nvenc_h264_tex", "obs_x264"]);
        let d = default_pick(&hevc_candidates(), &avail);
        assert_eq!(d, "obs_nvenc_hevc_tex");
        let chain = build_chain(&d, &avail, VideoEncoderOverride::Auto);
        assert_eq!(
            keys(&chain),
            ids(&["default:obs_nvenc_hevc_tex", "h264:obs_nvenc_h264_tex", "x264_veryfast"])
        );
    }

    #[test]
    fn amf_box() {
        let avail = ids(&["h265_texture_amf", "h264_texture_amf", "obs_x264"]);
        let d = default_pick(&hevc_candidates(), &avail);
        assert_eq!(d, "h265_texture_amf");
        let chain = build_chain(&d, &avail, VideoEncoderOverride::Auto);
        assert_eq!(
            keys(&chain),
            ids(&["default:h265_texture_amf", "h264:h264_texture_amf", "x264_veryfast"])
        );
    }

    #[test]
    fn hybrid_box_uses_the_default_vendor_only() {
        // NVIDIA dGPU + Intel iGPU: today's pick is NVENC; QSV is not part of its chain.
        let avail = ids(&[
            "obs_nvenc_hevc_tex",
            "obs_nvenc_h264_tex",
            "obs_qsv11_hevc",
            "obs_qsv11_v2",
            "obs_x264",
        ]);
        let d = default_pick(&hevc_candidates(), &avail);
        assert_eq!(d, "obs_nvenc_hevc_tex");
        let chain = build_chain(&d, &avail, VideoEncoderOverride::Auto);
        assert_eq!(
            keys(&chain),
            ids(&["default:obs_nvenc_hevc_tex", "h264:obs_nvenc_h264_tex", "x264_veryfast"])
        );
    }

    #[test]
    fn no_hardware_box() {
        let avail = ids(&["obs_x264", "ffmpeg_openh264"]);
        let d = default_pick(&hevc_candidates(), &avail);
        assert_eq!(d, "obs_x264");
        let chain = build_chain(&d, &avail, VideoEncoderOverride::Auto);
        assert_eq!(keys(&chain), ids(&["default:obs_x264", "x264_veryfast"]));
    }

    #[test]
    fn unlisted_vendor_h264_is_skipped() {
        let avail = ids(&["obs_qsv11_hevc", "obs_x264"]);
        let chain = build_chain("obs_qsv11_hevc", &avail, VideoEncoderOverride::Auto);
        assert_eq!(
            keys(&chain),
            ids(&[
                "default:obs_qsv11_hevc",
                "qsv_lookahead_off:obs_qsv11_hevc",
                "x264_veryfast"
            ])
        );
    }

    #[test]
    fn h264_default_does_not_repeat_itself() {
        let avail = ids(&["obs_qsv11_v2", "obs_x264"]);
        let chain = build_chain("obs_qsv11_v2", &avail, VideoEncoderOverride::Auto);
        assert_eq!(
            keys(&chain),
            ids(&["default:obs_qsv11_v2", "qsv_lookahead_off:obs_qsv11_v2", "x264_veryfast"])
        );
    }

    #[test]
    fn override_goes_first_then_the_usual_chain() {
        let avail = ids(&["obs_qsv11_hevc", "obs_qsv11_v2", "obs_x264"]);
        let x = build_chain("obs_qsv11_hevc", &avail, VideoEncoderOverride::X264);
        assert_eq!(x[0], EncoderOption::X264VeryFast);
        assert_eq!(x.len(), 4, "x264 is not repeated at the end");
        let q = build_chain("obs_qsv11_hevc", &avail, VideoEncoderOverride::QsvH264);
        assert_eq!(
            keys(&q),
            ids(&[
                "h264:obs_qsv11_v2",
                "default:obs_qsv11_hevc",
                "qsv_lookahead_off:obs_qsv11_hevc",
                "x264_veryfast"
            ])
        );
        // qsv_h264 on a machine without QSV is ignored.
        let avail = ids(&["obs_nvenc_hevc_tex", "obs_x264"]);
        let q = build_chain("obs_nvenc_hevc_tex", &avail, VideoEncoderOverride::QsvH264);
        assert_eq!(keys(&q), ids(&["default:obs_nvenc_hevc_tex", "x264_veryfast"]));
    }

    #[test]
    fn chain_is_capped() {
        let avail = ids(&["obs_qsv11_hevc", "obs_qsv11_v2", "obs_x264"]);
        let chain = build_chain("obs_qsv11_hevc", &avail, VideoEncoderOverride::QsvH264);
        assert!(chain.len() > MAX_ATTEMPTS - 1);
        let st = FallbackState::default();
        assert!(st.order(chain).len() <= MAX_ATTEMPTS);
    }

    #[test]
    fn fresh_state_keeps_chain_order() {
        let avail = ids(&["obs_qsv11_hevc", "obs_qsv11_v2", "obs_x264"]);
        let chain = build_chain("obs_qsv11_hevc", &avail, VideoEncoderOverride::Auto);
        let st = FallbackState::default();
        assert_eq!(st.order(chain.clone()), chain);
    }

    #[test]
    fn blame_rule_marks_only_options_before_a_later_success() {
        let avail = ids(&["obs_qsv11_hevc", "obs_qsv11_v2", "obs_x264"]);
        let chain = build_chain("obs_qsv11_hevc", &avail, VideoEncoderOverride::Auto);
        let mut st = FallbackState::default();

        // Options 1 and 2 failed, option 3 worked.
        st.record_success(
            &ids(&["default:obs_qsv11_hevc", "qsv_lookahead_off:obs_qsv11_hevc"]),
            "h264:obs_qsv11_v2",
        );
        assert_eq!(
            keys(&st.order(chain.clone())),
            ids(&["h264:obs_qsv11_v2", "x264_veryfast"]),
            "segment rotations go straight to the working option"
        );

        // Later the sticky one fails and x264 works: it is blamed, x264 becomes sticky.
        st.record_success(&ids(&["h264:obs_qsv11_v2"]), "x264_veryfast");
        assert_eq!(keys(&st.order(chain.clone())), ids(&["x264_veryfast"]));
    }

    #[test]
    fn total_failure_changes_nothing() {
        // A failure that hits every option never calls record_success, so the state stays
        // fresh and the next start tries today's pick first again.
        let avail = ids(&["obs_qsv11_hevc", "obs_qsv11_v2", "obs_x264"]);
        let chain = build_chain("obs_qsv11_hevc", &avail, VideoEncoderOverride::Auto);
        let st = FallbackState::default();
        assert_eq!(st.order(chain.clone())[0].key(), "default:obs_qsv11_hevc");
    }

    #[test]
    fn first_option_success_is_sticky_and_blames_nothing() {
        let avail = ids(&["obs_qsv11_hevc", "obs_qsv11_v2", "obs_x264"]);
        let chain = build_chain("obs_qsv11_hevc", &avail, VideoEncoderOverride::Auto);
        let mut st = FallbackState::default();
        st.record_success(&[], "default:obs_qsv11_hevc");
        assert_eq!(st.order(chain.clone()), chain);
        assert!(st.bad_keys().is_empty());
    }

    #[test]
    fn config_override_parsing() {
        use VideoEncoderOverride::*;
        assert_eq!(parse_video_encoder_override(None), (Auto, false));
        assert_eq!(parse_video_encoder_override(Some("auto")), (Auto, false));
        assert_eq!(parse_video_encoder_override(Some("")), (Auto, false));
        assert_eq!(parse_video_encoder_override(Some("x264")), (X264, false));
        assert_eq!(parse_video_encoder_override(Some(" X264 ")), (X264, false));
        assert_eq!(parse_video_encoder_override(Some("qsv_h264")), (QsvH264, false));
        assert_eq!(parse_video_encoder_override(Some("nvenc")), (Auto, true));
    }

    #[test]
    fn config_field_round_trips_and_defaults_to_none() {
        let cfg: crate::config::Config = toml::from_str("").unwrap();
        assert_eq!(cfg.recording.video_encoder, None);
        // Not written when unset, so existing config files are saved unchanged.
        let out = toml::to_string_pretty(&cfg).unwrap();
        assert!(!out.contains("video_encoder"));

        let cfg: crate::config::Config =
            toml::from_str("[recording]\nvideo_encoder = \"x264\"\n").unwrap();
        assert_eq!(cfg.recording.video_encoder.as_deref(), Some("x264"));
        // An unknown value still parses (and is treated as auto).
        let cfg: crate::config::Config =
            toml::from_str("[recording]\nvideo_encoder = \"bogus\"\n").unwrap();
        assert_eq!(
            parse_video_encoder_override(cfg.recording.video_encoder.as_deref()).0,
            VideoEncoderOverride::Auto
        );
    }
}
