use crate::domain::StreamEvent;

pub fn render(events: &[StreamEvent]) -> String {
    let mut output = String::from(
        "# HELP chronos_streams Number of streams by state\n# TYPE chronos_streams gauge\n",
    );
    for event in events {
        output.push_str(
            &format!(
                "chronos_streams{{stream_id=\"{}\",state=\"{:?}\"}} 1\n",
                event.stream_id, event.state
            )
            .to_ascii_lowercase(),
        );
    }
    output
}

#[cfg(test)]
mod tests {
    use super::render;
    use crate::domain::{StreamEvent, StreamState};
    #[test]
    fn emits_prometheus_stream_state() {
        assert!(
            render(&[StreamEvent {
                stream_id: "one".into(),
                state: StreamState::Running,
                detail: None,
                port: None,
                latency_ms: None,
                mode: None,
                loop_count: None,
                clients: Vec::new(),
            }])
            .contains("chronos_streams")
        );
    }
}
