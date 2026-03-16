use std::{
    fs, io,
    path::{Path, PathBuf},
};

use tracing::{debug, warn};
use v4l::Device;
use v4l::video::Capture;

use crate::manager::CameraManager;

pub const LOG_LEVEL_ENV: &str = "YV_STREAMER_SOFTWARE_LOG_LEVEL";

pub fn resolve_log_filter(log_level: Option<String>, rust_log: Option<String>) -> String {
    log_level
        .or(rust_log)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "info".to_string())
}

pub fn should_emit_debug_boot_report(filter: &str) -> bool {
    let filter = filter.to_ascii_lowercase();
    filter.contains("debug") || filter.contains("trace")
}

pub fn detect_video_devices() -> io::Result<Vec<PathBuf>> {
    detect_video_devices_in_dir(Path::new("/dev"))
}

pub fn detect_video_devices_in_dir(directory: &Path) -> io::Result<Vec<PathBuf>> {
    let mut devices = fs::read_dir(directory)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .map(|name| name.starts_with("video"))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();

    devices.sort();

    Ok(devices)
}

pub fn log_debug_boot_report(manager: &CameraManager, host: &str, port: u16) {
    debug!("Initialization step 1/4: resolved logging configuration");
    debug!("Initialization step 2/4: resolved bind target to {}:{}", host, port);

    debug!("Initialization step 3/4: scanning V4L2 devices under /dev");

    match detect_video_devices() {
        Ok(devices) if devices.is_empty() => {
            debug!("No /dev/video* nodes detected during startup");
        }
        Ok(devices) => {
            debug!("Detected {} video device node(s): {:?}", devices.len(), devices);

            for device in devices {
                log_video_device_details(&device);
            }
        }
        Err(error) => {
            warn!("Failed to enumerate /dev/video* nodes during startup: {}", error);
        }
    }

    let active_cameras = manager.active_camera_ids();

    debug!("Initialization step 4/4: inspecting managed camera workers");

    if active_cameras.is_empty() {
        debug!("No managed camera workers are active yet; workers are created lazily on first request");
    } else {
        debug!(
            "Managed camera workers already active at startup: {:?}",
            active_cameras
        );
    }
}

fn log_video_device_details(path: &Path) {
    let path_display = path.display().to_string();

    match Device::with_path(path) {
        Ok(device) => {
            let format_summary = device
                .format()
                .map(|format| {
                    format!(
                        "{}x{} {}",
                        format.width,
                        format.height,
                        format.fourcc.str().unwrap_or("unknown")
                    )
                })
                .unwrap_or_else(|error| format!("format unavailable: {error}"));

            let caps_summary = device
                .query_caps()
                .map(|caps| format!("{} ({})", caps.card, caps.driver))
                .unwrap_or_else(|error| format!("capabilities unavailable: {error}"));

            debug!(
                "Detected video device {} => {} [{}]",
                path_display, caps_summary, format_summary
            );
        }
        Err(error) => {
            warn!("Detected video device {} but failed to open it: {}", path_display, error);
        }
    }
}
