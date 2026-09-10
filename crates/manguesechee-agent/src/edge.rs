//! Cursor edge detection.
//!
//! Tracks the accumulated mouse position and fires when it crosses a screen
//! boundary. Does not interact with any display server — pure arithmetic.

use manguesechee_core::protocol::Edge;
use std::time::{Duration, Instant};
use tracing::debug;

pub struct EdgeDetector {
    x:               f64,
    y:               f64,
    width:           f64,
    height:          f64,
    corner_deadzone: f64,
    switch_delay:    Duration,
    contact_start:   Option<(Edge, Instant)>,
    locked:          bool,
    allowed_edge:    Option<Edge>,
    entry_cooldown:  Option<Instant>,
}

impl EdgeDetector {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            x:               width  as f64 / 2.0,
            y:               height as f64 / 2.0,
            width:           width  as f64,
            height:          height as f64,
            corner_deadzone: 50.0,
            switch_delay:    Duration::from_millis(0),
            contact_start:   None,
            locked:          false,
            allowed_edge:    None,
            entry_cooldown:  None,
        }
    }

    pub fn with_settings(mut self, deadzone_px: u32, delay_ms: u32, locked: bool) -> Self {
        self.corner_deadzone = deadzone_px as f64;
        self.switch_delay = Duration::from_millis(delay_ms as u64);
        self.locked = locked;
        self
    }

    pub fn set_allowed_edge(&mut self, edge: Option<Edge>) {
        self.allowed_edge = edge;
    }

    pub fn set_locked(&mut self, locked: bool) {
        self.locked = locked;
        if locked {
            self.contact_start = None;
        }
    }

    /// Position the cursor at the entry point for an incoming crossing.
    /// Leaves a comfortable margin inside the screen and arms a 150ms cooldown
    /// to prevent cursor bounce-back from entry jitter.
    pub fn place_at_entry(&mut self, from_edge: &Edge) {
        match from_edge {
            Edge::Right  => self.x = (self.width - 12.0).max(0.0),
            Edge::Left   => self.x = (self.width - 1.0).min(12.0),
            Edge::Bottom => self.y = (self.height - 12.0).max(0.0),
            Edge::Top    => self.y = (self.height - 1.0).min(12.0),
        }
        self.contact_start = None;
        self.entry_cooldown = Some(Instant::now());
    }

    /// Apply a relative mouse delta.
    pub fn update(&mut self, dx: i32, dy: i32) -> Option<Edge> {
        self.x += dx as f64;
        self.y += dy as f64;

        if self.locked {
            self.x = self.x.clamp(0.0, self.width - 1.0);
            self.y = self.y.clamp(0.0, self.height - 1.0);
            return None;
        }

        debug!("cursor at ({:.0}, {:.0}) / ({:.0}, {:.0})", self.x, self.y, self.width, self.height);

        let is_allowed = |edge: Edge, allowed: Option<Edge>| -> bool {
            match allowed {
                Some(a) => a == edge,
                None => true,
            }
        };

        let mut candidate = None;

        if self.x >= self.width {
            if is_allowed(Edge::Right, self.allowed_edge) {
                if self.y < self.corner_deadzone || self.y > (self.height - self.corner_deadzone) {
                    self.x = self.width - 1.0;
                } else {
                    candidate = Some(Edge::Right);
                }
            } else {
                self.x = self.width - 1.0;
            }
        } else if self.x < 0.0 {
            if is_allowed(Edge::Left, self.allowed_edge) {
                if self.y < self.corner_deadzone || self.y > (self.height - self.corner_deadzone) {
                    self.x = 0.0;
                } else {
                    candidate = Some(Edge::Left);
                }
            } else {
                self.x = 0.0;
            }
        }

        if self.y >= self.height {
            if is_allowed(Edge::Bottom, self.allowed_edge) {
                if self.x < self.corner_deadzone || self.x > (self.width - self.corner_deadzone) {
                    self.y = self.height - 1.0;
                } else {
                    candidate = Some(Edge::Bottom);
                }
            } else {
                self.y = self.height - 1.0;
            }
        } else if self.y < 0.0 {
            if is_allowed(Edge::Top, self.allowed_edge) {
                if self.x < self.corner_deadzone || self.x > (self.width - self.corner_deadzone) {
                    self.y = 0.0;
                } else {
                    candidate = Some(Edge::Top);
                }
            } else {
                self.y = 0.0;
            }
        }

        // Check entry cooldown to avoid immediately bouncing back through entry edge
        if let Some(cooldown_start) = self.entry_cooldown {
            if cooldown_start.elapsed() < Duration::from_millis(150) {
                if let Some(c) = candidate {
                    match c {
                        Edge::Right => self.x = self.width - 1.0,
                        Edge::Left => self.x = 0.0,
                        Edge::Bottom => self.y = self.height - 1.0,
                        Edge::Top => self.y = 0.0,
                    }
                    candidate = None;
                }
            } else {
                self.entry_cooldown = None;
            }
        }

        match candidate {
            Some(edge) => {
                if self.switch_delay.as_millis() == 0 {
                    self.contact_start = None;
                    Some(edge)
                } else {
                    match self.contact_start {
                        Some((contact_edge, start_time)) if contact_edge == edge => {
                            if start_time.elapsed() >= self.switch_delay {
                                self.contact_start = None;
                                Some(edge)
                            } else {
                                None
                            }
                        }
                        _ => {
                            self.contact_start = Some((edge, Instant::now()));
                            None
                        }
                    }
                }
            }
            None => {
                self.contact_start = None;
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowed_edge_clamping() {
        let mut detector = EdgeDetector::new(1920, 1080);
        // Restrict only to Left (e.g. peer returning to controller on Left)
        detector.set_allowed_edge(Some(Edge::Left));

        // Move cursor to top wall
        assert_eq!(detector.update(0, -1000), None);
        assert_eq!(detector.y, 0.0);

        // Move cursor to right wall
        assert_eq!(detector.update(3000, 0), None);
        assert_eq!(detector.x, 1919.0);

        // Move cursor to bottom wall
        assert_eq!(detector.update(0, 3000), None);
        assert_eq!(detector.y, 1079.0);

        // Move cursor to left wall (allowed)
        assert_eq!(detector.update(-5000, -500), Some(Edge::Left));
    }

    #[test]
    fn test_place_at_entry_and_cooldown() {
        let mut detector = EdgeDetector::new(1920, 1080);
        detector.place_at_entry(&Edge::Left);
        assert_eq!(detector.x, 12.0);

        // Immediate slight leftward jitter within 150ms should be absorbed by cooldown
        assert_eq!(detector.update(-50, 0), None);
        assert_eq!(detector.x, 0.0);
    }
}
