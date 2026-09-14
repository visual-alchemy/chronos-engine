use std::collections::BTreeMap;

use gstreamer_pbutils::DiscovererResult;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MediaItem {
    pub id: String,
    pub path: String,
    pub filename: String,
    pub size_bytes: u64,
    pub extension: Option<String>,
    pub probe_status: ProbeStatus,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    Pending,
    Ready,
    Unsupported,
    Incomplete,
    Failed,
}

pub(crate) fn probe_status_for_discoverer_result(result: DiscovererResult) -> ProbeStatus {
    match result {
        DiscovererResult::Ok => ProbeStatus::Ready,
        DiscovererResult::MissingPlugins => ProbeStatus::Unsupported,
        DiscovererResult::Timeout | DiscovererResult::Busy => ProbeStatus::Incomplete,
        DiscovererResult::UriInvalid | DiscovererResult::Error => ProbeStatus::Failed,
        _ => ProbeStatus::Failed,
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MediaProbe {
    pub id: String,
    pub status: ProbeStatus,
    pub container_format: Option<String>,
    pub duration_ms: Option<u64>,
    pub seekable: Option<bool>,
    pub tags: BTreeMap<String, Vec<String>>,
    pub streams: Vec<MediaStream>,
    pub error: Option<ProbeError>,
}

impl MediaProbe {
    pub fn failed(id: String, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            id,
            status: ProbeStatus::Failed,
            container_format: None,
            duration_ms: None,
            seekable: None,
            tags: BTreeMap::new(),
            streams: Vec::new(),
            error: Some(ProbeError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProbeError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MediaStream {
    pub id: Option<String>,
    pub kind: String,
    pub codec_name: Option<String>,
    pub caps: Option<String>,
    pub bitrate: Option<u32>,
    pub video: Option<VideoDetails>,
    pub audio: Option<AudioDetails>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VideoDetails {
    pub width: u32,
    pub height: u32,
    pub framerate_numerator: i32,
    pub framerate_denominator: i32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AudioDetails {
    pub sample_rate: u32,
    pub channels: u32,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamState {
    Draft,
    Probing,
    Ready,
    Starting,
    WaitingForCaller,
    Running,
    Looping,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Serialize)]
pub struct SrtClientAddress {
    pub ip: String,
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StreamEvent {
    pub stream_id: String,
    pub state: StreamState,
    pub detail: Option<String>,
    pub port: Option<u16>,
    pub latency_ms: Option<u32>,
    pub mode: Option<ProcessingMode>,
    /// Number of times the file has looped (0 = first play-through).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loop_count: Option<u64>,
    #[serde(default)]
    pub clients: Vec<SrtClientAddress>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CompatibilityPlan {
    pub profile_id: String,
    pub mode: ProcessingMode,
    pub video_codec: String,
    pub audio_codec: String,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum ProcessingMode {
    RemuxCopy,
    CopyVideoEncodeAudio,
    EncodeVideoCopyAudio,
    FullTranscode,
}

#[cfg(test)]
mod tests {
    use super::{
        ProbeStatus, ProcessingMode, StreamEvent, StreamState, probe_status_for_discoverer_result,
    };

    #[test]
    fn stream_event_serializes_connected_clients() {
        let event = StreamEvent {
            stream_id: "feed".into(),
            state: StreamState::Running,
            detail: None,
            port: Some(10000),
            latency_ms: Some(120),
            mode: Some(ProcessingMode::RemuxCopy),
            loop_count: Some(0),
            clients: vec![super::SrtClientAddress {
                ip: "192.0.2.10".into(),
                port: 54321,
            }],
        };

        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["clients"][0]["ip"], "192.0.2.10");
        assert_eq!(value["clients"][0]["port"], 54321);
    }

    #[test]
    fn maps_missing_plugins_to_unsupported() {
        assert_eq!(
            probe_status_for_discoverer_result(gstreamer_pbutils::DiscovererResult::MissingPlugins),
            ProbeStatus::Unsupported
        );
    }

    #[test]
    fn maps_timeout_to_incomplete() {
        assert_eq!(
            probe_status_for_discoverer_result(gstreamer_pbutils::DiscovererResult::Timeout),
            ProbeStatus::Incomplete
        );
    }
}
