use apex_domain::CancellationToken;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub enum StreamProtocol {
    WebSocket,
    ServerSentEvents,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub enum StreamDirection {
    Incoming,
    Outgoing,
    System,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct StreamEvent {
    pub sequence: u64,
    pub timestamp_ms: u128,
    pub direction: StreamDirection,
    pub event_type: Option<String>,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct ReconnectPolicy {
    pub enabled: bool,
    pub maximum_attempts: usize,
    pub base_delay_ms: u64,
    pub maximum_delay_ms: u64,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            maximum_attempts: 0,
            base_delay_ms: 500,
            maximum_delay_ms: 30_000,
        }
    }
}

impl ReconnectPolicy {
    pub fn delay_ms(&self, attempt: usize) -> Option<u64> {
        if !self.enabled || attempt == 0 || attempt > self.maximum_attempts {
            return None;
        }
        let exponent = u32::try_from(attempt.saturating_sub(1))
            .unwrap_or(u32::MAX)
            .min(31);
        Some(
            self.base_delay_ms
                .saturating_mul(1_u64 << exponent)
                .min(self.maximum_delay_ms),
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct StreamSessionExport {
    pub protocol: StreamProtocol,
    pub connected: bool,
    pub dropped_events: u64,
    pub events: Vec<StreamEvent>,
}

#[derive(Clone, Debug)]
pub struct BoundedStreamLog {
    protocol: StreamProtocol,
    capacity: usize,
    maximum_event_bytes: usize,
    connected: bool,
    next_sequence: u64,
    dropped_events: u64,
    events: VecDeque<StreamEvent>,
}

impl BoundedStreamLog {
    pub fn new(
        protocol: StreamProtocol,
        capacity: usize,
        maximum_event_bytes: usize,
    ) -> Result<Self, String> {
        if capacity == 0 || capacity > 1_000_000 {
            return Err("stream event capacity must be between 1 and 1000000".to_owned());
        }
        if maximum_event_bytes == 0 || maximum_event_bytes > 64 * 1024 * 1024 {
            return Err("stream event byte limit must be between 1 and 64 MiB".to_owned());
        }
        Ok(Self {
            protocol,
            capacity,
            maximum_event_bytes,
            connected: false,
            next_sequence: 1,
            dropped_events: 0,
            events: VecDeque::with_capacity(capacity.min(4096)),
        })
    }

    pub fn connect(&mut self, cancellation: &CancellationToken) -> Result<(), String> {
        if cancellation.is_cancelled() {
            return Err("stream connection was cancelled".to_owned());
        }
        self.connected = true;
        self.push(
            StreamDirection::System,
            Some("connected".to_owned()),
            Vec::new(),
        )
    }

    pub fn disconnect(&mut self, reason: &str) -> Result<(), String> {
        if !self.connected {
            return Ok(());
        }
        self.push(
            StreamDirection::System,
            Some("disconnected".to_owned()),
            reason.as_bytes().to_vec(),
        )?;
        self.connected = false;
        Ok(())
    }

    pub fn push(
        &mut self,
        direction: StreamDirection,
        event_type: Option<String>,
        data: Vec<u8>,
    ) -> Result<(), String> {
        if data.len() > self.maximum_event_bytes {
            self.dropped_events = self.dropped_events.saturating_add(1);
            return Err("stream event exceeds configured byte limit".to_owned());
        }
        if self.events.len() == self.capacity {
            self.events.pop_front();
            self.dropped_events = self.dropped_events.saturating_add(1);
        }
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("system clock is before Unix epoch: {error}"))?
            .as_millis();
        self.events.push_back(StreamEvent {
            sequence: self.next_sequence,
            timestamp_ms,
            direction,
            event_type,
            data,
        });
        self.next_sequence = self.next_sequence.saturating_add(1);
        Ok(())
    }

    pub fn filtered(&self, text: &str) -> Vec<&StreamEvent> {
        let needle = text.to_ascii_lowercase();
        self.events
            .iter()
            .filter(|event| {
                event
                    .event_type
                    .as_deref()
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .contains(&needle)
                    || String::from_utf8_lossy(&event.data)
                        .to_ascii_lowercase()
                        .contains(&needle)
            })
            .collect()
    }

    pub fn export(&self) -> StreamSessionExport {
        StreamSessionExport {
            protocol: self.protocol,
            connected: self.connected,
            dropped_events: self.dropped_events,
            events: self.events.iter().cloned().collect(),
        }
    }

    pub fn dropped_events(&self) -> u64 {
        self.dropped_events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_log_reports_drops_and_filters() {
        let mut log = BoundedStreamLog::new(StreamProtocol::WebSocket, 2, 128).expect("log");
        log.connect(&CancellationToken::default()).expect("connect");
        log.push(
            StreamDirection::Incoming,
            Some("message".to_owned()),
            b"alpha".to_vec(),
        )
        .expect("push");
        log.push(
            StreamDirection::Incoming,
            Some("message".to_owned()),
            b"beta".to_vec(),
        )
        .expect("push");
        assert_eq!(log.export().events.len(), 2);
        assert_eq!(log.dropped_events(), 1);
        assert_eq!(log.filtered("beta").len(), 1);
    }

    #[test]
    fn oversized_and_cancelled_events_are_visible() {
        let mut log = BoundedStreamLog::new(StreamProtocol::ServerSentEvents, 4, 4).expect("log");
        let token = CancellationToken::default();
        token.cancel();
        assert!(log.connect(&token).is_err());
        assert!(
            log.push(StreamDirection::Incoming, None, b"12345".to_vec())
                .is_err()
        );
        assert_eq!(log.dropped_events(), 1);
    }

    #[test]
    fn reconnect_backoff_is_bounded() {
        let policy = ReconnectPolicy {
            enabled: true,
            maximum_attempts: 5,
            base_delay_ms: 100,
            maximum_delay_ms: 250,
        };
        assert_eq!(policy.delay_ms(1), Some(100));
        assert_eq!(policy.delay_ms(3), Some(250));
        assert_eq!(policy.delay_ms(6), None);
    }
}
