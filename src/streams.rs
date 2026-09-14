use std::fmt;

pub const SRT_PORT_MIN: u16 = 10000;
pub const SRT_PORT_MAX: u16 = 10049;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SrtListenerConfig {
    pub port: u16,
    pub latency_ms: u32,
}

impl SrtListenerConfig {
    pub fn new(port: u16, latency_ms: u32) -> Result<Self, StreamConfigError> {
        if !(SRT_PORT_MIN..=SRT_PORT_MAX).contains(&port) {
            return Err(StreamConfigError::PortOutOfRange(port));
        }
        if latency_ms == 0 || latency_ms > i32::MAX as u32 {
            return Err(StreamConfigError::InvalidLatency);
        }
        Ok(Self { port, latency_ms })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamConfigError {
    PortOutOfRange(u16),
    InvalidLatency,
}

impl fmt::Display for StreamConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PortOutOfRange(port) => write!(f, "SRT port {port} is outside 10000-10049"),
            Self::InvalidLatency => write!(f, "SRT latency must be positive"),
        }
    }
}

impl std::error::Error for StreamConfigError {}

#[cfg(test)]
mod tests {
    use super::SrtListenerConfig;

    #[test]
    fn rejects_ports_outside_the_reserved_range() {
        assert!(SrtListenerConfig::new(9999, 120).is_err());
        assert!(SrtListenerConfig::new(10050, 120).is_err());
    }

    #[test]
    fn rejects_latency_that_gstreamer_cannot_represent() {
        assert!(SrtListenerConfig::new(10000, i32::MAX as u32 + 1).is_err());
    }
}
