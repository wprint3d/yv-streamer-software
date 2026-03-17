use std::{collections::HashMap, convert::Infallible, sync::{atomic::{AtomicBool, Ordering}, Arc}};

use async_stream::stream;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use bytes::Bytes;
use serde_json::json;

use crate::manager::{build_mjpeg_chunk, CameraManager, CameraManagerError};

pub fn build_router(manager: CameraManager) -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/cameras", get(list_cameras))
        .route("/api/v1/cameras/{camera_id}/state", get(internal_state))
        .route("/api/v1/cameras/{camera_id}/snapshot.jpg", get(internal_snapshot))
        .route("/api/v1/cameras/{camera_id}/stream.mjpeg", get(internal_stream))
        .route("/{camera_id}/state", get(compat_state))
        .route("/{camera_id}/snapshot", get(compat_snapshot))
        .route("/{camera_id}/stream", get(compat_stream))
        .route("/{camera_id}", get(compat_root))
        .route("/{camera_id}/", get(compat_root))
        .with_state(manager)
}

async fn health() -> impl IntoResponse {
    Json(json!({ "status": "ok" }))
}

async fn list_cameras(State(manager): State<CameraManager>) -> impl IntoResponse {
    Json(manager.list_cameras())
}

async fn internal_state(
    State(manager): State<CameraManager>,
    Path(camera_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    match manager.ensure_or_get_existing(&camera_id, &query, &headers) {
        Ok(worker) => Json(worker.snapshot()).into_response(),
        Err(error) => error_response(error),
    }
}

async fn internal_snapshot(
    State(manager): State<CameraManager>,
    Path(camera_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    match manager.ensure_or_get_existing(&camera_id, &query, &headers) {
        Ok(worker) => jpeg_response(worker.current_frame()),
        Err(error) => error_response(error),
    }
}

async fn internal_stream(
    State(manager): State<CameraManager>,
    Path(camera_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    match manager.ensure_or_get_existing(&camera_id, &query, &headers) {
        Ok(worker) => {
            let (receiver, frame_consumed) = worker.subscribe();
            mjpeg_stream_response(receiver, frame_consumed)
        }
        Err(error) => error_response(error),
    }
}

async fn compat_state(
    State(manager): State<CameraManager>,
    Path(camera_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    internal_state(State(manager), Path(camera_id), Query(query), headers).await
}

async fn compat_snapshot(
    State(manager): State<CameraManager>,
    Path(camera_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    internal_snapshot(State(manager), Path(camera_id), Query(query), headers).await
}

async fn compat_stream(
    State(manager): State<CameraManager>,
    Path(camera_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    internal_stream(State(manager), Path(camera_id), Query(query), headers).await
}

async fn compat_root(
    State(manager): State<CameraManager>,
    Path(camera_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    match query.get("action").map(String::as_str) {
        Some("snapshot") => internal_snapshot(State(manager), Path(camera_id), Query(query), headers).await,
        Some("stream") => internal_stream(State(manager), Path(camera_id), Query(query), headers).await,
        Some("state") => internal_state(State(manager), Path(camera_id), Query(query), headers).await,
        _ => (StatusCode::NOT_FOUND, Json(json!({ "error": "Unsupported compatibility action" }))).into_response(),
    }
}

fn jpeg_response(frame: Bytes) -> Response {
    let mut response = Response::new(Body::from(frame));
    response
        .headers_mut()
        .insert("content-type", HeaderValue::from_static("image/jpeg"));
    response
}

fn mjpeg_stream_response(mut receiver: tokio::sync::watch::Receiver<Bytes>, frame_consumed: Arc<AtomicBool>) -> Response {
    let body_stream = stream! {
        // Signal readiness so the capture thread produces a fresh frame
        // (breaks the frame-skip deadlock when no prior subscriber consumed).
        frame_consumed.store(true, Ordering::Relaxed);

        let initial = receiver.borrow().clone();
        if !initial.is_empty() {
            yield Ok::<Bytes, Infallible>(build_mjpeg_chunk(&initial));
        }

        while receiver.changed().await.is_ok() {
            let frame = receiver.borrow().clone();

            if frame.is_empty() {
                continue;
            }

            frame_consumed.store(true, Ordering::Relaxed);
            yield Ok::<Bytes, Infallible>(build_mjpeg_chunk(&frame));
        }
    };

    let mut response = Response::new(Body::from_stream(body_stream));
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("multipart/x-mixed-replace; boundary=frame"),
    );
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

fn error_response(error: CameraManagerError) -> Response {
    let status = match error {
        CameraManagerError::CameraNotFound(_) => StatusCode::NOT_FOUND,
        CameraManagerError::InvalidBootstrapField(_)
        | CameraManagerError::MissingBootstrapField(_)
        | CameraManagerError::UnsupportedCaptureEncoding(_) => StatusCode::BAD_REQUEST,
        CameraManagerError::WorkerInitialization(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };

    (status, Json(json!({ "error": error.to_string() }))).into_response()
}
