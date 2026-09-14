use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use gio::prelude::*;
use gstreamer as gst;
use gstreamer::prelude::*;
use tokio::sync::broadcast;

use crate::{
    domain::{ProcessingMode, SrtClientAddress, StreamEvent, StreamState},
    gst_runtime::{build_h264_aac_copy_pipeline, build_hybrid_pipeline, build_transcode_pipeline},
    streams::SrtListenerConfig,
};

#[derive(Clone)]
pub struct Supervisor {
    inner: Arc<Mutex<HashMap<String, ManagedStream>>>,
    publication: Arc<Mutex<()>>,
    events: broadcast::Sender<StreamEvent>,
}
struct ManagedStream {
    pipeline: gst::Pipeline,
    state: StreamState,
    /// Tracks how many times we have looped (informational only, no cap).
    restarts: u64,
    config: SrtListenerConfig,
    mode: ProcessingMode,
    clients: Vec<SrtClientAddress>,
}

fn event_from_managed(
    id: &str,
    managed: &ManagedStream,
    state: StreamState,
    detail: Option<String>,
) -> StreamEvent {
    StreamEvent {
        stream_id: id.into(),
        state,
        detail,
        port: Some(managed.config.port),
        latency_ms: Some(managed.config.latency_ms),
        mode: Some(managed.mode.clone()),
        loop_count: Some(managed.restarts),
        clients: managed.clients.clone(),
    }
}

fn socket_address_to_client(address: &gio::SocketAddress) -> Option<SrtClientAddress> {
    let address = address.clone().downcast::<gio::InetSocketAddress>().ok()?;
    Some(SrtClientAddress {
        ip: address.address().to_string().into(),
        port: address.port(),
    })
}

fn find_srt_sink(pipeline: &gst::Pipeline) -> Option<gst::Element> {
    pipeline
        .iterate_elements()
        .into_iter()
        .filter_map(Result::ok)
        .find(|element| {
            element
                .factory()
                .is_some_and(|factory| factory.name() == "srtsink")
        })
}

impl Supervisor {
    pub fn new() -> Self {
        let (events, _) = broadcast::channel(128);
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            publication: Arc::new(Mutex::new(())),
            events,
        }
    }
    pub fn subscribe(&self) -> broadcast::Receiver<StreamEvent> {
        self.events.subscribe()
    }
    pub fn states(&self) -> Vec<StreamEvent> {
        self.inner
            .lock()
            .map(|streams| {
                streams
                    .iter()
                    .map(|(id, managed)| event_from_managed(id, managed, managed.state, None))
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn start_copy(&self, id: String, path: &Path, config: SrtListenerConfig) -> Result<()> {
        self.start(
            id,
            build_h264_aac_copy_pipeline(path, config)?,
            config,
            ProcessingMode::RemuxCopy,
        )
    }
    pub fn start_transcode(
        &self,
        id: String,
        path: &Path,
        config: SrtListenerConfig,
    ) -> Result<()> {
        self.start(
            id,
            build_transcode_pipeline(path, config)?,
            config,
            ProcessingMode::FullTranscode,
        )
    }
    pub fn start_hybrid(
        &self,
        id: String,
        path: &Path,
        config: SrtListenerConfig,
        mode: ProcessingMode,
    ) -> Result<()> {
        let copy_video = matches!(mode, ProcessingMode::CopyVideoEncodeAudio);
        self.start(
            id,
            build_hybrid_pipeline(path, config, copy_video)?,
            config,
            mode,
        )
    }
    fn start(
        &self,
        id: String,
        pipeline: gst::Pipeline,
        config: SrtListenerConfig,
        mode: ProcessingMode,
    ) -> Result<()> {
        self.start_with(
            id,
            pipeline,
            config,
            mode,
            || {},
            |pipeline| {
                pipeline.set_state(gst::State::Playing)?;
                Ok(())
            },
            |supervisor, id, pipeline| {
                // Re-acquire the bus after each restart so monitoring survives
                // NULL → PLAYING cycles.
                std::thread::spawn(move || supervisor.monitor_loop(&id, &pipeline));
            },
        )
    }

    fn start_with<A, P, M>(
        &self,
        id: String,
        pipeline: gst::Pipeline,
        config: SrtListenerConfig,
        mode: ProcessingMode,
        after_starting: A,
        set_playing: P,
        start_monitor: M,
    ) -> Result<()>
    where
        A: FnOnce(),
        P: FnOnce(&gst::Pipeline) -> Result<()>,
        M: FnOnce(Supervisor, String, gst::Pipeline),
    {
        self.register_start(id.clone(), pipeline.clone(), config, mode)?;
        after_starting();
        if let Err(error) = self.attach_srt_client_handlers(&id, &pipeline) {
            if self.rollback_start(&id, &pipeline, StreamState::Starting) {
                self.set_stale_pipeline_null(&id, &pipeline);
            }
            return Err(error);
        }
        if !self.prepare_start_for_playback(&id, &pipeline) {
            let stop_owns_pipeline = self
                .inner
                .lock()
                .ok()
                .and_then(|streams| {
                    streams.get(&id).map(|managed| {
                        managed.pipeline == pipeline && managed.state == StreamState::Stopping
                    })
                })
                .unwrap_or(false);
            if !stop_owns_pipeline {
                self.set_stale_pipeline_null(&id, &pipeline);
            }
            anyhow::bail!("Stream {id} start is no longer current")
        }
        if let Err(error) = set_playing(&pipeline) {
            if self.rollback_start(&id, &pipeline, StreamState::WaitingForCaller) {
                self.set_stale_pipeline_null(&id, &pipeline);
            }
            return Err(error);
        }
        self.finalize_start(&id, &pipeline, start_monitor)
    }

    fn register_start(
        &self,
        id: String,
        pipeline: gst::Pipeline,
        config: SrtListenerConfig,
        mode: ProcessingMode,
    ) -> Result<()> {
        let registered = {
            let _publication = self
                .publication
                .lock()
                .expect("supervisor publication mutex poisoned");
            let mut streams = self.inner.lock().expect("supervisor mutex poisoned");
            if streams.contains_key(&id) {
                false
            } else {
                streams.insert(
                    id.clone(),
                    ManagedStream {
                        pipeline: pipeline.clone(),
                        state: StreamState::Starting,
                        restarts: 0,
                        config,
                        mode,
                        clients: Vec::new(),
                    },
                );
                let managed = streams.get(&id).expect("newly registered stream missing");
                let event = event_from_managed(&id, managed, StreamState::Starting, None);
                let _ = self.events.send(event);
                true
            }
        };
        if registered {
            return Ok(());
        }

        if let Err(cleanup_error) = pipeline.set_state(gst::State::Null) {
            tracing::warn!(
                stream_id = id,
                ?cleanup_error,
                "Failed to stop rejected pipeline"
            );
        }
        anyhow::bail!("Stream {id} is already managed")
    }

    fn prepare_start_for_playback(&self, id: &str, pipeline: &gst::Pipeline) -> bool {
        let _publication = self
            .publication
            .lock()
            .expect("supervisor publication mutex poisoned");
        let mut streams = self.inner.lock().expect("supervisor mutex poisoned");
        let Some(managed) = streams.get_mut(id) else {
            return false;
        };
        if managed.pipeline != *pipeline || managed.state != StreamState::Starting {
            return false;
        }
        managed.state = StreamState::WaitingForCaller;
        true
    }

    fn rollback_start(
        &self,
        id: &str,
        pipeline: &gst::Pipeline,
        expected_state: StreamState,
    ) -> bool {
        let _publication = self
            .publication
            .lock()
            .expect("supervisor publication mutex poisoned");
        let mut streams = self.inner.lock().expect("supervisor mutex poisoned");
        let Some(managed) = streams.get(id) else {
            return true;
        };
        if managed.pipeline != *pipeline {
            return true;
        }
        if managed.state == StreamState::Stopping {
            return false;
        }
        if managed.state == expected_state {
            streams.remove(id);
        }
        true
    }

    fn set_stale_pipeline_null(&self, id: &str, pipeline: &gst::Pipeline) {
        if let Err(cleanup_error) = pipeline.set_state(gst::State::Null) {
            tracing::warn!(
                stream_id = id,
                ?cleanup_error,
                "Failed to stop stale pipeline"
            );
        }
    }

    fn finalize_start<F>(&self, id: &str, pipeline: &gst::Pipeline, start_monitor: F) -> Result<()>
    where
        F: FnOnce(Supervisor, String, gst::Pipeline),
    {
        let finalized = {
            let _publication = self
                .publication
                .lock()
                .expect("supervisor publication mutex poisoned");
            let event = {
                let streams = self.inner.lock().expect("supervisor mutex poisoned");
                streams.get(id).and_then(|managed| {
                    (managed.pipeline == *pipeline
                        && managed.state == StreamState::WaitingForCaller)
                        .then(|| {
                            event_from_managed(id, managed, StreamState::WaitingForCaller, None)
                        })
                })
            };
            if let Some(event) = event {
                let _ = self.events.send(event);
                true
            } else {
                false
            }
        };

        if !finalized {
            if let Err(cleanup_error) = pipeline.set_state(gst::State::Null) {
                tracing::warn!(
                    stream_id = id,
                    ?cleanup_error,
                    "Failed to stop stale pipeline"
                );
            }
            anyhow::bail!("Stream {id} start is no longer current")
        }

        start_monitor(self.clone(), id.to_owned(), pipeline.clone());
        Ok(())
    }

    fn attach_srt_client_handlers(&self, id: &str, pipeline: &gst::Pipeline) -> Result<()> {
        let sink = find_srt_sink(pipeline)
            .ok_or_else(|| anyhow::anyhow!("Stream {id} pipeline has no srtsink"))?;
        for signal in ["caller-added", "caller-removed"] {
            let id = id.to_owned();
            // Avoid an ownership cycle: inner -> pipeline -> sink -> callback -> inner.
            let inner = Arc::downgrade(&self.inner);
            let publication = self.publication.clone();
            let events = self.events.clone();
            sink.connect(signal, false, move |values| {
                let Some(address) = values
                    .get(2)
                    .and_then(|value| value.get::<gio::SocketAddress>().ok())
                else {
                    tracing::warn!(
                        stream_id = id,
                        signal,
                        "Ignoring caller signal with malformed socket address"
                    );
                    return None;
                };
                let Some(client) = socket_address_to_client(&address) else {
                    tracing::warn!(
                        stream_id = id,
                        signal,
                        "Ignoring caller signal with unsupported socket address"
                    );
                    return None;
                };
                let Some(inner) = inner.upgrade() else {
                    return None;
                };
                let supervisor = Supervisor {
                    inner,
                    publication: publication.clone(),
                    events: events.clone(),
                };
                if signal == "caller-added" {
                    supervisor.record_client_added(&id, client);
                } else {
                    supervisor.record_client_removed(&id, &client);
                }
                None
            });
        }
        Ok(())
    }

    /// Bus monitor loop that survives pipeline restarts.
    ///
    /// After each EOS the pipeline is restarted (NULL → PLAYING). The old bus
    /// reference is flushed by the NULL transition, so we re-acquire the bus
    /// from the pipeline after every restart to keep receiving messages.
    fn monitor_bus(&self, id: &str, pipeline: &gst::Pipeline) -> Option<gst::Bus> {
        let is_current = {
            let streams = self.inner.lock().ok()?;
            let managed = streams.get(id)?;
            managed.pipeline == *pipeline
                && matches!(
                    managed.state,
                    StreamState::WaitingForCaller | StreamState::Running | StreamState::Looping
                )
        };
        is_current.then(|| pipeline.bus()).flatten()
    }

    fn monitor_loop(&self, id: &str, pipeline: &gst::Pipeline) {
        loop {
            // Fetch the bus only while this exact pipeline still owns the ID.
            let bus = self.monitor_bus(id, pipeline);
            let Some(bus) = bus else { return };

            // Drain this bus until EOS, error, or the pipeline is removed.
            loop {
                let Some(message) = bus.timed_pop(gst::ClockTime::from_seconds(1)) else {
                    // Timeout — check if the stream still exists.
                    if self.monitor_bus(id, pipeline).is_none() {
                        return;
                    }
                    continue;
                };
                match message.view() {
                    gst::MessageView::StateChanged(sc) => {
                        // Detect when the pipeline itself (not a sub-element)
                        // transitions to Playing. We compare object names since
                        // comparing GObject pointers across lock boundaries is
                        // unsafe and complex.
                        let src_name = message.src().map(|o| o.name().to_string());
                        let pipeline_name = Some(pipeline.name().to_string());
                        let is_pipeline_msg = src_name.is_some() && src_name == pipeline_name;
                        if is_pipeline_msg && sc.current() == gst::State::Playing {
                            let state_changed = if let Ok(mut streams) = self.inner.lock() {
                                if let Some(managed) = streams.get_mut(id) {
                                    if managed.pipeline == *pipeline
                                        && managed.state == StreamState::WaitingForCaller
                                    {
                                        managed.state = StreamState::Running;
                                        true
                                    } else {
                                        false
                                    }
                                } else {
                                    false
                                }
                            } else {
                                false
                            };
                            if state_changed {
                                self.emit_if_current_state(
                                    id,
                                    pipeline,
                                    StreamState::Running,
                                    None,
                                );
                            }
                        }
                    }
                    gst::MessageView::Eos(..) => {
                        // EOS: record and restart, then break to re-acquire the bus.
                        self.record_eos(id, pipeline);
                        break;
                    }
                    gst::MessageView::Error(error) => {
                        self.record_error(id, pipeline, error.error().to_string());
                        return; // Fatal — stop monitoring.
                    }
                    _ => {}
                }
            }

            // If the stream was removed (stopped), exit.
            if self.monitor_bus(id, pipeline).is_none() {
                return;
            }
        }
    }
    pub fn stop(&self, id: &str) -> Result<()> {
        self.stop_with(id, |pipeline| {
            pipeline.set_state(gst::State::Null)?;
            Ok(())
        })
    }

    fn stop_with<F>(&self, id: &str, set_null: F) -> Result<()>
    where
        F: FnOnce(&gst::Pipeline) -> Result<()>,
    {
        let pipeline = {
            let _publication = self
                .publication
                .lock()
                .expect("supervisor publication mutex poisoned");
            let (event, pipeline) = {
                let mut streams = self.inner.lock().expect("supervisor mutex poisoned");
                if let Some(managed) = streams.get_mut(id) {
                    managed.state = StreamState::Stopping;
                    (
                        event_from_managed(id, managed, StreamState::Stopping, None),
                        Some(managed.pipeline.clone()),
                    )
                } else {
                    (
                        StreamEvent {
                            stream_id: id.into(),
                            state: StreamState::Stopping,
                            detail: None,
                            port: None,
                            latency_ms: None,
                            mode: None,
                            loop_count: None,
                            clients: Vec::new(),
                        },
                        None,
                    )
                }
            };
            let _ = self.events.send(event);
            pipeline
        };

        let Some(pipeline) = pipeline else {
            let _publication = self
                .publication
                .lock()
                .expect("supervisor publication mutex poisoned");
            let _ = self.events.send(StreamEvent {
                stream_id: id.into(),
                state: StreamState::Stopped,
                detail: None,
                port: None,
                latency_ms: None,
                mode: None,
                loop_count: None,
                clients: Vec::new(),
            });
            return Ok(());
        };

        match set_null(&pipeline) {
            Ok(()) => {
                let _publication = self
                    .publication
                    .lock()
                    .expect("supervisor publication mutex poisoned");
                let event = {
                    let mut streams = self.inner.lock().expect("supervisor mutex poisoned");
                    let is_current = streams
                        .get(id)
                        .is_some_and(|managed| managed.pipeline == pipeline);
                    is_current
                        .then(|| streams.remove(id))
                        .flatten()
                        .map(|_| StreamEvent {
                            stream_id: id.into(),
                            state: StreamState::Stopped,
                            detail: None,
                            port: None,
                            latency_ms: None,
                            mode: None,
                            loop_count: None,
                            clients: Vec::new(),
                        })
                };
                if let Some(event) = event {
                    let _ = self.events.send(event);
                }
                Ok(())
            }
            Err(error) => {
                let detail = format!("Failed to stop pipeline: {error}");
                let _publication = self
                    .publication
                    .lock()
                    .expect("supervisor publication mutex poisoned");
                let event = {
                    let mut streams = self.inner.lock().expect("supervisor mutex poisoned");
                    streams.get_mut(id).and_then(|managed| {
                        (managed.pipeline == pipeline && managed.state == StreamState::Stopping)
                            .then(|| {
                                managed.state = StreamState::Failed;
                                event_from_managed(id, managed, StreamState::Failed, Some(detail))
                            })
                    })
                };
                tracing::error!(stream_id = id, %error, "Failed to stop GStreamer pipeline");
                if let Some(event) = event {
                    let _ = self.events.send(event);
                }
                Err(error)
            }
        }
    }
    fn record_eos(&self, id: &str, expected_pipeline: &gst::Pipeline) {
        self.record_eos_with(
            id,
            expected_pipeline,
            |pipeline| {
                pipeline.set_state(gst::State::Null)?;
                Ok(())
            },
            |pipeline| {
                pipeline.set_state(gst::State::Playing)?;
                Ok(())
            },
        );
    }

    fn record_eos_with<N, P>(
        &self,
        id: &str,
        expected_pipeline: &gst::Pipeline,
        set_null: N,
        set_playing: P,
    ) where
        N: FnOnce(&gst::Pipeline) -> Result<()>,
        P: FnOnce(&gst::Pipeline) -> Result<()>,
    {
        // Restart the pipeline from scratch on every EOS so the file loops
        // indefinitely. A seek_simple is unreliable once srtsink holds an
        // open UDP socket: the flush propagates into the sink and can cause
        // VLC to lose the connection. Transitioning NULL → PLAYING re-opens
        // the file while the SRT listener port stays bound.
        let pipeline = {
            let mut streams = match self.inner.lock() {
                Ok(s) => s,
                Err(_) => return,
            };
            let Some(managed) = streams.get_mut(id) else {
                return;
            };
            if managed.pipeline != *expected_pipeline
                || !matches!(managed.state, StreamState::Running | StreamState::Looping)
            {
                return;
            }
            managed.restarts = managed.restarts.saturating_add(1);
            managed.state = StreamState::Looping;
            managed.pipeline.clone()
        };
        // Stop and immediately restart to loop the file.
        if let Err(error) = set_null(&pipeline) {
            let detail = format!("EOS restart failed to stop pipeline: {error}");
            tracing::error!(stream_id = id, %error, "EOS: failed to set pipeline to Null");
            self.record_restart_failure(id, expected_pipeline, detail);
            return;
        }
        let still_current = self
            .inner
            .lock()
            .ok()
            .and_then(|streams| {
                streams.get(id).map(|managed| {
                    managed.pipeline == *expected_pipeline && managed.state == StreamState::Looping
                })
            })
            .unwrap_or(false);
        if !still_current {
            return;
        }

        if let Err(error) = set_playing(&pipeline) {
            let detail = format!("EOS restart failed to start pipeline: {error}");
            tracing::error!(stream_id = id, %error, "EOS: failed to restart pipeline");
            self.record_restart_failure(id, expected_pipeline, detail);
            if let Err(cleanup_error) = pipeline.set_state(gst::State::Null) {
                tracing::warn!(
                    stream_id = id,
                    ?cleanup_error,
                    "EOS: failed to stop unsuccessful pipeline"
                );
            }
            return;
        }

        let restarted = {
            let _publication = self
                .publication
                .lock()
                .expect("supervisor publication mutex poisoned");
            let event = {
                let streams = self.inner.lock().expect("supervisor mutex poisoned");
                streams.get(id).and_then(|managed| {
                    (managed.pipeline == *expected_pipeline
                        && managed.state == StreamState::Looping)
                        .then(|| event_from_managed(id, managed, StreamState::Looping, None))
                })
            };
            if let Some(event) = event {
                let _ = self.events.send(event);
                true
            } else {
                false
            }
        };
        if !restarted {
            if let Err(cleanup_error) = pipeline.set_state(gst::State::Null) {
                tracing::warn!(
                    stream_id = id,
                    ?cleanup_error,
                    "EOS: failed to stop stale pipeline"
                );
            }
        }
    }

    fn record_restart_failure(&self, id: &str, expected_pipeline: &gst::Pipeline, detail: String) {
        let _publication = self
            .publication
            .lock()
            .expect("supervisor publication mutex poisoned");
        let event = {
            let mut streams = self.inner.lock().expect("supervisor mutex poisoned");
            streams.get_mut(id).and_then(|managed| {
                (managed.pipeline == *expected_pipeline && managed.state == StreamState::Looping)
                    .then(|| {
                        managed.state = StreamState::Failed;
                        event_from_managed(id, managed, StreamState::Failed, Some(detail))
                    })
            })
        };
        if let Some(event) = event {
            let _ = self.events.send(event);
        }
    }
    fn record_error(&self, id: &str, expected_pipeline: &gst::Pipeline, detail: String) {
        if let Ok(mut streams) = self.inner.lock()
            && let Some(managed) = streams.get_mut(id)
            && managed.pipeline == *expected_pipeline
            && managed.state != StreamState::Stopping
        {
            managed.state = StreamState::Failed;
        }
        tracing::error!(stream_id = id, %detail, "GStreamer pipeline failed");
        self.emit_if_current_state(id, expected_pipeline, StreamState::Failed, Some(detail));
    }
    fn record_client_added(&self, id: &str, client: SrtClientAddress) {
        let _publication = match self.publication.lock() {
            Ok(publication) => publication,
            Err(_) => return,
        };
        let event = {
            let mut streams = match self.inner.lock() {
                Ok(streams) => streams,
                Err(_) => return,
            };
            let Some(managed) = streams.get_mut(id) else {
                return;
            };
            if managed.clients.contains(&client) {
                return;
            }
            managed.clients.push(client);
            event_from_managed(id, managed, managed.state, None)
        };
        let _ = self.events.send(event);
    }
    fn record_client_removed(&self, id: &str, client: &SrtClientAddress) {
        let _publication = match self.publication.lock() {
            Ok(publication) => publication,
            Err(_) => return,
        };
        let event = {
            let mut streams = match self.inner.lock() {
                Ok(streams) => streams,
                Err(_) => return,
            };
            let Some(managed) = streams.get_mut(id) else {
                return;
            };
            let previous_len = managed.clients.len();
            managed.clients.retain(|existing| existing != client);
            if managed.clients.len() == previous_len {
                return;
            }
            event_from_managed(id, managed, managed.state, None)
        };
        let _ = self.events.send(event);
    }
    fn emit_if_current_state(
        &self,
        id: &str,
        pipeline: &gst::Pipeline,
        state: StreamState,
        detail: Option<String>,
    ) {
        let _publication = match self.publication.lock() {
            Ok(publication) => publication,
            Err(_) => return,
        };
        let event = {
            let streams = match self.inner.lock() {
                Ok(streams) => streams,
                Err(_) => return,
            };
            streams.get(id).and_then(|managed| {
                (managed.pipeline == *pipeline && managed.state == state)
                    .then(|| event_from_managed(id, managed, state, detail))
            })
        };
        if let Some(event) = event {
            let _ = self.events.send(event);
        }
    }
    #[cfg(test)]
    fn emit(&self, id: &str, state: StreamState, detail: Option<String>) {
        let _publication = match self.publication.lock() {
            Ok(publication) => publication,
            Err(_) => return,
        };
        let event = {
            let streams = self.inner.lock().ok();
            streams
                .as_ref()
                .and_then(|streams| streams.get(id))
                .map(|managed| event_from_managed(id, managed, state, detail.clone()))
                .unwrap_or_else(|| StreamEvent {
                    stream_id: id.into(),
                    state,
                    detail,
                    port: None,
                    latency_ms: None,
                    mode: None,
                    loop_count: None,
                    clients: Vec::new(),
                })
        };
        let _ = self.events.send(event);
    }
}

#[cfg(test)]
mod tests {
    use gio::prelude::*;
    use std::{
        sync::{
            Arc, Mutex, TryLockError,
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        thread,
        time::{Duration, Instant},
    };

    use gstreamer as gst;
    use gstreamer::prelude::*;
    use tokio::sync::broadcast::error::TryRecvError;

    use super::{ManagedStream, Supervisor, find_srt_sink, socket_address_to_client};
    use crate::{
        domain::{ProcessingMode, SrtClientAddress, StreamState},
        streams::SrtListenerConfig,
    };

    fn supervisor_with_test_stream(id: &str) -> Supervisor {
        gst::init().unwrap();
        let supervisor = Supervisor::new();
        supervisor
            .inner
            .lock()
            .expect("supervisor mutex poisoned")
            .insert(
                id.into(),
                ManagedStream {
                    pipeline: gst::Pipeline::new(),
                    state: StreamState::Running,
                    restarts: 0,
                    config: SrtListenerConfig::new(9000, 120).unwrap(),
                    mode: ProcessingMode::RemuxCopy,
                    clients: Vec::new(),
                },
            );
        supervisor
    }

    #[test]
    fn converts_inet_socket_addresses() {
        for (ip, port) in [("192.0.2.10", 54321), ("2001:db8::10", 54322)] {
            let inet = gio::InetAddress::from_string(ip).unwrap();
            let address: gio::SocketAddress = gio::InetSocketAddress::new(&inet, port).upcast();
            assert_eq!(
                socket_address_to_client(&address),
                Some(SrtClientAddress {
                    ip: ip.into(),
                    port
                })
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn ignores_non_ip_socket_addresses() {
        let address = gio::UnixSocketAddress::new(std::path::Path::new("/tmp/chronos-test"));
        assert_eq!(socket_address_to_client(&address.upcast()), None);
    }

    #[test]
    fn finds_srt_sink_by_factory_not_element_name() {
        gst::init().unwrap();
        let pipeline = gst::Pipeline::new();
        let impostor = gst::ElementFactory::make("fakesink")
            .name("srtsink")
            .build()
            .unwrap();
        pipeline.add(&impostor).unwrap();
        assert!(find_srt_sink(&pipeline).is_none());
        let sink = gst::ElementFactory::make("srtsink")
            .name("output")
            .build()
            .unwrap();
        pipeline.add(&sink).unwrap();
        assert_eq!(find_srt_sink(&pipeline), Some(sink));
    }

    #[test]
    fn rejects_start_without_srt_sink() {
        gst::init().unwrap();
        let supervisor = Supervisor::new();
        let mut events = supervisor.subscribe();
        let pipeline = gst::Pipeline::new();
        let error = supervisor
            .start(
                "feed".into(),
                pipeline.clone(),
                SrtListenerConfig::new(9000, 120).unwrap(),
                ProcessingMode::RemuxCopy,
            )
            .unwrap_err();
        assert!(error.to_string().contains("srtsink"));
        assert!(supervisor.states().is_empty());
        assert_eq!(pipeline.current_state(), gst::State::Null);
        assert_eq!(events.try_recv().unwrap().state, StreamState::Starting);
        assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn caller_signals_update_clients_without_playback() {
        let supervisor = supervisor_with_test_stream("feed");
        let pipeline = supervisor.inner.lock().unwrap()["feed"].pipeline.clone();
        let sink = gst::ElementFactory::make("srtsink").build().unwrap();
        pipeline.add(&sink).unwrap();
        supervisor
            .attach_srt_client_handlers("feed", &pipeline)
            .unwrap();
        let mut events = supervisor.subscribe();
        let inet = gio::InetAddress::from_string("2001:db8::10").unwrap();
        let address: gio::SocketAddress = gio::InetSocketAddress::new(&inet, 54321).upcast();
        sink.emit_by_name::<()>("caller-added", &[&1i32, &address]);
        assert_eq!(
            events.try_recv().unwrap().clients,
            vec![SrtClientAddress {
                ip: "2001:db8::10".into(),
                port: 54321,
            }]
        );
        sink.emit_by_name::<()>("caller-removed", &[&1i32, &address]);
        assert!(events.try_recv().unwrap().clients.is_empty());
        let missing: Option<gio::SocketAddress> = None;
        sink.emit_by_name::<()>("caller-added", &[&1i32, &missing]);
        sink.emit_by_name::<()>("caller-removed", &[&1i32, &missing]);
        #[cfg(unix)]
        {
            let unsupported: gio::SocketAddress =
                gio::UnixSocketAddress::new(std::path::Path::new("/tmp/chronos-test")).upcast();
            sink.emit_by_name::<()>("caller-added", &[&1i32, &unsupported]);
            sink.emit_by_name::<()>("caller-removed", &[&1i32, &unsupported]);
        }
        assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));
        assert!(supervisor.states()[0].clients.is_empty());
        assert_eq!(pipeline.current_state(), gst::State::Null);
    }

    #[test]
    fn rolls_back_failed_playback_without_opening_listener() {
        gst::init().unwrap();
        let supervisor = Supervisor::new();
        let pipeline = gst::Pipeline::new();
        let sink = gst::ElementFactory::make("srtsink").build().unwrap();
        // Keep the sink from reaching Paused/Playing so the test cannot bind a socket.
        sink.set_locked_state(true);
        let directory = tempfile::tempdir().unwrap();
        let source = gst::ElementFactory::make("filesrc")
            .property(
                "location",
                directory.path().join("missing.ts").to_str().unwrap(),
            )
            .build()
            .unwrap();
        pipeline.add_many([&source, &sink]).unwrap();
        source.link(&sink).unwrap();
        let result = supervisor.start(
            "feed".into(),
            pipeline.clone(),
            SrtListenerConfig::new(9000, 120).unwrap(),
            ProcessingMode::RemuxCopy,
        );
        let pipeline_state = pipeline.current_state();

        // A locked child does not follow its parent back to Null. Unlock it and
        // clean it up explicitly before the final reference is dropped.
        sink.set_locked_state(false);
        sink.set_state(gst::State::Null).unwrap();

        assert!(result.is_err());
        assert!(supervisor.states().is_empty());
        assert_eq!(pipeline_state, gst::State::Null);
    }

    #[test]
    fn stop_after_starting_cancels_before_playback() {
        gst::init().unwrap();
        let supervisor = Supervisor::new();
        let pipeline = gst::Pipeline::new();
        pipeline
            .add(&gst::ElementFactory::make("srtsink").build().unwrap())
            .unwrap();
        let mut events = supervisor.subscribe();
        let stopping_supervisor = supervisor.clone();
        let playing_calls = Arc::new(AtomicUsize::new(0));
        let observed_playing = playing_calls.clone();
        let monitor_starts = Arc::new(AtomicUsize::new(0));
        let observed_monitors = monitor_starts.clone();

        let error = supervisor
            .start_with(
                "feed".into(),
                pipeline.clone(),
                SrtListenerConfig::new(9000, 120).unwrap(),
                ProcessingMode::RemuxCopy,
                move || stopping_supervisor.stop("feed").unwrap(),
                move |_| {
                    observed_playing.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
                move |_, _, _| {
                    observed_monitors.fetch_add(1, Ordering::SeqCst);
                },
            )
            .unwrap_err();

        assert!(error.to_string().contains("no longer current"));
        assert_eq!(playing_calls.load(Ordering::SeqCst), 0);
        assert_eq!(monitor_starts.load(Ordering::SeqCst), 0);
        assert_eq!(pipeline.current_state(), gst::State::Null);
        assert!(supervisor.states().is_empty());
        let states: Vec<_> = std::iter::from_fn(|| events.try_recv().ok())
            .map(|event| event.state)
            .collect();
        assert_eq!(
            states,
            vec![
                StreamState::Starting,
                StreamState::Stopping,
                StreamState::Stopped
            ]
        );
    }

    #[test]
    fn playback_failure_does_not_steal_entry_from_stop() {
        gst::init().unwrap();
        let supervisor = Supervisor::new();
        let pipeline = gst::Pipeline::new();
        pipeline
            .add(&gst::ElementFactory::make("srtsink").build().unwrap())
            .unwrap();
        let mut events = supervisor.subscribe();
        let (stop_entered_tx, stop_entered_rx) = mpsc::channel();
        let (release_stop_tx, release_stop_rx) = mpsc::channel();
        let stop_supervisor = supervisor.clone();
        let stop_thread = Arc::new(Mutex::new(None));
        let recorded_stop_thread = stop_thread.clone();
        let monitor_starts = Arc::new(AtomicUsize::new(0));
        let observed_monitors = monitor_starts.clone();

        let error = supervisor
            .start_with(
                "feed".into(),
                pipeline.clone(),
                SrtListenerConfig::new(9000, 120).unwrap(),
                ProcessingMode::RemuxCopy,
                || {},
                move |_| {
                    let handle = thread::spawn(move || {
                        stop_supervisor.stop_with("feed", |_| {
                            stop_entered_tx.send(()).unwrap();
                            release_stop_rx.recv().unwrap();
                            Ok(())
                        })
                    });
                    *recorded_stop_thread.lock().unwrap() = Some(handle);
                    stop_entered_rx.recv().unwrap();
                    anyhow::bail!("injected Playing failure")
                },
                move |_, _, _| {
                    observed_monitors.fetch_add(1, Ordering::SeqCst);
                },
            )
            .unwrap_err();

        assert!(error.to_string().contains("injected Playing failure"));
        assert_eq!(supervisor.states()[0].state, StreamState::Stopping);
        assert_eq!(monitor_starts.load(Ordering::SeqCst), 0);
        release_stop_tx.send(()).unwrap();
        stop_thread
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .join()
            .unwrap()
            .unwrap();
        assert!(supervisor.states().is_empty());
        assert_eq!(pipeline.current_state(), gst::State::Null);
        let states: Vec<_> = std::iter::from_fn(|| events.try_recv().ok())
            .map(|event| event.state)
            .collect();
        assert_eq!(
            states,
            vec![
                StreamState::Starting,
                StreamState::Stopping,
                StreamState::Stopped
            ]
        );
    }

    #[test]
    fn stopped_start_is_not_finalized_or_monitored() {
        gst::init().unwrap();
        let supervisor = supervisor_with_test_stream("feed");
        let pipeline = supervisor.inner.lock().unwrap()["feed"].pipeline.clone();
        pipeline.set_state(gst::State::Ready).unwrap();
        let mut events = supervisor.subscribe();

        supervisor.stop("feed").unwrap();

        let monitor_starts = Arc::new(AtomicUsize::new(0));
        let observed_starts = monitor_starts.clone();
        let error = supervisor
            .finalize_start("feed", &pipeline, move |_, _, _| {
                observed_starts.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap_err();

        assert!(error.to_string().contains("no longer current"));
        assert_eq!(pipeline.current_state(), gst::State::Null);
        assert_eq!(monitor_starts.load(Ordering::SeqCst), 0);
        let states: Vec<_> = std::iter::from_fn(|| events.try_recv().ok())
            .map(|event| event.state)
            .collect();
        assert_eq!(states, vec![StreamState::Stopping, StreamState::Stopped]);
    }

    #[test]
    fn stopped_event_always_clears_clients() {
        let supervisor = supervisor_with_test_stream("feed");
        supervisor.record_client_added(
            "feed",
            SrtClientAddress {
                ip: "192.0.2.10".into(),
                port: 54321,
            },
        );
        let mut events = supervisor.subscribe();

        supervisor.stop("feed").unwrap();

        let stopping = events.try_recv().unwrap();
        assert_eq!(stopping.state, StreamState::Stopping);
        assert_eq!(stopping.clients.len(), 1);
        let stopped = events.try_recv().unwrap();
        assert_eq!(stopped.state, StreamState::Stopped);
        assert_eq!(stopped.port, None);
        assert_eq!(stopped.latency_ms, None);
        assert_eq!(stopped.mode, None);
        assert_eq!(stopped.loop_count, None);
        assert!(stopped.clients.is_empty());
    }

    #[test]
    fn failed_stop_retains_managed_pipeline_and_reports_failure() {
        let supervisor = supervisor_with_test_stream("feed");
        let pipeline = supervisor.inner.lock().unwrap()["feed"].pipeline.clone();
        supervisor.record_client_added(
            "feed",
            SrtClientAddress {
                ip: "192.0.2.10".into(),
                port: 54321,
            },
        );
        let mut events = supervisor.subscribe();

        let error = supervisor
            .stop_with("feed", |_| anyhow::bail!("injected Null failure"))
            .unwrap_err();

        assert!(error.to_string().contains("injected Null failure"));
        let states = supervisor.states();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].state, StreamState::Failed);
        assert_eq!(states[0].clients.len(), 1);
        assert_eq!(supervisor.inner.lock().unwrap()["feed"].pipeline, pipeline);
        let stopping = events.try_recv().unwrap();
        assert_eq!(stopping.state, StreamState::Stopping);
        let failed = events.try_recv().unwrap();
        assert_eq!(failed.state, StreamState::Failed);
        assert!(failed.detail.unwrap().contains("injected Null failure"));
        assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn competing_start_cannot_replace_managed_pipeline() {
        gst::init().unwrap();
        let supervisor = supervisor_with_test_stream("feed");
        let original = supervisor.inner.lock().unwrap()["feed"].pipeline.clone();
        let competing = gst::Pipeline::new();
        competing.set_state(gst::State::Ready).unwrap();

        let error = supervisor
            .register_start(
                "feed".into(),
                competing.clone(),
                SrtListenerConfig::new(9001, 120).unwrap(),
                ProcessingMode::RemuxCopy,
            )
            .unwrap_err();

        assert!(error.to_string().contains("already managed"));
        assert_eq!(competing.current_state(), gst::State::Null);
        assert_eq!(supervisor.inner.lock().unwrap()["feed"].pipeline, original);
    }

    #[test]
    fn monitor_bus_is_scoped_to_pipeline_identity() {
        gst::init().unwrap();
        let supervisor = supervisor_with_test_stream("feed");
        let original = supervisor.inner.lock().unwrap()["feed"].pipeline.clone();
        assert!(supervisor.monitor_bus("feed", &original).is_some());

        let replacement = gst::Pipeline::new();
        supervisor.inner.lock().unwrap().insert(
            "feed".into(),
            ManagedStream {
                pipeline: replacement.clone(),
                state: StreamState::WaitingForCaller,
                restarts: 0,
                config: SrtListenerConfig::new(9001, 120).unwrap(),
                mode: ProcessingMode::RemuxCopy,
                clients: Vec::new(),
            },
        );

        assert!(supervisor.monitor_bus("feed", &original).is_none());
        assert!(supervisor.monitor_bus("feed", &replacement).is_some());
    }

    #[test]
    fn stopped_eos_restart_is_nulled_without_looping_event() {
        gst::init().unwrap();
        let supervisor = supervisor_with_test_stream("feed");
        let pipeline = supervisor.inner.lock().unwrap()["feed"].pipeline.clone();
        pipeline.set_state(gst::State::Ready).unwrap();
        let mut events = supervisor.subscribe();
        let stopping_supervisor = supervisor.clone();

        supervisor.record_eos_with(
            "feed",
            &pipeline,
            |pipeline| {
                pipeline.set_state(gst::State::Null)?;
                Ok(())
            },
            move |pipeline| {
                stopping_supervisor.stop("feed")?;
                pipeline.set_state(gst::State::Ready)?;
                Ok(())
            },
        );

        assert_eq!(pipeline.current_state(), gst::State::Null);
        assert!(supervisor.states().is_empty());
        let states: Vec<_> = std::iter::from_fn(|| events.try_recv().ok())
            .map(|event| event.state)
            .collect();
        assert_eq!(states, vec![StreamState::Stopping, StreamState::Stopped]);
    }

    #[test]
    fn eos_null_failure_never_attempts_playing_and_reports_failed() {
        let supervisor = supervisor_with_test_stream("feed");
        let pipeline = supervisor.inner.lock().unwrap()["feed"].pipeline.clone();
        let mut events = supervisor.subscribe();
        let playing_calls = Arc::new(AtomicUsize::new(0));
        let observed_calls = playing_calls.clone();

        supervisor.record_eos_with(
            "feed",
            &pipeline,
            |_| anyhow::bail!("injected Null failure"),
            move |_| {
                observed_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        );

        assert_eq!(playing_calls.load(Ordering::SeqCst), 0);
        let states = supervisor.states();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].state, StreamState::Failed);
        let failed = events.try_recv().unwrap();
        assert_eq!(failed.state, StreamState::Failed);
        assert!(failed.detail.unwrap().contains("injected Null failure"));
        assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn failed_stream_cannot_be_resurrected_by_eos() {
        let supervisor = supervisor_with_test_stream("feed");
        let pipeline = supervisor.inner.lock().unwrap()["feed"].pipeline.clone();
        supervisor
            .inner
            .lock()
            .unwrap()
            .get_mut("feed")
            .unwrap()
            .state = StreamState::Failed;
        let mut events = supervisor.subscribe();
        let transitions = Arc::new(AtomicUsize::new(0));
        let null_transitions = transitions.clone();
        let playing_transitions = transitions.clone();

        supervisor.record_eos_with(
            "feed",
            &pipeline,
            move |_| {
                null_transitions.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            move |_| {
                playing_transitions.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        );

        assert_eq!(transitions.load(Ordering::SeqCst), 0);
        assert_eq!(supervisor.states()[0].state, StreamState::Failed);
        assert!(supervisor.monitor_bus("feed", &pipeline).is_none());
        assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn starts_with_no_active_streams() {
        assert!(Supervisor::new().states().is_empty());
    }

    #[test]
    fn tracks_unique_clients_and_removes_them() {
        let supervisor = supervisor_with_test_stream("feed");
        let mut events = supervisor.subscribe();
        let client = SrtClientAddress {
            ip: "192.0.2.10".into(),
            port: 54321,
        };
        let other_client = SrtClientAddress {
            ip: "192.0.2.11".into(),
            port: 54322,
        };

        supervisor.record_client_added("feed", client.clone());
        assert_eq!(events.try_recv().unwrap().clients, vec![client.clone()]);

        supervisor.record_client_added("feed", client.clone());
        assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));
        assert_eq!(supervisor.states()[0].clients, vec![client.clone()]);

        supervisor.record_client_added("feed", other_client.clone());
        assert_eq!(
            events.try_recv().unwrap().clients,
            vec![client.clone(), other_client.clone()]
        );

        supervisor.record_client_removed("feed", &client);
        assert_eq!(
            events.try_recv().unwrap().clients,
            vec![other_client.clone()]
        );
        assert_eq!(supervisor.states()[0].clients, vec![other_client.clone()]);

        supervisor.record_client_removed("feed", &client);
        assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));

        supervisor.record_client_removed("feed", &other_client);
        assert!(events.try_recv().unwrap().clients.is_empty());
        assert!(supervisor.states()[0].clients.is_empty());
    }

    #[test]
    fn ignores_client_updates_for_unknown_streams() {
        let supervisor = Supervisor::new();
        let mut events = supervisor.subscribe();
        let client = SrtClientAddress {
            ip: "192.0.2.10".into(),
            port: 54321,
        };

        supervisor.record_client_added("missing", client.clone());
        supervisor.record_client_removed("missing", &client);

        assert!(supervisor.states().is_empty());
        assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn publishes_concurrent_client_updates_in_mutation_order() {
        let supervisor = supervisor_with_test_stream("feed");
        let mut events = supervisor.subscribe();
        let client = SrtClientAddress {
            ip: "192.0.2.10".into(),
            port: 54321,
        };

        let inner_guard = supervisor.inner.lock().unwrap();
        let add_supervisor = supervisor.clone();
        let add_client = client.clone();
        let (add_started_tx, add_started_rx) = mpsc::channel();
        let add = thread::spawn(move || {
            add_started_tx.send(()).unwrap();
            add_supervisor.record_client_added("feed", add_client);
        });
        add_started_rx.recv().unwrap();

        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match supervisor.publication.try_lock() {
                Err(TryLockError::WouldBlock) => break,
                Err(TryLockError::Poisoned(_)) => panic!("publication mutex poisoned"),
                Ok(guard) => drop(guard),
            }
            assert!(
                Instant::now() < deadline,
                "add never acquired publication lock"
            );
            thread::yield_now();
        }

        let remove_supervisor = supervisor.clone();
        let remove_client = client.clone();
        let (remove_started_tx, remove_started_rx) = mpsc::channel();
        let remove = thread::spawn(move || {
            remove_started_tx.send(()).unwrap();
            remove_supervisor.record_client_removed("feed", &remove_client);
        });
        remove_started_rx.recv().unwrap();

        drop(inner_guard);
        add.join().unwrap();
        remove.join().unwrap();

        assert_eq!(events.try_recv().unwrap().clients, vec![client]);
        let last_event = events.try_recv().unwrap();
        assert!(last_event.clients.is_empty());
        assert_eq!(last_event.clients, supervisor.states()[0].clients);
    }

    #[test]
    fn emit_preserves_explicit_state_and_snapshots_managed_fields() {
        let supervisor = supervisor_with_test_stream("feed");
        let client = SrtClientAddress {
            ip: "192.0.2.10".into(),
            port: 54321,
        };
        supervisor.record_client_added("feed", client.clone());
        let mut events = supervisor.subscribe();

        supervisor.emit("feed", StreamState::Starting, Some("starting again".into()));

        let event = events.try_recv().unwrap();
        assert_eq!(event.state, StreamState::Starting);
        assert_eq!(event.detail.as_deref(), Some("starting again"));
        assert_eq!(event.port, Some(9000));
        assert_eq!(event.latency_ms, Some(120));
        assert_eq!(event.mode, Some(ProcessingMode::RemuxCopy));
        assert_eq!(event.loop_count, Some(0));
        assert_eq!(event.clients, vec![client]);
    }

    #[test]
    fn emit_uses_empty_fallback_for_unknown_streams() {
        let supervisor = Supervisor::new();
        let mut events = supervisor.subscribe();

        supervisor.emit("missing", StreamState::Stopped, Some("gone".into()));

        let event = events.try_recv().unwrap();
        assert_eq!(event.stream_id, "missing");
        assert_eq!(event.state, StreamState::Stopped);
        assert_eq!(event.detail.as_deref(), Some("gone"));
        assert_eq!(event.port, None);
        assert_eq!(event.latency_ms, None);
        assert_eq!(event.mode, None);
        assert_eq!(event.loop_count, None);
        assert!(event.clients.is_empty());
    }
}
