//! Cursor edge detection.
//!
//! Tracks the accumulated mouse position and fires when it crosses a screen
//! boundary. Does not interact with any display server — pure arithmetic.

use manguesechee_core::protocol::Edge;
use std::time::{Duration, Instant};
use tracing::debug;

pub struct EdgeDetector {
    x:                  f64,
    y:                  f64,
    width:              f64,
    height:             f64,
    corner_deadzone:    f64,
    switch_delay:       Duration,
    contact_start:      Option<(Edge, Instant)>,
    locked:             bool,
    allowed_edge:       Option<Edge>,
    entry_cooldown:     Option<Instant>,
    velocity_threshold: f64,
}

impl EdgeDetector {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            x:                  width  as f64 / 2.0,
            y:                  height as f64 / 2.0,
            width:              width  as f64,
            height:             height as f64,
            corner_deadzone:    50.0,
            switch_delay:       Duration::from_millis(0),
            contact_start:      None,
            locked:             false,
            allowed_edge:       None,
            entry_cooldown:     None,
            velocity_threshold: 20.0,
        }
    }

    pub fn with_settings(
        mut self,
        deadzone_px: u32,
        delay_ms: u32,
        locked: bool,
        velocity_threshold: u32,
    ) -> Self {
        self.corner_deadzone = deadzone_px as f64;
        self.switch_delay = Duration::from_millis(delay_ms as u64);
        self.locked = locked;
        self.velocity_threshold = velocity_threshold as f64;
        self
    }

    #[allow(dead_code)]
    pub fn set_velocity_threshold(&mut self, threshold: u32) {
        self.velocity_threshold = threshold as f64;
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
    /// Leaves a comfortable margin inside the screen and arms a 400ms cooldown
    /// to prevent cursor bounce-back from entry jitter or touchpad inertia.
    pub fn place_at_entry(&mut self, from_edge: &Edge) {
        let margin_x = (self.width * 0.05).clamp(80.0, 150.0);
        let margin_y = (self.height * 0.05).clamp(80.0, 150.0);
        match from_edge {
            Edge::Right  => self.x = (self.width - margin_x).max(0.0),
            Edge::Left   => self.x = margin_x.min(self.width - 1.0),
            Edge::Bottom => self.y = (self.height - margin_y).max(0.0),
            Edge::Top    => self.y = margin_y.min(self.height - 1.0),
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
            if cooldown_start.elapsed() < Duration::from_millis(400) {
                let margin_x = (self.width * 0.05).clamp(80.0, 150.0);
                let margin_y = (self.height * 0.05).clamp(80.0, 150.0);
                if let Some(c) = candidate {
                    match c {
                        Edge::Right => self.x = (self.width - margin_x).max(0.0),
                        Edge::Left => self.x = margin_x.min(self.width - 1.0),
                        Edge::Bottom => self.y = (self.height - margin_y).max(0.0),
                        Edge::Top => self.y = margin_y.min(self.height - 1.0),
                    }
                    candidate = None;
                }
            } else {
                self.entry_cooldown = None;
            }
        }

        let clamp_to_edge = |x: &mut f64, y: &mut f64, width: f64, height: f64, edge: Edge| {
            match edge {
                Edge::Right => *x = width - 1.0,
                Edge::Left => *x = 0.0,
                Edge::Bottom => *y = height - 1.0,
                Edge::Top => *y = 0.0,
            }
        };

        match candidate {
            Some(edge) => {
                let approach_speed = match edge {
                    Edge::Right => dx as f64,
                    Edge::Left => (-dx) as f64,
                    Edge::Bottom => dy as f64,
                    Edge::Top => (-dy) as f64,
                };

                let is_fast = self.velocity_threshold > 0.0 && approach_speed >= self.velocity_threshold;

                if is_fast {
                    // Fast flick into edge: cross immediately
                    self.contact_start = None;
                    Some(edge)
                } else if self.velocity_threshold == 0.0 {
                    // Velocity gating disabled (legacy behavior)
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
                                    clamp_to_edge(&mut self.x, &mut self.y, self.width, self.height, edge);
                                    None
                                }
                            }
                            _ => {
                                self.contact_start = Some((edge, Instant::now()));
                                clamp_to_edge(&mut self.x, &mut self.y, self.width, self.height, edge);
                                None
                            }
                        }
                    }
                } else {
                    // Slow movement (approach_speed < velocity_threshold):
                    // Stay clamped on current machine unless dwell delay is configured and elapsed
                    if self.switch_delay.as_millis() == 0 {
                        clamp_to_edge(&mut self.x, &mut self.y, self.width, self.height, edge);
                        self.contact_start = None;
                        None
                    } else {
                        match self.contact_start {
                            Some((contact_edge, start_time)) if contact_edge == edge => {
                                if start_time.elapsed() >= self.switch_delay {
                                    self.contact_start = None;
                                    Some(edge)
                                } else {
                                    clamp_to_edge(&mut self.x, &mut self.y, self.width, self.height, edge);
                                    None
                                }
                            }
                            _ => {
                                self.contact_start = Some((edge, Instant::now()));
                                clamp_to_edge(&mut self.x, &mut self.y, self.width, self.height, edge);
                                None
                            }
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
        let expected_margin = (1920.0_f64 * 0.05).clamp(80.0, 150.0);
        assert_eq!(detector.x, expected_margin);

        // Immediate leftward jitter within 400ms should be absorbed by cooldown
        assert_eq!(detector.update(-500, 0), None);
        assert_eq!(detector.x, expected_margin);
    }

    #[test]
    fn test_fast_flick_jumps_immediately() {
        let mut detector = EdgeDetector::new(1920, 1080).with_settings(50, 200, false, 20);
        // Position near right edge (x = 1910)
        detector.x = 1910.0;
        detector.y = 500.0;

        // Fast flick: dx = 25 (>= threshold of 20)
        let crossed = detector.update(25, 0);
        assert_eq!(crossed, Some(Edge::Right));
    }

    #[test]
    fn test_slow_movement_clamped_when_switch_delay_zero() {
        let mut detector = EdgeDetector::new(1920, 1080).with_settings(50, 0, false, 20);
        detector.x = 1910.0;
        detector.y = 500.0;

        // Slow movement: dx = 15 (< threshold of 20)
        let crossed = detector.update(15, 0);
        assert_eq!(crossed, None);
        assert_eq!(detector.x, 1919.0);

        // Continued slow movement keeps cursor clamped on current machine
        let crossed2 = detector.update(5, 0);
        assert_eq!(crossed2, None);
        assert_eq!(detector.x, 1919.0);

        // But a fast flick immediately crosses
        let crossed3 = detector.update(25, 0);
        assert_eq!(crossed3, Some(Edge::Right));
    }

    #[test]
    fn test_slow_movement_dwell_delay() {
        let mut detector = EdgeDetector::new(1920, 1080).with_settings(50, 50, false, 20);
        detector.x = 1910.0;
        detector.y = 500.0;

        // Slow movement hits edge: contact timer starts
        let crossed = detector.update(15, 0);
        assert_eq!(crossed, None);

        // Too soon (< 50ms)
        let crossed2 = detector.update(2, 0);
        assert_eq!(crossed2, None);

        // Sleep to let dwell time expire
        std::thread::sleep(Duration::from_millis(60));
        let crossed3 = detector.update(2, 0);
        assert_eq!(crossed3, Some(Edge::Right));
    }

    #[test]
    fn test_velocity_threshold_zero_disabled() {
        let mut detector = EdgeDetector::new(1920, 1080).with_settings(50, 0, false, 0);
        detector.x = 1910.0;
        detector.y = 500.0;

        // Slow movement crosses immediately because threshold is 0 (disabled)
        let crossed = detector.update(15, 0);
        assert_eq!(crossed, Some(Edge::Right));
    }
}
