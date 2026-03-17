use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, RwLock,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use http::HeaderMap;
use jpeg_encoder::{ColorType, Encoder};
use serde::Serialize;
use tokio::sync::watch;
use tracing::{debug, info, warn};
use v4l::{
    buffer::Type,
    io::mmap::Stream as MmapStream,
    io::traits::CaptureStream,
    prelude::*,
    video::Capture,
    video::capture::Parameters,
    FourCC,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CameraConfig {
    pub camera_id: String,
    pub node: String,
    pub width: u32,
    pub height: u32,
    pub framerate: u32,
    pub capture_encoding: String,
}

impl CameraConfig {
    #[cfg(test)]
    pub fn test(camera_id: &str) -> Self {
        Self {
            camera_id: camera_id.to_string(),
            node: "/dev/video0".to_string(),
            width: 640,
            height: 480,
            framerate: 30,
            capture_encoding: "YUYV".to_string(),
        }
    }

    pub fn from_request(
        camera_id: &str,
        query: &HashMap<String, String>,
        headers: &HeaderMap,
        existing: Option<&CameraConfig>,
    ) -> Result<Self, CameraManagerError> {
        let node = value_from_request(query, headers, "node", "x-node")
            .or_else(|| existing.map(|config| config.node.clone()))
            .ok_or(CameraManagerError::MissingBootstrapField("node"))?;

        let resolution = value_from_request(query, headers, "resolution", "x-resolution")
            .or_else(|| existing.map(|config| format!("{}x{}", config.width, config.height)))
            .ok_or(CameraManagerError::MissingBootstrapField("resolution"))?;

        let (width, height) = parse_resolution(&resolution)?;

        let framerate = value_from_request(query, headers, "framerate", "x-framerate")
            .or_else(|| existing.map(|config| config.framerate.to_string()))
            .ok_or(CameraManagerError::MissingBootstrapField("framerate"))?
            .parse::<f64>()
            .map(|v| v as u32)
            .map_err(|_| CameraManagerError::InvalidBootstrapField("framerate"))?;

        let capture_encoding = value_from_request(
            query,
            headers,
            "capture_encoding",
            "x-capture-encoding",
        )
        .or_else(|| existing.map(|config| config.capture_encoding.clone()))
        .ok_or(CameraManagerError::MissingBootstrapField("capture_encoding"))?
        .to_uppercase();

        Ok(Self {
            camera_id: camera_id.to_string(),
            node,
            width,
            height,
            framerate,
            capture_encoding,
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CameraStateSnapshot {
    pub camera_id: String,
    pub node: String,
    pub width: u32,
    pub height: u32,
    pub framerate: u32,
    pub capture_encoding: String,
    pub status: String,
    pub frames_captured: u64,
    pub last_frame_at_ms: Option<u128>,
    pub last_error: Option<String>,
}

#[derive(Clone, Default)]
pub struct CameraManager {
    workers: Arc<RwLock<HashMap<String, Arc<CameraWorker>>>>,
}

impl CameraManager {
    pub fn ensure_camera(
        &self,
        camera_id: &str,
        query: &HashMap<String, String>,
        headers: &HeaderMap,
    ) -> Result<Arc<CameraWorker>, CameraManagerError> {
        debug!(
            "Ensuring camera worker for {} (bootstrap_present={})",
            camera_id,
            has_bootstrap(query, headers)
        );

        let current = self
            .workers
            .read()
            .expect("camera worker read lock poisoned")
            .get(camera_id)
            .cloned();

        let config = CameraConfig::from_request(
            camera_id,
            query,
            headers,
            current.as_ref().map(|worker| worker.config()),
        )?;

        if let Some(worker) = current {
            if worker.config() == &config {
                debug!("Reusing existing worker for camera {}", camera_id);
                return Ok(worker);
            }

            info!(
                "Camera {} configuration changed, restarting worker with node={} resolution={}x{} fps={} encoding={}",
                camera_id,
                config.node,
                config.width,
                config.height,
                config.framerate,
                config.capture_encoding
            );
            worker.stop();
        }

        let worker = CameraWorker::spawn_live(config)?;

        self.workers
            .write()
            .expect("camera worker write lock poisoned")
            .insert(camera_id.to_string(), worker.clone());

        info!("Started worker for camera {}", camera_id);
        debug!("Managed camera workers: {:?}", self.active_camera_ids());

        Ok(worker)
    }

    pub fn get_existing(&self, camera_id: &str) -> Option<Arc<CameraWorker>> {
        self.workers
            .read()
            .expect("camera worker read lock poisoned")
            .get(camera_id)
            .cloned()
    }

    pub fn ensure_or_get_existing(
        &self,
        camera_id: &str,
        query: &HashMap<String, String>,
        headers: &HeaderMap,
    ) -> Result<Arc<CameraWorker>, CameraManagerError> {
        if self.get_existing(camera_id).is_some() || has_bootstrap(query, headers) {
            self.ensure_camera(camera_id, query, headers)
        } else {
            debug!(
                "Camera {} requested without bootstrap data and no active worker",
                camera_id
            );
            self.get_existing(camera_id)
                .ok_or(CameraManagerError::CameraNotFound(camera_id.to_string()))
        }
    }

    pub fn list_cameras(&self) -> Vec<CameraStateSnapshot> {
        self.workers
            .read()
            .expect("camera worker read lock poisoned")
            .values()
            .map(|worker| worker.snapshot())
            .collect()
    }

    pub fn active_camera_ids(&self) -> Vec<String> {
        let mut camera_ids = self
            .workers
            .read()
            .expect("camera worker read lock poisoned")
            .keys()
            .cloned()
            .collect::<Vec<_>>();

        camera_ids.sort();

        camera_ids
    }

    pub fn register_static_frame(&self, config: CameraConfig, frame: Vec<u8>) {
        let worker = CameraWorker::from_static_frame(config, frame);
        let camera_id = worker.config().camera_id.clone();
        self.workers
            .write()
            .expect("camera worker write lock poisoned")
            .insert(camera_id.clone(), Arc::new(worker));

        debug!(
            "Registered static frame for test camera {}; managed cameras: {:?}",
            camera_id,
            self.active_camera_ids()
        );
    }
}

#[derive(Debug)]
pub enum CameraManagerError {
    CameraNotFound(String),
    InvalidBootstrapField(&'static str),
    MissingBootstrapField(&'static str),
    UnsupportedCaptureEncoding(String),
    WorkerInitialization(String),
}

impl std::fmt::Display for CameraManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CameraNotFound(camera_id) => write!(f, "No camera worker found for {camera_id}"),
            Self::InvalidBootstrapField(field) => write!(f, "Invalid value for bootstrap field {field}"),
            Self::MissingBootstrapField(field) => write!(f, "Missing bootstrap field {field}"),
            Self::UnsupportedCaptureEncoding(encoding) => write!(f, "Unsupported capture encoding {encoding}"),
            Self::WorkerInitialization(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for CameraManagerError {}

#[derive(Debug)]
struct CameraWorkerState {
    status: String,
    last_error: Option<String>,
    last_frame_at_ms: Option<u128>,
}

pub struct CameraWorker {
    config: CameraConfig,
    latest_frame: watch::Sender<Bytes>,
    stop: Arc<AtomicBool>,
    frame_count: AtomicU64,
    state: Arc<RwLock<CameraWorkerState>>,
}

impl CameraWorker {
    fn spawn_live(config: CameraConfig) -> Result<Arc<Self>, CameraManagerError> {
        if !matches!(config.capture_encoding.as_str(), "YUYV" | "MJPG") {
            return Err(CameraManagerError::UnsupportedCaptureEncoding(
                config.capture_encoding.clone(),
            ));
        }

        debug!(
            "Spawning live worker for camera {} on {} at {}x{} {}fps ({})",
            config.camera_id,
            config.node,
            config.width,
            config.height,
            config.framerate,
            config.capture_encoding
        );

        let (latest_frame, _) = watch::channel(Bytes::new());
        let worker = Arc::new(Self {
            config,
            latest_frame,
            stop: Arc::new(AtomicBool::new(false)),
            frame_count: AtomicU64::new(0),
            state: Arc::new(RwLock::new(CameraWorkerState {
                status: "starting".to_string(),
                last_error: None,
                last_frame_at_ms: None,
            })),
        });

        let thread_worker = worker.clone();
        thread::Builder::new()
            .name(format!("capture-{}", thread_worker.config.camera_id))
            .spawn(move || {
                thread_worker.run_capture_loop();
            })
            .map_err(|error| CameraManagerError::WorkerInitialization(error.to_string()))?;

        Ok(worker)
    }

    fn from_static_frame(config: CameraConfig, frame: Vec<u8>) -> Self {
        let (latest_frame, _) = watch::channel(Bytes::from(frame));
        Self {
            config,
            latest_frame,
            stop: Arc::new(AtomicBool::new(false)),
            frame_count: AtomicU64::new(1),
            state: Arc::new(RwLock::new(CameraWorkerState {
                status: "ready".to_string(),
                last_error: None,
                last_frame_at_ms: Some(now_millis()),
            })),
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<Bytes> {
        self.latest_frame.subscribe()
    }

    pub fn current_frame(&self) -> Bytes {
        self.latest_frame.borrow().clone()
    }

    pub fn config(&self) -> &CameraConfig {
        &self.config
    }

    pub fn snapshot(&self) -> CameraStateSnapshot {
        let state = self.state.read().expect("camera state read lock poisoned");

        CameraStateSnapshot {
            camera_id: self.config.camera_id.clone(),
            node: self.config.node.clone(),
            width: self.config.width,
            height: self.config.height,
            framerate: self.config.framerate,
            capture_encoding: self.config.capture_encoding.clone(),
            status: state.status.clone(),
            frames_captured: self.frame_count.load(Ordering::Relaxed),
            last_frame_at_ms: state.last_frame_at_ms,
            last_error: state.last_error.clone(),
        }
    }

    pub fn stop(&self) {
        debug!("Stopping worker for camera {}", self.config.camera_id);
        self.stop.store(true, Ordering::Relaxed);
    }

    fn set_status(&self, status: &str) {
        if let Ok(mut state) = self.state.write() {
            state.status = status.to_string();
            state.last_error = None;
        }
    }

    fn set_error(&self, error: String) {
        if let Ok(mut state) = self.state.write() {
            state.status = "error".to_string();
            state.last_error = Some(error);
        }
    }

    fn publish_frame(&self, frame: Bytes) {
        let _ = self.latest_frame.send(frame);
        let frame_count = self.frame_count.fetch_add(1, Ordering::Relaxed) + 1;

        if let Ok(mut state) = self.state.write() {
            state.status = "ready".to_string();
            state.last_error = None;
            state.last_frame_at_ms = Some(now_millis());
        }

        if frame_count == 1 {
            info!(
                "Camera {} delivered its first JPEG frame",
                self.config.camera_id
            );
        } else if frame_count % 300 == 0 {
            debug!(
                "Camera {} has published {} JPEG frames",
                self.config.camera_id,
                frame_count
            );
        }
    }

    fn run_capture_loop(&self) {
        debug!(
            "Camera {} capture loop started for node {}",
            self.config.camera_id,
            self.config.node
        );

        while !self.stop.load(Ordering::Relaxed) {
            match self.capture_once() {
                Ok(()) => {}
                Err(error) => {
                    warn!(
                        "Camera {} capture iteration failed: {}",
                        self.config.camera_id, error
                    );
                    self.set_error(error);
                    thread::sleep(Duration::from_millis(500));
                }
            }
        }

        debug!("Camera {} capture loop stopped", self.config.camera_id);
    }

    fn capture_once(&self) -> Result<(), String> {
        self.set_status("opening");

        debug!(
            "Camera {} opening device {} with requested {}x{} {}fps ({})",
            self.config.camera_id,
            self.config.node,
            self.config.width,
            self.config.height,
            self.config.framerate,
            self.config.capture_encoding
        );

        let device = Device::with_path(&self.config.node)
            .map_err(|error| format!("Failed to open {}: {error}", self.config.node))?;

        let mut format = device
            .format()
            .map_err(|error| format!("Failed to read device format: {error}"))?;
        format.width = self.config.width;
        format.height = self.config.height;
        format.fourcc = encoding_to_fourcc(&self.config.capture_encoding)
            .ok_or_else(|| format!("Unsupported capture encoding {}", self.config.capture_encoding))?;
        let applied_format = device
            .set_format(&format)
            .map_err(|error| format!("Failed to set device format: {error}"))?;

        debug!(
            "Camera {} applied V4L2 format {}x{} ({})",
            self.config.camera_id,
            applied_format.width,
            applied_format.height,
            applied_format.fourcc.str().unwrap_or("unknown")
        );

        let params = Parameters::with_fps(self.config.framerate);
        let _ = device.set_params(&params);
        debug!(
            "Camera {} requested stream parameters at {}fps",
            self.config.camera_id,
            self.config.framerate
        );

        let mut stream =
            MmapStream::with_buffers(&device, Type::VideoCapture, 4).map_err(|error| error.to_string())?;

        self.set_status("streaming");
        debug!(
            "Camera {} entered streaming state using mmap buffers",
            self.config.camera_id
        );

        while !self.stop.load(Ordering::Relaxed) {
            let (frame, _) = stream.next().map_err(|error| error.to_string())?;
            let jpeg = encode_frame_to_jpeg(
                frame,
                self.config.width,
                self.config.height,
                &self.config.capture_encoding,
            )
            .map_err(|error| error.to_string())?;

            self.publish_frame(Bytes::from(jpeg));
        }

        Ok(())
    }
}

pub fn build_mjpeg_chunk(frame: &Bytes) -> Bytes {
    let mut chunk = Vec::with_capacity(frame.len() + 128);
    chunk.extend_from_slice(b"--frame\r\nContent-Type: image/jpeg\r\nContent-Length: ");
    chunk.extend_from_slice(frame.len().to_string().as_bytes());
    chunk.extend_from_slice(b"\r\n\r\n");
    chunk.extend_from_slice(frame);
    chunk.extend_from_slice(b"\r\n");
    Bytes::from(chunk)
}

fn encode_frame_to_jpeg(
    frame: &[u8],
    width: u32,
    height: u32,
    capture_encoding: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    match capture_encoding {
        "MJPG" => Ok(frame.to_vec()),
        "YUYV" => {
            let rgb = yuyv_to_rgb(frame);
            let mut output = Vec::new();
            let encoder = Encoder::new(&mut output, 80);
            encoder.encode(&rgb, width as u16, height as u16, ColorType::Rgb)?;
            Ok(output)
        }
        unsupported => Err(format!("Unsupported capture encoding {unsupported}").into()),
    }
}

fn yuyv_to_rgb(frame: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(frame.len() / 2 * 3);

    for chunk in frame.chunks_exact(4) {
        let y0 = chunk[0] as i32;
        let u  = chunk[1] as i32 - 128;
        let y1 = chunk[2] as i32;
        let v  = chunk[3] as i32 - 128;

        output.extend_from_slice(&yuv_to_rgb(y0, u, v));
        output.extend_from_slice(&yuv_to_rgb(y1, u, v));
    }

    output
}

pub fn yuv_to_rgb(y: i32, u: i32, v: i32) -> [u8; 3] {
    // Coefficients scaled by 2^16 (65536):
    //   1.402    * 65536 = 91881
    //   0.344136 * 65536 = 22554
    //   0.714136 * 65536 = 46802
    //   1.772    * 65536 = 116130
    let y_fixed = y << 16;
    let red   = (y_fixed + 91881 * v + 32768) >> 16;
    let green = (y_fixed - 22554 * u - 46802 * v + 32768) >> 16;
    let blue  = (y_fixed + 116130 * u + 32768) >> 16;

    [clamp_u8(red), clamp_u8(green), clamp_u8(blue)]
}

fn clamp_u8(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

fn encoding_to_fourcc(encoding: &str) -> Option<FourCC> {
    match encoding {
        "YUYV" => Some(FourCC::new(b"YUYV")),
        "MJPG" => Some(FourCC::new(b"MJPG")),
        _ => None,
    }
}

fn parse_resolution(value: &str) -> Result<(u32, u32), CameraManagerError> {
    let (width, height) = value
        .split_once('x')
        .ok_or(CameraManagerError::InvalidBootstrapField("resolution"))?;

    let width = width
        .parse::<u32>()
        .map_err(|_| CameraManagerError::InvalidBootstrapField("resolution"))?;
    let height = height
        .parse::<u32>()
        .map_err(|_| CameraManagerError::InvalidBootstrapField("resolution"))?;

    Ok((width, height))
}

fn value_from_request(
    query: &HashMap<String, String>,
    headers: &HeaderMap,
    query_key: &str,
    header_key: &str,
) -> Option<String> {
    query
        .get(query_key)
        .cloned()
        .or_else(|| headers.get(header_key).and_then(|value| value.to_str().ok().map(str::to_owned)))
}

fn has_bootstrap(query: &HashMap<String, String>, headers: &HeaderMap) -> bool {
    ["node", "resolution", "framerate", "capture_encoding"]
        .iter()
        .any(|key| query.contains_key(*key))
        || ["x-node", "x-resolution", "x-framerate", "x-capture-encoding"]
            .iter()
            .any(|key| headers.contains_key(*key))
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
