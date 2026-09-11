use serde::Serialize;
#[derive(Debug, Clone, Serialize)]
pub struct MediaItem { pub path: String, pub filename: String, pub size_bytes: u64, pub extension: Option<String>, pub probe_status: ProbeStatus }
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus { Pending, Unsupported }
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingMode { RemuxCopy, CopyVideoEncodeAudio, EncodeVideoCopyAudio, FullTranscode }
