use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_pbutils as gst_pbutils;
use gstreamer_pbutils::prelude::*;
use std::{
    collections::BTreeMap,
    fmt,
    path::{Component, Path, PathBuf},
};
use walkdir::WalkDir;

use crate::domain::{
    AudioDetails, MediaItem, MediaProbe, MediaStream, ProbeError, ProbeStatus, VideoDetails,
    probe_status_for_discoverer_result,
};

const DISCOVERY_TIMEOUT_SECONDS: u64 = 15;

#[derive(Debug)]
pub enum MediaPathError {
    InvalidId,
    InvalidPath,
    Io(std::io::Error),
}

impl fmt::Display for MediaPathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId => write!(formatter, "media id is invalid"),
            Self::InvalidPath => write!(
                formatter,
                "media path is outside MEDIA_ROOT or is not a file"
            ),
            Self::Io(error) => write!(formatter, "unable to access media path: {error}"),
        }
    }
}

impl std::error::Error for MediaPathError {}

pub fn scan(root: &Path) -> Vec<MediaItem> {
    let Ok(canonical_root) = std::fs::canonicalize(root) else {
        return Vec::new();
    };
    WalkDir::new(&canonical_root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|entry| media_item(&canonical_root, entry.path(), entry.metadata().ok()?.len()))
        .collect()
}

fn media_item(root: &Path, path: &Path, size_bytes: u64) -> Option<MediaItem> {
    let relative = path.strip_prefix(root).ok()?;
    let relative_text = relative.to_str()?.to_owned();
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    Some(MediaItem {
        id: encode_media_id(&relative_text),
        path: relative_text,
        filename: path.file_name()?.to_string_lossy().into_owned(),
        size_bytes,
        extension,
        probe_status: ProbeStatus::Pending,
    })
}

pub fn resolve_media_id(root: &Path, id: &str) -> Result<PathBuf, MediaPathError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(id)
        .map_err(|_| MediaPathError::InvalidId)?;
    let relative = String::from_utf8(bytes).map_err(|_| MediaPathError::InvalidId)?;
    resolve_media_path(root, &relative)
}

pub fn resolve_media_path(root: &Path, relative: &str) -> Result<PathBuf, MediaPathError> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(MediaPathError::InvalidPath);
    }
    let canonical_root = std::fs::canonicalize(root).map_err(MediaPathError::Io)?;
    let canonical_path =
        std::fs::canonicalize(canonical_root.join(relative_path)).map_err(MediaPathError::Io)?;
    if !canonical_path.starts_with(&canonical_root) || !canonical_path.is_file() {
        return Err(MediaPathError::InvalidPath);
    }
    Ok(canonical_path)
}

pub fn probe(path: &Path, id: String) -> MediaProbe {
    let uri = match gst::glib::filename_to_uri(path, None) {
        Ok(uri) => uri,
        Err(error) => return MediaProbe::failed(id, "invalid_media_path", error.to_string()),
    };
    let discoverer =
        match gst_pbutils::Discoverer::new(gst::ClockTime::from_seconds(DISCOVERY_TIMEOUT_SECONDS))
        {
            Ok(discoverer) => discoverer,
            Err(error) => {
                return MediaProbe::failed(
                    id,
                    "discoverer_initialization_failed",
                    error.to_string(),
                );
            }
        };
    match discoverer.discover_uri(uri.as_str()) {
        Ok(info) => map_discoverer_info(id, &info),
        Err(error) => MediaProbe::failed(id, "discovery_failed", error.to_string()),
    }
}

fn map_discoverer_info(id: String, info: &gst_pbutils::DiscovererInfo) -> MediaProbe {
    let status = probe_status_for_discoverer_result(info.result());
    let error = match status {
        ProbeStatus::Ready | ProbeStatus::Pending => None,
        ProbeStatus::Unsupported => Some(ProbeError {
            code: "unsupported_media".to_owned(),
            message: format!("GStreamer discovery result: {:?}", info.result()),
        }),
        ProbeStatus::Incomplete => Some(ProbeError {
            code: "discovery_incomplete".to_owned(),
            message: format!("GStreamer discovery result: {:?}", info.result()),
        }),
        ProbeStatus::Failed => Some(ProbeError {
            code: "discovery_failed".to_owned(),
            message: format!("GStreamer discovery result: {:?}", info.result()),
        }),
    };
    MediaProbe {
        id,
        status,
        container_format: info
            .container_streams()
            .first()
            .and_then(|stream| stream.caps())
            .map(|caps| caps.to_string()),
        duration_ms: info.duration().map(|duration| duration.mseconds()),
        seekable: Some(info.is_seekable()),
        tags: map_tags(info.tags().as_ref()),
        streams: info.stream_list().iter().map(map_stream).collect(),
        error,
    }
}

fn map_stream(stream: &gst_pbutils::DiscovererStreamInfo) -> MediaStream {
    let caps = stream.caps();
    let codec_name = caps
        .as_ref()
        .and_then(|caps| caps.structure(0))
        .map(|structure| structure.name().to_string());
    let mut mapped = MediaStream {
        id: stream.stream_id().map(|id| id.to_string()),
        kind: stream.stream_type_nick().to_string(),
        codec_name,
        caps: caps.map(|caps| caps.to_string()),
        bitrate: None,
        video: None,
        audio: None,
    };
    if let Ok(video) = stream
        .clone()
        .downcast::<gst_pbutils::DiscovererVideoInfo>()
    {
        let framerate = video.framerate();
        mapped.bitrate = non_zero(video.bitrate());
        mapped.video = Some(VideoDetails {
            width: video.width(),
            height: video.height(),
            framerate_numerator: framerate.numer(),
            framerate_denominator: framerate.denom(),
        });
    } else if let Ok(audio) = stream
        .clone()
        .downcast::<gst_pbutils::DiscovererAudioInfo>()
    {
        mapped.bitrate = non_zero(audio.bitrate());
        mapped.audio = Some(AudioDetails {
            sample_rate: audio.sample_rate(),
            channels: audio.channels(),
        });
    }
    mapped
}

fn non_zero(value: u32) -> Option<u32> {
    (value != 0).then_some(value)
}

fn map_tags(tags: Option<&gst::TagList>) -> BTreeMap<String, Vec<String>> {
    tags.map(|tags| {
        tags.iter_generic()
            .map(|(name, values)| {
                (
                    name.to_string(),
                    values
                        .map(|value| {
                            value
                                .serialize()
                                .map(|value| value.to_string())
                                .unwrap_or_else(|_| format!("{value:?}"))
                        })
                        .collect(),
                )
            })
            .collect()
    })
    .unwrap_or_default()
}

fn encode_media_id(relative: &str) -> String {
    URL_SAFE_NO_PAD.encode(relative.as_bytes())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{resolve_media_path, scan};

    #[test]
    fn resolves_a_file_inside_the_canonical_media_root() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().join("media");
        fs::create_dir(&root).expect("media root");
        let file = root.join("clip.mp4");
        fs::write(&file, b"fixture").expect("fixture file");

        assert_eq!(
            resolve_media_path(&root, "clip.mp4").expect("safe path"),
            fs::canonicalize(file).expect("canonical fixture")
        );
    }

    #[test]
    fn rejects_parent_traversal_outside_media_root() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().join("media");
        fs::create_dir(&root).expect("media root");
        fs::write(directory.path().join("outside.mp4"), b"fixture").expect("outside fixture");

        assert!(resolve_media_path(&root, "../outside.mp4").is_err());
    }

    #[test]
    fn scan_ignores_hidden_files() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().join("media");
        fs::create_dir(&root).expect("media root");
        fs::write(root.join(".gitkeep"), b"placeholder").expect("placeholder");

        assert!(scan(&root).is_empty());
    }
}
