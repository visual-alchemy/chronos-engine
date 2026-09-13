use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::Result;
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
        self.emit(&id, StreamState::Starting, None);
        pipeline.set_state(gst::State::Playing)?;
        self.inner
            .lock()
            .expect("supervisor mutex poisoned")
            .insert(
                id.clone(),
                ManagedStream {
                    pipeline,
                    state: StreamState::WaitingForCaller,
                    restarts: 0u64,
                    config,
                    mode,
                    clients: Vec::new(),
                },
            );
        self.emit(&id, StreamState::WaitingForCaller, None);

        // Spawn a monitor thread that re-acquires the bus after each restart
        // so it keeps watching across NULL → PLAYING cycles.
        let supervisor = self.clone();
        std::thread::spawn(move || {
            supervisor.monitor_loop(&id);
        });
        Ok(())
    }

    /// Bus monitor loop that survives pipeline restarts.
    ///
    /// After each EOS the pipeline is restarted (NULL → PLAYING). The old bus
    /// reference is flushed by the NULL transition, so we re-acquire the bus
    /// from the pipeline after every restart to keep receiving messages.
    fn monitor_loop(&self, id: &str) {
        loop {
            // Fetch the current bus from the live pipeline.
            let bus = {
                let streams = match self.inner.lock() {
                    Ok(s) => s,
                    Err(_) => return,
                };
                match streams.get(id) {
                    Some(m) => m.pipeline.bus(),
                    None => return, // stream was stopped/removed
                }
            };
            let Some(bus) = bus else { return };

            // Drain this bus until EOS, error, or the pipeline is removed.
            loop {
                let Some(message) = bus.timed_pop(gst::ClockTime::from_seconds(1)) else {
                    // Timeout — check if the stream still exists.
                    if !self
                        .inner
                        .lock()
                        .map(|s| s.contains_key(id))
                        .unwrap_or(false)
                    {
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
                        let pipeline_name = self
                            .inner
                            .lock()
                            .ok()
                            .and_then(|s| s.get(id).map(|m| m.pipeline.name().to_string()));
                        let is_pipeline_msg = src_name.is_some() && src_name == pipeline_name;
                        if is_pipeline_msg && sc.current() == gst::State::Playing {
                            let state_changed = if let Ok(mut streams) = self.inner.lock() {
                                if let Some(managed) = streams.get_mut(id) {
                                    if managed.state == StreamState::WaitingForCaller {
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
                                self.emit(id, StreamState::Running, None);
                            }
                        }
                    }
                    gst::MessageView::Eos(..) => {
                        // EOS: record and restart, then break to re-acquire the bus.
                        self.record_eos(id);
                        break;
                    }
                    gst::MessageView::Error(error) => {
                        self.record_error(id, error.error().to_string());
                        return; // Fatal — stop monitoring.
                    }
                    _ => {}
                }
            }

            // If the stream was removed (stopped), exit.
            if !self
                .inner
                .lock()
                .map(|s| s.contains_key(id))
                .unwrap_or(false)
            {
                return;
            }
        }
    }
    pub fn stop(&self, id: &str) -> Result<()> {
        self.emit(id, StreamState::Stopping, None);
        if let Some(managed) = self
            .inner
            .lock()
            .expect("supervisor mutex poisoned")
            .remove(id)
        {
            managed.pipeline.set_state(gst::State::Null)?;
        }
        self.emit(id, StreamState::Stopped, None);
        Ok(())
    }
    pub fn record_eos(&self, id: &str) {
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
            managed.restarts = managed.restarts.saturating_add(1);
            managed.state = StreamState::Looping;
            managed.pipeline.clone()
        };
        // Stop and immediately restart to loop the file.
        if let Err(err) = pipeline.set_state(gst::State::Null) {
            tracing::warn!(stream_id = id, ?err, "EOS: failed to set pipeline to Null");
        }
        if let Err(err) = pipeline.set_state(gst::State::Playing) {
            tracing::warn!(stream_id = id, ?err, "EOS: failed to restart pipeline");
            // Mark failed only when we can't recover
            if let Ok(mut streams) = self.inner.lock() {
                if let Some(managed) = streams.get_mut(id) {
                    managed.state = StreamState::Failed;
                }
            }
            self.emit(id, StreamState::Failed, Some("EOS restart failed".into()));
            return;
        }
        self.emit(id, StreamState::Looping, None);
    }
    fn record_error(&self, id: &str, detail: String) {
        if let Ok(mut streams) = self.inner.lock()
            && let Some(managed) = streams.get_mut(id)
        {
            managed.state = StreamState::Failed;
        }
        tracing::error!(stream_id = id, %detail, "GStreamer pipeline failed");
        self.emit(id, StreamState::Failed, Some(detail));
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
    use std::{
        sync::{TryLockError, mpsc},
        thread,
        time::{Duration, Instant},
    };

    use gstreamer as gst;
    use tokio::sync::broadcast::error::TryRecvError;

    use super::{ManagedStream, Supervisor};
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
