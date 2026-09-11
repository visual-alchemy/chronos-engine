# Codex Implementation Tasks

Read PRD and architecture first. Work on one task at a time. Do not spawn `gst-launch-1.0`; use `gstreamer-rs`. Never panic on operator-controlled media. Run `cargo fmt`, `cargo clippy -- -D warnings`, and `cargo test` for each task.

1. Add `gstreamer-pbutils` and cached `GstDiscoverer` probing.
2. Add SQLite migrations and repositories.
3. Parse output profiles and implement rule-driven capability planning.
4. Implement typed SRT listener configuration and UDP port allocator.
5. Implement H.264/AAC copy graph: demux → h264parse/aacparse → mpegtsmux → srtsink.
6. Implement supervisor, Bus watcher, graceful stop, and EOS loop restart.
7. Add stream CRUD/start/stop/status API and WebSocket events.
8. Export SRT/pipeline telemetry and Prometheus metrics.
9. Implement hybrid/full-transcode fallback.
10. Build media browser, helper, profile selection, active-stream, and logs pages.
