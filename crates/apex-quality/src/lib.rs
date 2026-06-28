#![forbid(unsafe_code)]

use apex_domain::CancellationToken;
use std::ops::Range;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FocusTarget {
    pub id: String,
    pub label: String,
    pub enabled: bool,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FocusOrder {
    targets: Vec<FocusTarget>,
    current: Option<usize>,
}
impl FocusOrder {
    pub fn new(targets: Vec<FocusTarget>) -> Result<Self, String> {
        if targets
            .iter()
            .any(|t| t.id.trim().is_empty() || t.label.trim().is_empty())
        {
            return Err("focus targets require non-empty ids and accessible labels".into());
        }
        let current = targets.iter().position(|t| t.enabled);
        Ok(Self { targets, current })
    }
    pub fn current(&self) -> Option<&FocusTarget> {
        self.current.and_then(|i| self.targets.get(i))
    }
    pub fn move_next(&mut self, reverse: bool) -> Option<&FocusTarget> {
        if self.targets.is_empty() {
            return None;
        }
        let start = self.current.unwrap_or(0);
        for step in 1..=self.targets.len() {
            let i = if reverse {
                (start + self.targets.len() - step % self.targets.len()) % self.targets.len()
            } else {
                (start + step) % self.targets.len()
            };
            if self.targets[i].enabled {
                self.current = Some(i);
                break;
            }
        }
        self.current()
    }
}

pub fn visible_range(
    total: usize,
    scroll_offset_px: f32,
    row_height_px: f32,
    viewport_height_px: f32,
    overscan: usize,
) -> Result<Range<usize>, String> {
    if !row_height_px.is_finite()
        || row_height_px <= 0.0
        || !scroll_offset_px.is_finite()
        || !viewport_height_px.is_finite()
    {
        return Err("virtualization dimensions must be finite and row height positive".into());
    }
    let first = (scroll_offset_px.max(0.0) / row_height_px).floor() as usize;
    let count = (viewport_height_px.max(0.0) / row_height_px).ceil() as usize;
    Ok(first.saturating_sub(overscan).min(total)
        ..first
            .saturating_add(count)
            .saturating_add(overscan)
            .min(total))
}

pub fn process_chunks(
    data: &[u8],
    chunk_size: usize,
    cancellation: &CancellationToken,
    mut consume: impl FnMut(&[u8]),
) -> Result<usize, String> {
    if chunk_size == 0 {
        return Err("chunk size must be non-zero".into());
    }
    let mut processed = 0;
    for chunk in data.chunks(chunk_size) {
        if cancellation.is_cancelled() {
            return Err("processing cancelled".into());
        }
        consume(chunk);
        processed += chunk.len();
    }
    Ok(processed)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PerformanceObservation {
    pub workload: String,
    pub items: usize,
    pub rendered_items: usize,
    pub allocated_bytes_upper_bound: usize,
}
pub fn large_response_observation(
    total_rows: usize,
    viewport_rows: usize,
    overscan: usize,
    row_bytes: usize,
) -> PerformanceObservation {
    let rendered_items = total_rows.min(viewport_rows.saturating_add(overscan.saturating_mul(2)));
    PerformanceObservation {
        workload: "large-response-virtualization".into(),
        items: total_rows,
        rendered_items,
        allocated_bytes_upper_bound: rendered_items.saturating_mul(row_bytes),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    #[test]
    fn keyboard_focus_wraps_skips_disabled_and_has_labels() {
        let mut order = FocusOrder::new(vec![
            FocusTarget {
                id: "url".into(),
                label: "Request URL".into(),
                enabled: true,
            },
            FocusTarget {
                id: "hidden".into(),
                label: "Hidden".into(),
                enabled: false,
            },
            FocusTarget {
                id: "send".into(),
                label: "Send request".into(),
                enabled: true,
            },
        ])
        .unwrap();
        assert_eq!(order.current().unwrap().id, "url");
        assert_eq!(order.move_next(false).unwrap().id, "send");
        assert_eq!(order.move_next(false).unwrap().id, "url");
        assert_eq!(order.move_next(true).unwrap().id, "send");
    }
    #[test]
    fn million_row_response_renders_only_a_bounded_window() {
        let start = Instant::now();
        let range = visible_range(1_000_000, 5_000_000.0, 20.0, 800.0, 10).unwrap();
        assert!(range.len() <= 60);
        let observation = large_response_observation(1_000_000, 40, 10, 256);
        assert_eq!(observation.rendered_items, 60);
        assert!(observation.allocated_bytes_upper_bound < 20_000);
        assert!(start.elapsed() < Duration::from_millis(50));
    }
    #[test]
    fn chunk_processing_is_bounded_and_cancellable() {
        let token = CancellationToken::default();
        let mut chunks = 0;
        let processed =
            process_chunks(&vec![1; 1_000_000], 64 * 1024, &token, |_| chunks += 1).unwrap();
        assert_eq!(processed, 1_000_000);
        assert!(chunks < 20);
        token.cancel();
        assert!(process_chunks(&[1; 10], 2, &token, |_| {}).is_err());
    }
}
