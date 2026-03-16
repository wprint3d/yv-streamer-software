# yv-streamer-software

`yv-streamer-software` is the software MJPEG fallback sidecar used by WPrint 3D for UVC cameras that expose raw formats such as `YUYV` instead of a browser-friendly MJPEG stream.

>Hey there, human here! So, uh... no, I haven't learned Rust yet. But I have a beatiful subscription to Codex and I wanted to give it a try. It works. I don't know why or how, but it does, and I don't care enough because this will run behind a bizillion layers of abstraction and containerization and will be thoroughly tested before it ever sees the light of day. So here we are. I hope this is helpful to someone, but if not, that's fine too. There's so many things I use and don't fully understand, whatever.
>
>The code is readable enough and I moreso get the general architecture and approach, and that's enough in my books. If it DOES end up working as reliably as it seems to be, this might become a multi-standard streamer. In that case, it'll be renamed to something more generic, so stay tuned for that.
>
>Also, don't hate me, please. :)

## Endpoints

### Internal API

- `GET /api/v1/health`
- `GET /api/v1/cameras`
- `GET /api/v1/cameras/:id/state`
- `GET /api/v1/cameras/:id/snapshot.jpg`
- `GET /api/v1/cameras/:id/stream.mjpeg`

### Compatibility API

- `GET /:id/state`
- `GET /:id/snapshot`
- `GET /:id/stream`
- `GET /:id?action=state`
- `GET /:id?action=snapshot`
- `GET /:id?action=stream`

## Bootstrap Configuration

When WPrint proxies a fallback camera through this service, it forwards the runtime parameters through request headers:

- `X-Node`
- `X-Resolution`
- `X-Framerate`
- `X-Capture-Encoding`

Equivalent query string parameters are also accepted for direct development and diagnostics.

## Logging

Use `YV_STREAMER_SOFTWARE_LOG_LEVEL` to control the sidecar log verbosity. It defaults to `info`.

Examples:

- `YV_STREAMER_SOFTWARE_LOG_LEVEL=debug`
- `YV_STREAMER_SOFTWARE_LOG_LEVEL=trace`
- `YV_STREAMER_SOFTWARE_LOG_LEVEL=warn`

When the effective log level includes `debug` or `trace`, the service emits a startup report that walks through initialization, scans `/dev/video*`, logs the detected V4L2 devices it can inspect, and lists the camera workers currently managed by the process.

## Local Validation

Build and run tests in Docker:

```bash
docker build -f yv-streamer-software/Dockerfile --target test yv-streamer-software
```

## Running Manually

Build the standalone image:

```bash
docker build -t yv-streamer-software:local -f yv-streamer-software/Dockerfile yv-streamer-software
```

Create a small Docker network for manual testing and start the sidecar inside it:

```bash
docker network create yv-streamer-software-net

docker run -d \
  --name yv-streamer-software \
  --network yv-streamer-software-net \
  -p 8080:8080 \
  --privileged \
  -e YV_STREAMER_SOFTWARE_HOST=0.0.0.0 \
  -e YV_STREAMER_SOFTWARE_PORT=8080 \
  -e YV_STREAMER_SOFTWARE_LOG_LEVEL=debug \
  -v /dev:/dev \
  -v /dev/bus/usb:/dev/bus/usb \
  -v /sys/class:/sys/class \
  -v /sys/devices:/sys/devices \
  -v /run/udev:/run/udev:ro \
  yv-streamer-software:local
```

This keeps the process inside a container without depending on the full WPrint stack. The service listens on `0.0.0.0:8080` inside the Docker network and is also published on `http://127.0.0.1:8080` on the host for browser-based testing. A camera worker is created lazily on the first request for a camera ID, using either request headers or query parameters as the bootstrap source.

To exercise it manually, send requests from another container on the same Docker network:

Example using a browser on the host:

```text
http://127.0.0.1:8080/cam-1?action=stream&node=/dev/video0&resolution=640x480&framerate=30&capture_encoding=YUYV
```

Example using query parameters:

```bash
docker run --rm --network yv-streamer-software-net curlimages/curl:8.12.1 \
  "http://yv-streamer-software:8080/cam-1?action=stream&node=/dev/video0&resolution=640x480&framerate=30&capture_encoding=YUYV"
```

Example using the compatibility endpoints and headers:

```bash
docker run --rm --network yv-streamer-software-net curlimages/curl:8.12.1 \
  -H 'X-Node: /dev/video0' \
  -H 'X-Resolution: 640x480' \
  -H 'X-Framerate: 30' \
  -H 'X-Capture-Encoding: YUYV' \
  "http://yv-streamer-software:8080/cam-1/stream"
```

Fetch a single JPEG snapshot instead of a live MJPEG stream:

```bash
docker run --rm --network yv-streamer-software-net \
  -v "$PWD:/workspace" \
  -w /workspace \
  curlimages/curl:8.12.1 \
  -H 'X-Node: /dev/video0' \
  -H 'X-Resolution: 640x480' \
  -H 'X-Framerate: 30' \
  -H 'X-Capture-Encoding: YUYV' \
  "http://yv-streamer-software:8080/cam-1/snapshot" \
  --output snapshot.jpg
```

Inspect the worker state after it has been bootstrapped:

```bash
docker run --rm --network yv-streamer-software-net curlimages/curl:8.12.1 \
  "http://yv-streamer-software:8080/cam-1/state"
```

For manual testing, make sure the `yv-streamer-software` container can read the target V4L2 device, for example `/dev/video0`.

Clean up when you are done:

```bash
docker rm -f yv-streamer-software
docker network rm yv-streamer-software-net
```
