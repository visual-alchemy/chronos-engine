# Architecture

## Components
- API/UI: Axum REST and WebSocket endpoints.
- Catalog: scans `/media`, caches Discoverer output in SQLite.
- Planner: maps source caps to an output-profile decision.
- Supervisor: owns stream state machines and routes commands to GStreamer.
- Runtime: one `gst::Pipeline` per playout; Bus messages drive errors, EOS, and telemetry.
- Transport: `srtsink` emits MPEG-TS for caller clients.

## State machine
`draft → probing → ready → starting → waiting_for_caller → running → looping → stopping → stopped`. Terminal failures transition to `failed`; recovery is bounded by a restart budget.

## Processing plans
- `remux_copy`: demux → parser → mpegtsmux → srtsink.
- `copy_video_encode_audio`: preserve compatible video, encode audio only.
- `encode_video_copy_audio`: preserve compatible audio, encode video only.
- `full_transcode`: decode both tracks and output profile codecs.

## Safety
Canonicalize every catalog path and require it to remain under `/media`. Never concatenate user input into a GStreamer launch description. Use port range 9000-9099. Secrets must be masked in API events and logs.
