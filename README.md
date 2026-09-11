# C.H.R.O.N.O.S

**C**yclic **H**igh-performance **R**emuxing & **O**n-demand **N**etwork **O**utput **S**ystem.

Rust + GStreamer engine for inspecting local media, selecting the least expensive valid processing path, looping assets, and publishing network outputs. The MVP target is SRT listener with MPEG-TS output and a copy-first policy.

## Run

```bash
mkdir -p /srv/chronos/media
docker compose up --build
curl http://localhost:8080/healthz
curl http://localhost:8080/api/media
```

Read `docs/PRD.md`, `docs/ARCHITECTURE.md`, and `docs/CODEX_TASKS.md` before implementing features.
