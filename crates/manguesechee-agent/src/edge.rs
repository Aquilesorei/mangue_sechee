//! Cursor edge detection.
//!
//! Tracks the accumulated mouse position and fires when it crosses a screen
//! boundary. Does not interact with any display server — pure arithmetic.

use manguesechee_core::protocol::Edge;
use tracing::debug;

pub struct EdgeDetector {
    x:      f64,
    y:      f64,
    width:  f64,
    height: f64,
}

impl EdgeDetector {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            x:      width  as f64 / 2.0,
            y:      height as f64 / 2.0,
            width:  width  as f64,
            height: height as f64,
        }
    }

    /// Position the cursor at the entry point for an incoming crossing.
    /// A cursor coming from the right arrives at the left edge of this screen,
    /// a cursor from the left arrives at the right edge, and so on.
    pub fn place_at_entry(&mut self, from_edge: &Edge) {
        match from_edge {
            Edge::Right  => self.x = self.width  - 1.0,
            Edge::Left   => self.x = 0.0,
            Edge::Bottom => self.y = self.height - 1.0,
            Edge::Top    => self.y = 0.0,
        }
    }

    /// Apply a relative mouse delta.
    /// Returns `Some(edge)` if the cursor has crossed that edge, `None` otherwise.
    /// On a crossing the cursor position is reset to the centre so subsequent
    /// moves are measured from a neutral position.
    pub fn update(&mut self, dx: i32, dy: i32) -> Option<Edge> {
        self.x += dx as f64;
        self.y += dy as f64;

        debug!("cursor at ({:.0}, {:.0})", self.x, self.y);

        let crossed = if self.x >= self.width {
            Some(Edge::Right)
        } else if self.x < 0.0 {
            Some(Edge::Left)
        } else if self.y >= self.height {
            Some(Edge::Bottom)
        } else if self.y < 0.0 {
            Some(Edge::Top)
        } else {
            None
        };

        if crossed.is_some() {
            // Reset to centre so the detector is clean for the next session
            self.x = self.width  / 2.0;
            self.y = self.height / 2.0;
        }

        crossed
    }
}
