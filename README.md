# C.H.R.O.N.O.S

**C**yclic **H**igh-performance **R**emuxing & **O**n-demand **N**etwork **O**utput **S**ystem.

Rust + GStreamer engine for inspecting local media, selecting the least expensive valid processing path, looping assets, and publishing network outputs. The MVP target is an SRT listener with MPEG-TS output and a copy-first policy.

## Prerequisites

| Requirement | Minimum |
|---|---|
| Rust | 1.85+ (uses edition 2024) |
| GStreamer | 1.x (dev headers to build, plugin set to run) |
| pkg-config | any recent |

The asset bundle (`assets/`) and output profiles (`config/output-profiles.yaml`) are compiled into the binary via `include_str!` — no runtime config files are required. Only your media directory and a writable database path are needed at runtime.

### GStreamer plugins used

| Pipeline path | Elements |
|---|---|
| Copy (remux) | `filesrc`, `qtdemux`, `h264parse`, `aacparse`, `mpegtsmux`, `srtsink` |
| Hybrid / transcode | above + `x264enc`, `avenc_aac`, `decodebin`, `videoconvert`, `audioresample` |

Required plugin sets: **base**, **good**, **bad** (`mpegtsmux`, `srtsink`), **ugly** (`x264enc`), **libav** (`avenc_aac`).

## Runtime configuration

| Env var | Default | Purpose |
|---|---|---|
| `MEDIA_ROOT` | `/media` | Directory to scan for media assets |
| `DATABASE_PATH` | `chronos.db` | SQLite file for probe cache + port allocation |
| `BIND_ADDR` | `0.0.0.0:8502` | HTTP bind address |
| `RUST_LOG` | `chronos_engine=info,tower_http=info` | Tracing filter |
| `GST_DEBUG` | — | GStreamer debug level (e.g. `3`) |
| `LAN_IP` | — | Host's LAN IP shown to other devices; set this when running in Docker |

### Ports

| Port | Protocol | Purpose |
|---|---|---|
| `8502` | TCP | HTTP API + dashboard |
| `10000–10049` | UDP | SRT listener range (one per stream) |

---

## Build & run

### Docker (recommended)

```bash
# put your media files in ./media, then:
docker compose up --build
```

This builds the image, mounts `./media` read-only at `/media`, persists the SQLite DB in the `chronos-data` volume, and publishes ports `8502/tcp` + `10000–10049/udp`.

Build and run the image manually:

```bash
docker build -t chronos-engine .
docker run --rm -p 8502:8502/tcp -p 10000-10049:10000-10049/udp \
  -v "$PWD/media:/media:ro" -v chronos-data:/data \
  -e MEDIA_ROOT=/media -e DATABASE_PATH=/data/chronos.db \
  chronos-engine
```

### Linux — bare metal (Debian / Ubuntu)

Install Rust via [rustup](https://rustup.rs), then the build + runtime packages:

```bash
sudo apt-get update
sudo apt-get install -y \
  pkg-config libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
  gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly gstreamer1.0-libav
```

Build and run:

```bash
cargo build --release
MEDIA_ROOT=./media DATABASE_PATH=./chronos.db ./target/release/chronos-engine
```

Other distros install the equivalent of `gst-plugins-base/good/bad/ugly` + `gst-libav`. Ensure the SRT plugin (from **bad**) and `avenc_aac` (from **libav**) are present, otherwise hybrid/transcode modes fall back or fail.

### macOS — bare metal

Install Rust via [rustup](https://rustup.rs), then GStreamer via Homebrew:

```bash
brew install gstreamer gst-plugins-base gst-plugins-good \
  gst-plugins-bad gst-plugins-ugly gst-libav
```

Build and run:

```bash
cargo build --release
MEDIA_ROOT=./media DATABASE_PATH=./chronos.db ./target/release/chronos-engine
```

If `pkg-config` cannot locate GStreamer, export the Homebrew pkg-config path first (Apple Silicon shown):

```bash
export PKG_CONFIG_PATH="/opt/homebrew/lib/pkgconfig:$PKG_CONFIG_PATH"
```

### Windows — bare metal (MSYS2, recommended)

Install [MSYS2](https://www.msys2.org/), then in an **MSYS2 MINGW64** shell install the toolchain and GStreamer:

```bash
pacman -S --needed \
  mingw-w64-x86_64-rust \
  mingw-w64-x86_64-pkg-config \
  mingw-w64-x86_64-gstreamer \
  mingw-w64-x86_64-gst-plugins-base \
  mingw-w64-x86_64-gst-plugins-good \
  mingw-w64-x86_64-gst-plugins-bad \
  mingw-w64-x86_64-gst-plugins-ugly \
  mingw-w64-x86_64-gst-libav
```

Build and run from the MINGW64 shell:

```bash
cargo build --release
MEDIA_ROOT=./media DATABASE_PATH=./chronos.db ./target/release/chronos-engine.exe
```

Alternative: install the official GStreamer runtime + development MSI installers (gstreamer.freedesktop.org), then build with the MSVC Rust toolchain (requires Visual Studio Build Tools with the C++ workload). Point `pkg-config` at the GStreamer dev tree and add the GStreamer `bin` directory to `PATH`.

---

## Verify

```bash
cargo test                 # unit + integration tests (47)
cargo fmt --check          # formatting
cargo clippy -D warnings   # lints
```

## Usage

Open http://localhost:8502/ for the media and stream dashboard. Prometheus-compatible stream metrics are available at http://localhost:8502/metrics.

```bash
curl http://localhost:8502/healthz
curl http://localhost:8502/api/media
```

`GET /api/media` returns each asset's opaque `id`. Probe an asset with:

```bash
curl http://localhost:8502/api/media/<id>/probe
```

Probes use the GStreamer Rust bindings and cache results for the running process. See `docs/API.md` for the full response contract.

Read `docs/PRD.md`, `docs/ARCHITECTURE.md`, and `docs/CODEX_TASKS.md` before implementing features.
