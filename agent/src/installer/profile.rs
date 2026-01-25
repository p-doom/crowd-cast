//! OBS Profile configuration and hardware encoder selection

use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
#[allow(unused_imports)]
use tracing::{info, warn};

use super::obs_detector::OBSInstallation;

/// crowd-cast profile name
const PROFILE_NAME: &str = "crowd-cast";

/// crowd-cast scene collection name  
const SCENE_COLLECTION_NAME: &str = "crowd-cast Capture";

/// Hardware encoder types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardwareEncoder {
    /// NVIDIA NVENC
    Nvenc,
    /// AMD AMF
    Amf,
    /// Intel Quick Sync
    Qsv,
    /// Apple VideoToolbox
    VideoToolbox,
    /// VA-API (Linux)
    Vaapi,
    /// Software x264 fallback
    Software,
}

impl HardwareEncoder {
    /// Get the OBS encoder ID for this encoder
    pub fn obs_id(&self) -> &'static str {
        match self {
            HardwareEncoder::Nvenc => "jim_nvenc",
            HardwareEncoder::Amf => "h264_texture_amf",
            HardwareEncoder::Qsv => "obs_qsv11",
            HardwareEncoder::VideoToolbox => "com.apple.videotoolbox.videoencoder.ave.avc",
            HardwareEncoder::Vaapi => "ffmpeg_vaapi",
            HardwareEncoder::Software => "obs_x264",
        }
    }
    
    /// Get the HEVC variant if available
    pub fn hevc_id(&self) -> Option<&'static str> {
        match self {
            HardwareEncoder::Nvenc => Some("jim_hevc_nvenc"),
            HardwareEncoder::Amf => Some("h265_texture_amf"),
            HardwareEncoder::Qsv => Some("obs_qsv11_av1"), // QSV doesn't have great HEVC
            HardwareEncoder::VideoToolbox => Some("com.apple.videotoolbox.videoencoder.ave.hevc"),
            HardwareEncoder::Vaapi => Some("ffmpeg_vaapi"), // VAAPI handles codec internally
            HardwareEncoder::Software => None,
        }
    }
    
    /// Get display name
    pub fn display_name(&self) -> &'static str {
        match self {
            HardwareEncoder::Nvenc => "NVIDIA NVENC",
            HardwareEncoder::Amf => "AMD AMF",
            HardwareEncoder::Qsv => "Intel Quick Sync",
            HardwareEncoder::VideoToolbox => "Apple VideoToolbox",
            HardwareEncoder::Vaapi => "VA-API",
            HardwareEncoder::Software => "Software (x264)",
        }
    }
}

/// Detect the best available hardware encoder
pub fn detect_best_encoder() -> HardwareEncoder {
    #[cfg(target_os = "macos")]
    {
        // macOS always has VideoToolbox
        info!("Using Apple VideoToolbox encoder");
        HardwareEncoder::VideoToolbox
    }
    
    #[cfg(target_os = "windows")]
    {
        // Check for NVIDIA
        if has_nvidia_gpu() {
            info!("Detected NVIDIA GPU, using NVENC");
            HardwareEncoder::Nvenc
        }
        // Check for AMD
        else if has_amd_gpu() {
            info!("Detected AMD GPU, using AMF");
            HardwareEncoder::Amf
        }
        // Check for Intel
        else if has_intel_gpu() {
            info!("Detected Intel GPU, using Quick Sync");
            HardwareEncoder::Qsv
        } else {
            warn!("No hardware encoder detected, falling back to software");
            HardwareEncoder::Software
        }
    }
    
    #[cfg(target_os = "linux")]
    {
        // Check for NVIDIA (proprietary driver)
        if has_nvidia_gpu() {
            info!("Detected NVIDIA GPU, using NVENC");
            HardwareEncoder::Nvenc
        }
        // Check for VA-API (AMD/Intel on Linux)
        else if has_vaapi() {
            info!("Detected VA-API support");
            HardwareEncoder::Vaapi
        } else {
            warn!("No hardware encoder detected, falling back to software");
            HardwareEncoder::Software
        }
    }
}

/// Check for NVIDIA GPU
#[cfg(any(target_os = "windows", target_os = "linux"))]
fn has_nvidia_gpu() -> bool {
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        Command::new("nvidia-smi")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        // Check for nvidia-smi or nvidia kernel module
        Command::new("nvidia-smi")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
            || std::path::Path::new("/dev/nvidia0").exists()
    }
}

/// Check for AMD GPU
#[cfg(target_os = "windows")]
fn has_amd_gpu() -> bool {
    // Check for AMD driver DLLs
    let system32 = std::env::var("SystemRoot")
        .map(|r| format!(r"{}\System32", r))
        .unwrap_or_else(|_| r"C:\Windows\System32".to_string());
    
    std::path::Path::new(&format!(r"{}\amfrt64.dll", system32)).exists()
}

/// Check for Intel GPU (Quick Sync)
#[cfg(target_os = "windows")]
fn has_intel_gpu() -> bool {
    use std::process::Command;
    
    // Try to detect Intel GPU via WMIC
    Command::new("wmic")
        .args(["path", "win32_VideoController", "get", "name"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase().contains("intel"))
        .unwrap_or(false)
}

/// Check for VA-API support on Linux
#[cfg(target_os = "linux")]
fn has_vaapi() -> bool {
    use std::process::Command;
    
    // Check for vainfo command
    Command::new("vainfo")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
        || std::path::Path::new("/dev/dri/renderD128").exists()
}

/// Create the crowd-cast profile in OBS
pub fn create_profile(obs: &OBSInstallation, encoder: HardwareEncoder) -> Result<PathBuf> {
    let profile_dir = obs.data_dir.join("basic").join("profiles").join(PROFILE_NAME);
    
    fs::create_dir_all(&profile_dir)
        .with_context(|| format!("Failed to create profile directory: {:?}", profile_dir))?;
    
    // Create basic.ini
    let basic_ini = generate_basic_ini(encoder);
    fs::write(profile_dir.join("basic.ini"), basic_ini)?;
    
    // Create recordEncoder.json if using advanced output mode
    let encoder_json = generate_encoder_json(encoder);
    fs::write(profile_dir.join("recordEncoder.json"), encoder_json)?;
    
    info!("Created OBS profile '{}' with {} encoder", PROFILE_NAME, encoder.display_name());
    
    Ok(profile_dir)
}

/// Generate basic.ini content
fn generate_basic_ini(encoder: HardwareEncoder) -> String {
    let encoder_id = encoder.hevc_id().unwrap_or_else(|| encoder.obs_id());
    
    format!(r#"[General]
Name={profile_name}

[Video]
BaseCX=1920
BaseCY=1080
OutputCX=1920
OutputCY=1080
FPSType=1
FPSCommon=30
FPSInt=30
FPSNum=30
FPSDen=1

[Audio]
SampleRate=48000
ChannelSetup=Stereo
DesktopAudioDevice1=default
DesktopAudioDevice2=disabled
AuxAudioDevice1=default
AuxAudioDevice2=disabled
AuxAudioDevice3=disabled
AuxAudioDevice4=disabled

[Output]
Mode=Advanced
FilePath=
RecType=Standard
RecFormat=mkv
RecTracks=1
RecEncoder={encoder_id}
RecRB=true
RecRBTime=30
RecSplitFile=true
RecSplitFileType=Time
RecSplitFileTime=300

[AdvOut]
RecEncoder={encoder_id}
RecMuxerCustom=
RecRB=true
RecRBTime=30
RecRBSize=512
TrackIndex=1
VodTrackIndex=2
Track1Bitrate=160
Track1Name=
Track2Bitrate=160
Track2Name=
Track3Bitrate=160
Track3Name=
Track4Bitrate=160
Track4Name=
Track5Bitrate=160
Track5Name=
Track6Bitrate=160
Track6Name=
RecType=Standard
RecFormat=mkv
RecTracks=1
FLVTrack=1
RecSplitFile=true
RecSplitFileTime=300
RecSplitFileType=Time
FFOutputToFile=true
FFFilePath=
FFExtension=mp4
FFVBitrate=2500
FFVGOPSize=250
FFUseRescale=false
FFIgnoreCompat=false
FFRescaleRes=1280x720
FFABitrate=160
FFTrack1Bitrate=160
FFTrack2Bitrate=160
FFTrack3Bitrate=160
FFTrack4Bitrate=160
FFTrack5Bitrate=160
FFTrack6Bitrate=160
FFMuxerCustom=
Encoder=obs_x264
FFVEncoder=
FFVEncoderId=0
FFAEncoder=
FFAEncoderId=0
"#, 
        profile_name = PROFILE_NAME,
        encoder_id = encoder_id
    )
}

/// Generate encoder JSON settings
fn generate_encoder_json(encoder: HardwareEncoder) -> String {
    match encoder {
        HardwareEncoder::Nvenc => r#"{
    "bitrate": 6000,
    "cqp": 20,
    "keyint_sec": 2,
    "preset": "p5",
    "profile": "high",
    "psycho_aq": true,
    "rate_control": "VBR"
}"#.to_string(),
        
        HardwareEncoder::VideoToolbox => r#"{
    "bitrate": 6000,
    "keyint_sec": 2,
    "profile": "high",
    "rate_control": "ABR"
}"#.to_string(),
        
        HardwareEncoder::Amf => r#"{
    "bitrate": 6000,
    "cqp": 20,
    "preset": "quality",
    "profile": "high",
    "rate_control": "VBR"
}"#.to_string(),
        
        HardwareEncoder::Qsv => r#"{
    "bitrate": 6000,
    "keyint_sec": 2,
    "profile": "high",
    "rate_control": "VBR",
    "target_usage": "balanced"
}"#.to_string(),
        
        HardwareEncoder::Vaapi => r#"{
    "bitrate": 6000,
    "keyint_sec": 2,
    "profile": "high",
    "rate_control": "VBR"
}"#.to_string(),
        
        HardwareEncoder::Software => r#"{
    "bitrate": 4000,
    "crf": 23,
    "keyint_sec": 2,
    "preset": "veryfast",
    "profile": "high",
    "rate_control": "CRF",
    "tune": "zerolatency"
}"#.to_string(),
    }
}

/// Create a basic scene collection for crowd-cast
pub fn create_scene_collection(obs: &OBSInstallation) -> Result<PathBuf> {
    let scenes_dir = obs.data_dir.join("basic").join("scenes");
    fs::create_dir_all(&scenes_dir)?;
    
    let scene_file = scenes_dir.join(format!("{}.json", SCENE_COLLECTION_NAME));
    
    let scene_json = generate_scene_collection();
    fs::write(&scene_file, scene_json)?;
    
    info!("Created scene collection '{}'", SCENE_COLLECTION_NAME);
    
    Ok(scene_file)
}

/// Generate a basic scene collection JSON
fn generate_scene_collection() -> String {
    r#"{
    "current_program_scene": "crowd-cast Capture",
    "current_scene": "crowd-cast Capture",
    "name": "crowd-cast Capture",
    "scene_order": [
        {"name": "crowd-cast Capture"}
    ],
    "sources": [],
    "transitions": [],
    "current_transition": "Fade",
    "transition_duration": 300,
    "groups": [],
    "quick_transitions": []
}"#.to_string()
}

/// Check if the crowd-cast profile exists
pub fn profile_exists(obs: &OBSInstallation) -> bool {
    obs.data_dir
        .join("basic")
        .join("profiles")
        .join(PROFILE_NAME)
        .join("basic.ini")
        .exists()
}

/// Get profile name constant
pub fn get_profile_name() -> &'static str {
    PROFILE_NAME
}

/// Get scene collection name constant
pub fn get_scene_collection_name() -> &'static str {
    SCENE_COLLECTION_NAME
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_detect_encoder() {
        let encoder = detect_best_encoder();
        println!("Detected encoder: {:?} ({})", encoder, encoder.display_name());
        println!("OBS ID: {}", encoder.obs_id());
    }
    
    #[test]
    fn test_generate_basic_ini() {
        let ini = generate_basic_ini(HardwareEncoder::Software);
        assert!(ini.contains("[Video]"));
        assert!(ini.contains("FPSCommon=30"));
    }
}
