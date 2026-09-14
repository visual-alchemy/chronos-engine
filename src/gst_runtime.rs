use std::path::Path;

use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;

use crate::streams::SrtListenerConfig;

fn listener_uri(srt: SrtListenerConfig) -> String {
    format!(
        "srt://:{}?mode=listener&latency={}",
        srt.port, srt.latency_ms
    )
}

fn configure_listener(sink: &gst::Element, srt: SrtListenerConfig) {
    sink.set_property("uri", listener_uri(srt));
    sink.set_property("authentication", false);
    sink.set_property("wait-for-connection", false);
}

fn configure_ts_mux(mux: &gst::Element) {
    // Seven 188-byte transport packets form the conventional 1316-byte SRT
    // payload. This is the layout used by GStreamer's SRT examples.
    mux.set_property("alignment", 7_i32);
    // Emit PAT/PMT tables frequently so clients that connect mid-stream can
    // identify the streams without waiting for the next PAT interval.
    mux.set_property("pat-interval", 100_u32); // 100 ms
    mux.set_property("pmt-interval", 100_u32); // 100 ms
}

fn configure_h264_parser(parser: &gst::Element) {
    // A caller can join after the pipeline begins. Repeat SPS/PPS on each IDR
    // frame so decoders can start without having seen the file's beginning.
    parser.set_property("config-interval", -1_i32);
}

/// Insert an `identity` element with `sync=true` just before `srtsink`.
///
/// Without this, `filesrc` drives the pipeline as fast as possible — the
/// entire file is read in milliseconds and VLC is flooded with data it
/// cannot render (black screen). `sync=true` makes GStreamer wait until
/// the real-time clock reaches each buffer's presentation timestamp before
/// pushing it downstream, replicating `ffmpeg -re` behaviour.
fn make_rate_limiter() -> Result<gst::Element> {
    let id = make("identity")?;
    id.set_property("sync", true);
    Ok(id)
}

fn make_adts_capsfilter() -> Result<gst::Element> {
    let filter = make("capsfilter")?;
    filter.set_property(
        "caps",
        gst::Caps::builder("audio/mpeg")
            .field("mpegversion", 4i32)
            .field("stream-format", "adts")
            .build(),
    );
    Ok(filter)
}

#[cfg(test)]
pub fn copy_graph_elements() -> [&'static str; 9] {
    [
        "filesrc",
        "qtdemux",
        "queue",
        "h264parse",
        "queue",
        "aacparse",
        "capsfilter",
        "mpegtsmux",
        "srtsink",
    ]
}

#[cfg(test)]
pub fn fallback_graph_elements() -> [&'static str; 10] {
    [
        "uridecodebin",
        "videoconvert",
        "x264enc",
        "h264parse",
        "audioconvert",
        "audioresample",
        "avenc_aac",
        "aacparse",
        "mpegtsmux",
        "srtsink",
    ]
}

pub fn build_transcode_pipeline(path: &Path, srt: SrtListenerConfig) -> Result<gst::Pipeline> {
    let pipeline = gst::Pipeline::new();
    let source = make("uridecodebin")?;
    source.set_property("uri", gst::glib::filename_to_uri(path, None)?.as_str());
    let video_convert = make("videoconvert")?;
    let video_encoder = make("x264enc")?;
    let video_parser = make("h264parse")?;
    configure_h264_parser(&video_parser);
    let audio_convert = make("audioconvert")?;
    let audio_resample = make("audioresample")?;
    let audio_encoder = make("avenc_aac")?;
    let audio_parser = make("aacparse")?;
    let mux = make("mpegtsmux")?;
    configure_ts_mux(&mux);
    let rate_limiter = make_rate_limiter()?;
    let sink = make("srtsink")?;
    configure_listener(&sink, srt);
    pipeline.add_many([
        &source,
        &video_convert,
        &video_encoder,
        &video_parser,
        &audio_convert,
        &audio_resample,
        &audio_encoder,
        &audio_parser,
        &mux,
        &rate_limiter,
        &sink,
    ])?;
    gst::Element::link_many([
        &video_convert,
        &video_encoder,
        &video_parser,
        &mux,
        &rate_limiter,
        &sink,
    ])?;
    gst::Element::link_many([
        &audio_convert,
        &audio_resample,
        &audio_encoder,
        &audio_parser,
        &mux,
    ])?;
    let video_sink = video_convert
        .static_pad("sink")
        .context("videoconvert missing sink pad")?;
    let audio_sink = audio_convert
        .static_pad("sink")
        .context("audioconvert missing sink pad")?;
    source.connect_pad_added(move |_, pad| {
        let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
        let structure_name = caps.structure(0).map(|s| s.name().to_string());
        let pad_name = pad.name();
        let is_video = structure_name
            .as_deref()
            .is_some_and(|n| n.starts_with("video/"))
            || pad_name.starts_with("video");
        let is_audio = structure_name
            .as_deref()
            .is_some_and(|n| n.starts_with("audio/"))
            || pad_name.starts_with("audio");
        let target = if is_video {
            &video_sink
        } else if is_audio {
            &audio_sink
        } else {
            return;
        };
        if !target.is_linked() {
            let _ = pad.link(target);
        }
    });
    Ok(pipeline)
}

pub fn build_hybrid_pipeline(
    path: &Path,
    srt: SrtListenerConfig,
    copy_video: bool,
) -> Result<gst::Pipeline> {
    let pipeline = gst::Pipeline::new();
    let source = make("filesrc")?;
    source.set_property("location", path);
    let demux = make("qtdemux")?;
    let video_decode = make("decodebin")?;
    let video_convert = make("videoconvert")?;
    let video_encoder = make("x264enc")?;
    let video_parser = make("h264parse")?;
    configure_h264_parser(&video_parser);
    let audio_decode = make("decodebin")?;
    let audio_convert = make("audioconvert")?;
    let audio_resample = make("audioresample")?;
    let audio_encoder = make("avenc_aac")?;
    let audio_parser = make("aacparse")?;
    let mux = make("mpegtsmux")?;
    configure_ts_mux(&mux);
    let rate_limiter = make_rate_limiter()?;
    let sink = make("srtsink")?;
    configure_listener(&sink, srt);
    if copy_video {
        pipeline.add_many([
            &source,
            &demux,
            &video_parser,
            &audio_decode,
            &audio_convert,
            &audio_resample,
            &audio_encoder,
            &audio_parser,
            &mux,
            &rate_limiter,
            &sink,
        ])?;
        source.link(&demux)?;
        gst::Element::link_many([&video_parser, &mux, &rate_limiter, &sink])?;
        gst::Element::link_many([
            &audio_convert,
            &audio_resample,
            &audio_encoder,
            &audio_parser,
            &mux,
        ])?;
    } else {
        let audio_caps = make_adts_capsfilter()?;
        pipeline.add_many([
            &source,
            &demux,
            &video_decode,
            &video_convert,
            &video_encoder,
            &video_parser,
            &audio_parser,
            &audio_caps,
            &mux,
            &rate_limiter,
            &sink,
        ])?;
        source.link(&demux)?;
        gst::Element::link_many([
            &video_convert,
            &video_encoder,
            &video_parser,
            &mux,
            &rate_limiter,
            &sink,
        ])?;
        gst::Element::link_many([&audio_parser, &audio_caps, &mux])?;
    }
    let video_target = if copy_video {
        video_parser.static_pad("sink")
    } else {
        video_decode.static_pad("sink")
    }
    .context("video branch missing sink pad")?;
    let audio_target = if copy_video {
        audio_decode.static_pad("sink")
    } else {
        audio_parser.static_pad("sink")
    }
    .context("audio branch missing sink pad")?;
    if copy_video {
        let target = audio_convert
            .static_pad("sink")
            .context("audio convert missing sink")?;
        audio_decode.connect_pad_added(move |_, pad| {
            if !target.is_linked() {
                let _ = pad.link(&target);
            }
        });
    } else {
        let target = video_convert
            .static_pad("sink")
            .context("video convert missing sink")?;
        video_decode.connect_pad_added(move |_, pad| {
            if !target.is_linked() {
                let _ = pad.link(&target);
            }
        });
    }
    demux.connect_pad_added(move |_, pad| {
        let caps = pad.current_caps().or_else(|| Some(pad.query_caps(None)));
        let structure_name = caps
            .as_ref()
            .and_then(|c| c.structure(0))
            .map(|s| s.name().to_string());
        let pad_name = pad.name();
        let is_video = structure_name
            .as_deref()
            .is_some_and(|n| n.starts_with("video/"))
            || pad_name.starts_with("video");
        let is_audio = structure_name
            .as_deref()
            .is_some_and(|n| n.starts_with("audio/"))
            || pad_name.starts_with("audio");
        let target = if is_video {
            &video_target
        } else if is_audio {
            &audio_target
        } else {
            return;
        };
        if !target.is_linked() {
            let _ = pad.link(target);
        }
    });
    Ok(pipeline)
}

pub fn build_h264_aac_copy_pipeline(path: &Path, srt: SrtListenerConfig) -> Result<gst::Pipeline> {
    let pipeline = gst::Pipeline::new();
    let source = make("filesrc")?;
    source.set_property("location", path);
    let demux = make("qtdemux")?;
    let video_queue = make("queue")?;
    let video_parser = make("h264parse")?;
    configure_h264_parser(&video_parser);
    let audio_queue = make("queue")?;
    let audio_parser = make("aacparse")?;
    let audio_caps = make_adts_capsfilter()?;
    let mux = make("mpegtsmux")?;
    configure_ts_mux(&mux);
    let rate_limiter = make_rate_limiter()?;
    let sink = make("srtsink")?;
    configure_listener(&sink, srt);
    pipeline.add_many([
        &source,
        &demux,
        &video_queue,
        &video_parser,
        &audio_queue,
        &audio_parser,
        &audio_caps,
        &mux,
        &rate_limiter,
        &sink,
    ])?;
    source.link(&demux)?;
    gst::Element::link_many([&video_queue, &video_parser, &mux, &rate_limiter, &sink])?;
    gst::Element::link_many([&audio_queue, &audio_parser, &audio_caps, &mux])?;
    let video_sink = video_queue
        .static_pad("sink")
        .context("video queue missing sink pad")?;
    let audio_sink = audio_queue
        .static_pad("sink")
        .context("audio queue missing sink pad")?;
    demux.connect_pad_added(move |_, pad| {
        let caps = pad.current_caps().or_else(|| Some(pad.query_caps(None)));
        let structure_name = caps
            .as_ref()
            .and_then(|c| c.structure(0))
            .map(|s| s.name().to_string());
        let pad_name = pad.name();
        let is_video = structure_name
            .as_deref()
            .is_some_and(|n| n == "video/x-h264")
            || pad_name.starts_with("video");
        let is_audio = structure_name.as_deref().is_some_and(|n| n == "audio/mpeg")
            || pad_name.starts_with("audio");
        let target = if is_video {
            &video_sink
        } else if is_audio {
            &audio_sink
        } else {
            return;
        };
        if !target.is_linked() {
            let _ = pad.link(target);
        }
    });
    Ok(pipeline)
}

fn make(factory: &str) -> Result<gst::Element> {
    gst::ElementFactory::make(factory)
        .build()
        .with_context(|| format!("GStreamer element {factory} is unavailable"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use gstreamer::prelude::*;

    use super::{
        build_h264_aac_copy_pipeline, copy_graph_elements, fallback_graph_elements, listener_uri,
    };
    use crate::streams::SrtListenerConfig;

    #[test]
    fn copy_graph_uses_parsers_and_mpegts_mux() {
        assert_eq!(
            copy_graph_elements(),
            [
                "filesrc",
                "qtdemux",
                "queue",
                "h264parse",
                "queue",
                "aacparse",
                "capsfilter",
                "mpegtsmux",
                "srtsink"
            ]
        );
    }
    #[test]
    fn fallback_graph_has_decode_and_encoders() {
        assert!(fallback_graph_elements().contains(&"x264enc"));
        assert!(fallback_graph_elements().contains(&"avenc_aac"));
    }
    #[test]
    fn listener_uri_uses_the_gstreamer_listener_form() {
        assert_eq!(
            listener_uri(SrtListenerConfig {
                port: 9000,
                latency_ms: 120
            }),
            "srt://:9000?mode=listener&latency=120"
        );
    }

    #[test]
    fn copy_pipeline_decouples_both_demux_branches() {
        gstreamer::init().expect("GStreamer initializes");
        let pipeline = build_h264_aac_copy_pipeline(
            Path::new("/tmp/chronos-test-input.mov"),
            SrtListenerConfig {
                port: 9000,
                latency_ms: 120,
            },
        )
        .expect("copy pipeline builds");

        let queue_count = pipeline
            .iterate_elements()
            .into_iter()
            .map(|element| element.expect("stable pipeline iteration"))
            .filter(|element| {
                element
                    .factory()
                    .is_some_and(|factory| factory.name() == "queue")
            })
            .count();

        assert_eq!(queue_count, 2, "video and audio each need their own queue");
    }

    #[test]
    fn copy_pipeline_forces_adts_audio() {
        gstreamer::init().expect("GStreamer initializes");
        let pipeline = build_h264_aac_copy_pipeline(
            Path::new("/tmp/chronos-test-input.mov"),
            SrtListenerConfig {
                port: 9000,
                latency_ms: 120,
            },
        )
        .expect("copy pipeline builds");

        let capsfilter_count = pipeline
            .iterate_elements()
            .into_iter()
            .map(|element| element.expect("stable pipeline iteration"))
            .filter(|element| {
                element
                    .factory()
                    .is_some_and(|factory| factory.name() == "capsfilter")
            })
            .count();

        assert_eq!(capsfilter_count, 1, "copy pipeline must force ADTS audio");
    }

    #[test]
    fn copy_pipeline_accepts_callers_without_waiting() {
        gstreamer::init().expect("GStreamer initializes");
        let pipeline = build_h264_aac_copy_pipeline(
            Path::new("/tmp/chronos-test-input.mov"),
            SrtListenerConfig {
                port: 9000,
                latency_ms: 120,
            },
        )
        .expect("copy pipeline builds");

        let waits = pipeline
            .iterate_elements()
            .into_iter()
            .map(|element| element.expect("stable pipeline iteration"))
            .find(|element| {
                element
                    .factory()
                    .is_some_and(|factory| factory.name() == "srtsink")
            })
            .expect("srtsink is present")
            .property::<bool>("wait-for-connection");

        assert!(
            !waits,
            "srtsink must stream to callers without blocking for the first one"
        );
    }
}
