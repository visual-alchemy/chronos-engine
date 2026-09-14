# API

## `GET /api/media/{id}/probe`

Probes an asset previously returned by `GET /api/media`. `id` is an opaque, URL-safe identifier for a path relative to `MEDIA_ROOT`; clients must not construct it from an absolute path.

The server decodes the identifier, canonicalizes the target, and requires that it is a regular file under the canonical `MEDIA_ROOT`. Results are cached by canonical path for the lifetime of the process.

Successful responses have status `200` and return:

```json
{
  "id": "bmVzdGVkL2NsaXAubXA0",
  "status": "ready",
  "container_format": "video/quicktime, variant=(string)iso",
  "duration_ms": 12345,
  "seekable": true,
  "tags": { "title": ["Example clip"] },
  "streams": [
    {
      "id": "video_0",
      "kind": "video",
      "codec_name": "video/x-h264",
      "caps": "video/x-h264, profile=(string)high",
      "bitrate": 4000000,
      "video": { "width": 1920, "height": 1080, "framerate_numerator": 30000, "framerate_denominator": 1001 },
      "audio": null
    }
  ],
  "error": null
}
```

Optional values are `null` when GStreamer cannot report them. `status` is one of `pending`, `ready`, `unsupported`, `incomplete`, or `failed`. `unsupported` indicates missing GStreamer support; `incomplete` indicates a timeout or unavailable Discoverer; `failed` includes an `error` object. Invalid identifiers and paths outside `MEDIA_ROOT` return `400` with `{ "code": "invalid_media", "message": "..." }`.

## `GET /api/media/{id}/compatibility/{profile_id}`

Returns a typed processing decision after the asset has been probed. `mode` is `remux_copy`, `copy_video_encode_audio`, `encode_video_copy_audio`, or `full_transcode`; `reasons` explains every required fallback.

## Streams

`POST /api/streams` creates an SRT listener copy stream:

```json
{ "id": "morning-feed", "media_id": "...", "port": 9000, "latency_ms": 120 }
```

Ports are constrained to `9000`–`9099` and cannot be allocated twice. `GET /api/streams` returns state:

```json
{
  "stream_id": "morning-feed",
  "state": "running",
  "port": 9000,
  "latency_ms": 120,
  "mode": "remux_copy",
  "loop_count": 0,
  "detail": null,
  "clients": [{ "ip": "192.168.1.42", "port": 54321 }]
}
```

`clients` contains only currently connected callers known to this process. A disconnected caller is removed, and the list becomes `[]` after the last tracked caller disconnects. A successful stop clears the list, and a newly created or recreated stream starts with an empty list. Client history, connection timestamps, and persistence are not provided.

`POST /api/streams/{id}/stop` stops the pipeline and releases its port. `GET /api/events` upgrades to a WebSocket that emits stream-state JSON. `GET /metrics` exports Prometheus text metrics.
