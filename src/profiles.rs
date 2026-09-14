#[cfg(test)]
mod tests {
    use super::OutputProfiles;

    #[test]
    fn loads_the_universal_srt_profile() {
        let profiles = OutputProfiles::from_yaml(include_str!("../config/output-profiles.yaml"))
            .expect("profiles parse");

        assert!(profiles.get("srt-ts-universal").is_some());
    }
}
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OutputProfile {
    pub description: String,
    pub protocol: String,
    pub container: String,
    pub video_copy_codecs: Vec<String>,
    pub audio_copy_codecs: Vec<String>,
    pub fallback_video: String,
    pub fallback_audio: String,
    #[serde(default = "default_latency_ms")]
    pub default_latency_ms: u32,
}

fn default_latency_ms() -> u32 {
    120
}

impl OutputProfile {
    #[cfg(test)]
    pub fn universal() -> Self {
        OutputProfiles::from_yaml(include_str!("../config/output-profiles.yaml"))
            .expect("built-in profiles")
            .get("srt-ts-universal")
            .expect("universal profile")
            .clone()
    }
}

#[derive(Debug, Deserialize)]
pub struct OutputProfiles {
    profiles: BTreeMap<String, OutputProfile>,
}

impl OutputProfiles {
    pub fn from_yaml(source: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(source)
    }
    pub fn get(&self, id: &str) -> Option<&OutputProfile> {
        self.profiles.get(id)
    }
}
