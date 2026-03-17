pub mod app;
pub mod manager;
pub mod startup;

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::{
        app::build_router,
        manager::{CameraConfig, CameraManager},
        startup::{detect_video_devices_in_dir, resolve_log_filter, should_emit_debug_boot_report},
    };

    #[tokio::test]
    async fn health_endpoint_reports_ok() {
        let manager = CameraManager::test_new();
        let app = build_router(manager);

        let response = app
            .oneshot(Request::builder().uri("/api/v1/health").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn snapshot_and_compatibility_endpoints_expose_registered_camera_frames() {
        let manager = CameraManager::test_new();
        manager.register_static_frame(CameraConfig::test("cam-1"), vec![1, 2, 3, 4]);
        let app = build_router(manager);

        let snapshot = app
            .clone()
            .oneshot(Request::builder().uri("/api/v1/cameras/cam-1/snapshot.jpg").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(snapshot.status(), StatusCode::OK);
        assert_eq!(snapshot.headers()["content-type"], "image/jpeg");

        let compat = app
            .oneshot(Request::builder().uri("/cam-1?action=snapshot").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(compat.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn mjpeg_stream_endpoints_return_multipart_responses() {
        let manager = CameraManager::test_new();
        manager.register_static_frame(CameraConfig::test("cam-1"), vec![1, 2, 3, 4]);
        let app = build_router(manager);

        let response = app
            .clone()
            .oneshot(Request::builder().uri("/api/v1/cameras/cam-1/stream.mjpeg").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("multipart/x-mixed-replace"));

        let mut body = response.into_body();
        let chunk = body
            .frame()
            .await
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap();
        assert!(chunk.windows("Content-Type: image/jpeg".len()).any(|window| window == b"Content-Type: image/jpeg"));

        let compat_response = app
            .oneshot(Request::builder().uri("/cam-1/stream").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(compat_response.status(), StatusCode::OK);
    }

    #[test]
    fn explicit_log_level_overrides_rust_log() {
        assert_eq!(
            resolve_log_filter(Some("debug".to_string()), Some("warn".to_string())),
            "debug"
        );
    }

    #[test]
    fn rust_log_is_used_when_explicit_log_level_is_missing() {
        assert_eq!(
            resolve_log_filter(None, Some("trace".to_string())),
            "trace"
        );
    }

    #[test]
    fn info_logging_does_not_emit_debug_boot_report() {
        assert!(!should_emit_debug_boot_report("info"));
        assert!(should_emit_debug_boot_report("debug"));
        assert!(should_emit_debug_boot_report("trace"));
        assert!(should_emit_debug_boot_report("yv_streamer_software=debug,tower_http=info"));
    }

    #[test]
    fn video_device_detection_finds_and_sorts_video_nodes() {
        let tempdir = std::env::temp_dir().join(format!("yv-streamer-test-{}", std::process::id()));
        std::fs::create_dir_all(&tempdir).unwrap();
        std::fs::write(tempdir.join("video2"), b"").unwrap();
        std::fs::write(tempdir.join("video0"), b"").unwrap();
        std::fs::write(tempdir.join("not-a-camera"), b"").unwrap();

        let devices = detect_video_devices_in_dir(&tempdir).unwrap();

        assert_eq!(devices, vec![tempdir.join("video0"), tempdir.join("video2")]);

        std::fs::remove_file(tempdir.join("video2")).unwrap();
        std::fs::remove_file(tempdir.join("video0")).unwrap();
        std::fs::remove_file(tempdir.join("not-a-camera")).unwrap();
        std::fs::remove_dir(&tempdir).unwrap();
    }

    #[test]
    fn camera_config_equality_ignores_adaptive_quality() {
        let mut a = CameraConfig::test("cam-1");
        let mut b = CameraConfig::test("cam-1");

        a.adaptive_quality = false;
        b.adaptive_quality = true;
        assert_eq!(a, b, "adaptive_quality should be excluded from equality");

        // Verify other fields ARE included
        b.framerate = 15;
        assert_ne!(a, b, "framerate change should make configs unequal");
    }

    #[tokio::test]
    async fn watch_sender_requires_live_receiver_for_borrow_to_reflect_sent_value() {
        // When all receivers are dropped, send() returns Err and borrow() returns the initial value.
        // We must keep at least one receiver alive for the publish/borrow pattern to work.
        let (tx, rx) = tokio::sync::watch::channel(bytes::Bytes::new());

        let payload = bytes::Bytes::from_static(b"test-jpeg-data");
        let send_result = tx.send(payload.clone());
        assert!(send_result.is_ok(), "send should succeed with a live receiver");

        let borrowed = tx.borrow().clone();
        assert_eq!(borrowed, payload, "borrow() must return the sent value");
        drop(rx);
    }

    #[test]
    fn integer_yuv_to_rgb_matches_reference_output() {
        use crate::manager::yuv_to_rgb;

        // Pure white: Y=255, U=0, V=0 (after -128 offset applied by caller)
        assert_eq!(yuv_to_rgb(255, 0, 0), [255, 255, 255]);

        // Pure black: Y=0, U=0, V=0
        assert_eq!(yuv_to_rgb(0, 0, 0), [0, 0, 0]);

        // Mid-gray: Y=128, U=0, V=0
        assert_eq!(yuv_to_rgb(128, 0, 0), [128, 128, 128]);

        // Red-ish: Y=128, U=-50, V=100
        let rgb = yuv_to_rgb(128, -50, 100);
        let ref_r = (128.0 + 1.402 * 100.0f32).round().clamp(0.0, 255.0) as u8;
        let ref_g = (128.0 - 0.344136 * -50.0f32 - 0.714136 * 100.0).round().clamp(0.0, 255.0) as u8;
        let ref_b = (128.0 + 1.772 * -50.0f32).round().clamp(0.0, 255.0) as u8;
        assert!((rgb[0] as i16 - ref_r as i16).abs() <= 1, "red: {} vs {}", rgb[0], ref_r);
        assert!((rgb[1] as i16 - ref_g as i16).abs() <= 1, "green: {} vs {}", rgb[1], ref_g);
        assert!((rgb[2] as i16 - ref_b as i16).abs() <= 1, "blue: {} vs {}", rgb[2], ref_b);
    }
}
