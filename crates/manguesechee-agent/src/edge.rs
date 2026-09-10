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
        }
    }

    pub fn with_settings(mut self, deadzone_px: u32, delay_ms: u32, locked: bool) -> Self {
        self.corner_deadzone = deadzone_px as f64;
        self.switch_delay = Duration::from_millis(delay_ms as u64);
        self.locked = locked;
        self
    }

    pub fn set_locked(&mut self, locked: bool) {
        self.locked = locked;
        if locked {
            self.contact_start = None;
        }
    }

    /// Position the cursor at the entry point for an incoming crossing.
    pub fn place_at_entry(&mut self, from_edge: &Edge) {
        match from_edge {
            Edge::Right  => self.x = self.width  - 1.0,
            Edge::Left   => self.x = 0.0,
            Edge::Bottom => self.y = self.height - 1.0,
            Edge::Top    => self.y = 0.0,
        }
        self.contact_start = None;
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

        debug!("cursor at ({:.0}, {:.0})", self.x, self.y);

        let candidate = if self.x >= self.width {
            if self.y < self.corner_deadzone || self.y > (self.height - self.corner_deadzone) {
                self.x = self.width - 1.0;
                None
            } else {
                Some(Edge::Right)
            }
        } else if self.x < 0.0 {
            if self.y < self.corner_deadzone || self.y > (self.height - self.corner_deadzone) {
                self.x = 0.0;
                None
            } else {
                Some(Edge::Left)
            }
        } else if self.y >= self.height {
            if self.x < self.corner_deadzone || self.x > (self.width - self.corner_deadzone) {
                self.y = self.height - 1.0;
                None
            } else {
                Some(Edge::Bottom)
            }
        } else if self.y < 0.0 {
            if self.x < self.corner_deadzone || self.x > (self.width - self.corner_deadzone) {
                self.y = 0.0;
                None
            } else {
                Some(Edge::Top)
            }
        } else {
            None
        };

        match candidate {
            Some(edge) => {
                if self.switch_delay.as_millis() == 0 {
                    self.x = self.width / 2.0;
                    self.y = self.height / 2.0;
                    self.contact_start = None;
                    Some(edge)
                } else {
                    match self.contact_start {
                        Some((contact_edge, start_time)) if contact_edge == edge => {
                            if start_time.elapsed() >= self.switch_delay {
                                self.x = self.width / 2.0;
                                self.y = self.height / 2.0;
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
