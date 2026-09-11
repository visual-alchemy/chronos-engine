# CHRONOS Product Requirements Document

## Vision
CHRONOS is a Docker-deployable adaptive file playout engine. An operator selects an asset, CHRONOS probes its streams, explains compatibility with a selected network-output profile, prefers remux/stream-copy, and only transcodes tracks that require it. MVP output is MPEG-TS over SRT listener; playback can loop indefinitely.

## MVP goals
1. Scan a mounted read-only media library.
2. Probe container, tracks, codecs, duration, resolution, FPS, bitrate, and metadata.
3. Evaluate the source against `srt-ts-universal`.
4. Explain whether full copy, hybrid, or full transcode is required.
5. Start/stop one SRT listener playout per allocated UDP port.
6. Loop sources safely at EOS and emit state events.
7. Persist media cache, profiles, stream definitions, and history in SQLite.
8. Provide HTTP API, WebSocket events, Docker Compose, healthcheck, logs, and metrics.

## Non-goals
No schedule/playlist automation, ad insertion, frame-accurate broadcast playout, SDI/NDI, cluster orchestration, or unrestricted external URL inputs in MVP.

## Acceptance criteria
- H.264 + AAC MOV/MP4 starts as MPEG-TS SRT listener without decode/encode.
- Incompatible track produces a human-readable helper and valid fallback options.
- A looped source recovers from EOS without process exit.
- A bound port cannot be allocated twice.
- `GET /healthz` and stream status expose failure state without parsing logs.
