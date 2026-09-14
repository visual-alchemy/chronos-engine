use crate::{
    domain::{CompatibilityPlan, MediaProbe, ProcessingMode},
    profiles::OutputProfile,
};

pub fn plan(probe: &MediaProbe, profile: &OutputProfile) -> CompatibilityPlan {
    let video = probe
        .streams
        .iter()
        .find(|stream| stream.kind == "video")
        .map(|stream| {
            let combined = format!(
                "{} {}",
                stream.codec_name.as_deref().unwrap_or_default(),
                stream.caps.as_deref().unwrap_or_default()
            );
            normalize_codec(&combined)
        });
    let audio = probe
        .streams
        .iter()
        .find(|stream| stream.kind == "audio")
        .map(|stream| {
            let combined = format!(
                "{} {}",
                stream.codec_name.as_deref().unwrap_or_default(),
                stream.caps.as_deref().unwrap_or_default()
            );
            normalize_codec(&combined)
        });
    let video_copy = video.as_deref().is_some_and(|codec| {
        profile
            .video_copy_codecs
            .iter()
            .any(|allowed| allowed == codec)
    });
    let audio_copy = audio.as_deref().is_some_and(|codec| {
        profile
            .audio_copy_codecs
            .iter()
            .any(|allowed| allowed == codec)
    });
    let mode = match (video_copy, audio_copy) {
        (true, true) => ProcessingMode::RemuxCopy,
        (true, false) => ProcessingMode::CopyVideoEncodeAudio,
        (false, true) => ProcessingMode::EncodeVideoCopyAudio,
        (false, false) => ProcessingMode::FullTranscode,
    };
    let mut reasons = Vec::new();
    if !video_copy {
        reasons.push("video codec requires transcoding".into());
    }
    if !audio_copy {
        reasons.push("audio codec requires transcoding".into());
    }
    CompatibilityPlan {
        profile_id: "srt-ts-universal".into(),
        mode,
        video_codec: if video_copy {
            video.unwrap_or_default()
        } else {
            profile.fallback_video.clone()
        },
        audio_codec: if audio_copy {
            audio.unwrap_or_default()
        } else {
            profile.fallback_audio.clone()
        },
        reasons,
    }
}

fn normalize_codec(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if lower.contains("h264") {
        "h264".into()
    } else if lower.contains("h265") || lower.contains("hevc") {
        "h265".into()
    } else if lower.contains("aac") || lower.contains("mpegversion=(int)4") {
        "aac".into()
    } else if lower.contains("mp3") {
        "mp3".into()
    } else if lower.contains("ac3") {
        "ac3".into()
    } else {
        lower.split('/').next_back().unwrap_or("unknown").into()
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::{MediaProbe, MediaStream, ProbeStatus};
    use crate::profiles::OutputProfile;

    use super::plan;

    #[test]
    fn chooses_remux_copy_for_h264_and_aac() {
        let probe = MediaProbe {
            id: "clip".into(),
            status: ProbeStatus::Ready,
            container_format: None,
            duration_ms: None,
            seekable: Some(true),
            tags: Default::default(),
            streams: vec![
                MediaStream {
                    id: None,
                    kind: "video".into(),
                    codec_name: Some("video/x-h264".into()),
                    caps: None,
                    bitrate: None,
                    video: None,
                    audio: None,
                },
                MediaStream {
                    id: None,
                    kind: "audio".into(),
                    codec_name: Some("audio/mpeg, mpegversion=(int)4".into()),
                    caps: None,
                    bitrate: None,
                    video: None,
                    audio: None,
                },
            ],
            error: None,
        };
        let profile = OutputProfile::universal();

        assert_eq!(
            plan(&probe, &profile).mode,
            crate::domain::ProcessingMode::RemuxCopy
        );
    }
}
