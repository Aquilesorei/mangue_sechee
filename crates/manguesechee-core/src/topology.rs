//! Spatial 2D Grid Topology engine.
//! Represents physical multi-monitor desk arrangement where each screen
//! has integer coordinates (grid_x, grid_y) relative to the controller (0, 0).

use serde::{Deserialize, Serialize};
use crate::protocol::Edge;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScreenNode {
    pub id:        String,
    pub name:      String,
    pub address:   Option<String>,
    pub grid_x:    i32,
    pub grid_y:    i32,
    pub width:     u32,
    pub height:    u32,
    #[serde(default)]
    pub alignment: Option<String>,
}

impl ScreenNode {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        address: Option<String>,
        grid_x: i32,
        grid_y: i32,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            address,
            grid_x,
            grid_y,
            width,
            height,
            alignment: None,
        }
    }

    pub fn with_alignment(mut self, alignment: Option<String>) -> Self {
        self.alignment = alignment;
        self
    }

    pub fn coord(&self) -> (i32, i32) {
        (self.grid_x, self.grid_y)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GridTopology {
    pub screens: Vec<ScreenNode>,
}

impl GridTopology {
    pub fn new() -> Self {
        Self { screens: Vec::new() }
    }

    /// Find the nearest unoccupied (x, y) slot starting from (target_x, target_y).
    /// Searches outward in cardinal directions (right, left, top, bottom) then perimeter rings.
    pub fn find_free_slot_near(&self, target_x: i32, target_y: i32) -> (i32, i32) {
        if self.find_at(target_x, target_y).is_none() {
            return (target_x, target_y);
        }
        for r in 1..=50 {
            // Check cardinal candidates first (right, left, top, bottom)
            let cardinals = [
                (target_x + r, target_y),
                (target_x - r, target_y),
                (target_x, target_y + r),
                (target_x, target_y - r),
            ];
            for (cx, cy) in cardinals {
                if self.find_at(cx, cy).is_none() {
                    return (cx, cy);
                }
            }
            // Check perimeter ring
            for dx in -r..=r {
                for dy in -r..=r {
                    if dx.abs() == r || dy.abs() == r {
                        let candidate = (target_x + dx, target_y + dy);
                        if self.find_at(candidate.0, candidate.1).is_none() {
                            return candidate;
                        }
                    }
                }
            }
        }
        (target_x + 100, target_y)
    }

    pub fn from_peer_infos(peers: &[crate::ipc::PeerInfo]) -> Self {
        let mut grid = Self::new();
        grid.add_or_update(ScreenNode::new(
            "local",
            "This Machine",
            None,
            0,
            0,
            1920,
            1080,
        ));
        for p in peers {
            grid.add_or_update(ScreenNode::new(
                p.name.clone(),
                p.name.clone(),
                Some(p.address.clone()),
                p.grid_x,
                p.grid_y,
                1920,
                1080,
            ));
        }
        grid
    }

    pub fn from_peer_configs(peers: &[crate::config::PeerConfig]) -> Self {
        let mut grid = Self::new();
        grid.add_or_update(ScreenNode::new(
            "local",
            "This Machine",
            None,
            0,
            0,
            1920,
            1080,
        ));
        for p in peers {
            let (gx, gy) = p.coordinates();
            let node = ScreenNode::new(
                p.id.clone(),
                p.id.clone(),
                p.address.clone(),
                gx,
                gy,
                1920,
                1080,
            ).with_alignment(p.alignment.clone());
            grid.add_or_update(node);
        }
        grid
    }

    pub fn add_or_update(&mut self, mut node: ScreenNode) {
        // If coordinate is already occupied by a different screen, auto-shift to avoid collision/shadowing
        if let Some(occupant) = self.screens.iter().find(|s| s.id != node.id && s.grid_x == node.grid_x && s.grid_y == node.grid_y) {
            let free_pos = self.find_free_slot_near(node.grid_x, node.grid_y);
            tracing::warn!(
                "⚠️ Grid collision: Screen '{}' requested occupied slot ({}, {}) (held by '{}'). Auto-shifted to ({}, {})",
                node.name, node.grid_x, node.grid_y, occupant.name, free_pos.0, free_pos.1
            );
            node.grid_x = free_pos.0;
            node.grid_y = free_pos.1;
        }

        if let Some(existing) = self.screens.iter_mut().find(|s| s.id == node.id) {
            *existing = node;
        } else {
            self.screens.push(node);
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.screens.retain(|s| s.id != id);
    }

    pub fn find_at(&self, x: i32, y: i32) -> Option<&ScreenNode> {
        self.screens.iter().find(|s| s.grid_x == x && s.grid_y == y)
    }

    pub fn find_by_id(&self, id: &str) -> Option<&ScreenNode> {
        self.screens.iter().find(|s| s.id == id)
    }

    pub fn find_by_addr(&self, addr: &str) -> Option<&ScreenNode> {
        self.screens.iter().find(|s| {
            if let Some(ref a) = s.address {
                a == addr || addr.contains(a) || a.contains(addr)
            } else {
                false
            }
        })
    }

    /// Look up the immediate neighbor in the given direction from `from: (x, y)`.
    pub fn find_neighbor(&self, from: (i32, i32), direction: Edge) -> Option<&ScreenNode> {
        let (dx, dy) = direction.delta();
        let target_x = from.0 + dx;
        let target_y = from.1 + dy;
        self.find_at(target_x, target_y)
    }

    /// Move a screen to a new coordinate slot.
    /// If another screen already occupies (new_x, new_y), swap their positions.
    pub fn move_screen(&mut self, id: &str, new_x: i32, new_y: i32) -> bool {
        let old_pos = match self.screens.iter().find(|s| s.id == id) {
            Some(s) => (s.grid_x, s.grid_y),
            None => return false,
        };

        if old_pos == (new_x, new_y) {
            return true;
        }

        // Swap if occupied
        if let Some(occupant) = self.screens.iter_mut().find(|s| s.id != id && s.grid_x == new_x && s.grid_y == new_y) {
            occupant.grid_x = old_pos.0;
            occupant.grid_y = old_pos.1;
        }

        if let Some(target) = self.screens.iter_mut().find(|s| s.id == id) {
            target.grid_x = new_x;
            target.grid_y = new_y;
            true
        } else {
            false
        }
    }

    /// Calculate bounding box: (min_x, max_x, min_y, max_y)
    pub fn bounds(&self) -> (i32, i32, i32, i32) {
        let mut min_x = 0;
        let mut max_x = 0;
        let mut min_y = 0;
        let mut max_y = 0;
        for s in &self.screens {
            min_x = min_x.min(s.grid_x);
            max_x = max_x.max(s.grid_x);
            min_y = min_y.min(s.grid_y);
            max_y = max_y.max(s.grid_y);
        }
        (min_x, max_x, min_y, max_y)
    }
}

// Legacy compatibility types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Topology {
    pub screens: Vec<Screen>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Screen {
    pub id: String,
    pub position: Position,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Position {
    Left,
    Right,
    Above,
    Below,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grid_neighbor_lookup_and_stacking() {
        let mut grid = GridTopology::new();
        grid.add_or_update(ScreenNode::new("laptop-r1", "Right 1", Some("192.168.1.51:24800".into()), 1, 0, 1920, 1080));
        grid.add_or_update(ScreenNode::new("laptop-r2", "Right 2", Some("192.168.1.52:24800".into()), 2, 0, 1920, 1080));
        grid.add_or_update(ScreenNode::new("laptop-top", "Top", Some("192.168.1.53:24800".into()), 0, 1, 1920, 1080));

        // From Center (0, 0) Right -> Right 1
        let r1 = grid.find_neighbor((0, 0), Edge::Right).unwrap();
        assert_eq!(r1.id, "laptop-r1");

        // From Right 1 (1, 0) Right -> Right 2 (stacked!)
        let r2 = grid.find_neighbor((1, 0), Edge::Right).unwrap();
        assert_eq!(r2.id, "laptop-r2");

        // From Right 2 (2, 0) Left -> Right 1
        let back_to_r1 = grid.find_neighbor((2, 0), Edge::Left).unwrap();
        assert_eq!(back_to_r1.id, "laptop-r1");

        // From Center (0, 0) Top -> Top Laptop
        let top = grid.find_neighbor((0, 0), Edge::Top).unwrap();
        assert_eq!(top.id, "laptop-top");
    }

    #[test]
    fn test_move_and_swap() {
        let mut grid = GridTopology::new();
        grid.add_or_update(ScreenNode::new("nodeA", "A", None, 1, 0, 1920, 1080));
        grid.add_or_update(ScreenNode::new("nodeB", "B", None, 2, 0, 1920, 1080));

        // Move nodeA to (2, 0): should swap with nodeB!
        assert!(grid.move_screen("nodeA", 2, 0));
        assert_eq!(grid.find_by_id("nodeA").unwrap().grid_x, 2);
        assert_eq!(grid.find_by_id("nodeB").unwrap().grid_x, 1);
    }

    #[test]
    fn test_grid_collision_auto_shift() {
        let mut grid = GridTopology::new();
        // Machine 1 at (1, 0)
        grid.add_or_update(ScreenNode::new("laptop1", "Laptop 1", None, 1, 0, 1920, 1080));
        // Machine 2 also requests (1, 0) -> auto-shifted to next free slot (2, 0)
        grid.add_or_update(ScreenNode::new("laptop2", "Laptop 2", None, 1, 0, 1920, 1080));
        // Machine 3 also requests (1, 0) -> auto-shifted to next free slot (3, 0) or adjacent
        grid.add_or_update(ScreenNode::new("laptop3", "Laptop 3", None, 1, 0, 1920, 1080));

        assert_eq!(grid.find_by_id("laptop1").unwrap().coord(), (1, 0));
        let l2_coord = grid.find_by_id("laptop2").unwrap().coord();
        let l3_coord = grid.find_by_id("laptop3").unwrap().coord();

        assert_ne!(l2_coord, (1, 0));
        assert_ne!(l3_coord, (1, 0));
        assert_ne!(l2_coord, l3_coord);
    }

    #[test]
    fn test_five_laptop_spatial_navigation() {
        let peers = vec![
            crate::ipc::PeerInfo {
                name: "Laptop-Right1".into(),
                display_name: "Sleepy Penguin".into(),
                address: "192.168.1.11:24800".into(),
                paired: true,
                connected: true,
                position: "right".into(),
                grid_x: 1,
                grid_y: 0,
            },
            crate::ipc::PeerInfo {
                name: "Laptop-Right2".into(),
                display_name: "Angry Potato".into(),
                address: "192.168.1.12:24800".into(),
                paired: true,
                connected: true,
                position: "right".into(),
                grid_x: 2,
                grid_y: 0,
            },
            crate::ipc::PeerInfo {
                name: "Laptop-Left".into(),
                display_name: "Peach".into(),
                address: "192.168.1.13:24800".into(),
                paired: true,
                connected: true,
                position: "left".into(),
                grid_x: -1,
                grid_y: 0,
            },
            crate::ipc::PeerInfo {
                name: "Laptop-Top".into(),
                display_name: "Quantum Toaster".into(),
                address: "192.168.1.14:24800".into(),
                paired: true,
                connected: true,
                position: "above".into(),
                grid_x: 0,
                grid_y: 1,
            },
            crate::ipc::PeerInfo {
                name: "Laptop-Bottom".into(),
                display_name: "Tiny Dragon".into(),
                address: "192.168.1.15:24800".into(),
                paired: true,
                connected: true,
                position: "below".into(),
                grid_x: 0,
                grid_y: -1,
            },
        ];

        let grid = GridTopology::from_peer_infos(&peers);

        // Local machine is automatically at (0, 0)
        let local = grid.find_at(0, 0).unwrap();
        assert_eq!(local.id, "local");
        assert_eq!(local.coord(), (0, 0));

        // Sequence: User at (0, 0) spams Ctrl+Alt+Right -> Right 1
        let mut curr = (0, 0);
        let next = grid.find_neighbor(curr, Edge::Right).unwrap();
        assert_eq!(next.id, "Laptop-Right1");
        curr = next.coord();
        assert_eq!(curr, (1, 0));

        // Spams Right again -> Right 2 (stacked screen!)
        let next = grid.find_neighbor(curr, Edge::Right).unwrap();
        assert_eq!(next.id, "Laptop-Right2");
        curr = next.coord();
        assert_eq!(curr, (2, 0));

        // Hits right boundary (no further machine)
        assert!(grid.find_neighbor(curr, Edge::Right).is_none());

        // Spams Left -> back to Right 1
        let next = grid.find_neighbor(curr, Edge::Left).unwrap();
        assert_eq!(next.id, "Laptop-Right1");
        curr = next.coord();
        assert_eq!(curr, (1, 0));

        // Spams Left again -> back to Local Machine (0, 0)!
        let next = grid.find_neighbor(curr, Edge::Left).unwrap();
        assert_eq!(next.id, "local");
        curr = next.coord();
        assert_eq!(curr, (0, 0));

        // Spams Left again -> Left machine (-1, 0)
        let next = grid.find_neighbor(curr, Edge::Left).unwrap();
        assert_eq!(next.id, "Laptop-Left");
        curr = next.coord();
        assert_eq!(curr, (-1, 0));

        // Back Right to local (0, 0)
        let next = grid.find_neighbor(curr, Edge::Right).unwrap();
        assert_eq!(next.id, "local");
        curr = next.coord();

        // Up to Top machine (0, 1)
        let next = grid.find_neighbor(curr, Edge::Top).unwrap();
        assert_eq!(next.id, "Laptop-Top");
        curr = next.coord();
        assert_eq!(curr, (0, 1));

        // Down back to local (0, 0)
        let next = grid.find_neighbor(curr, Edge::Bottom).unwrap();
        assert_eq!(next.id, "local");
        curr = next.coord();

        // Down to Bottom machine (0, -1)
        let next = grid.find_neighbor(curr, Edge::Bottom).unwrap();
        assert_eq!(next.id, "Laptop-Bottom");
        curr = next.coord();
        assert_eq!(curr, (0, -1));

        // Up back to local (0, 0)
        let next = grid.find_neighbor(curr, Edge::Top).unwrap();
        assert_eq!(next.id, "local");

        // Bounds test
        let (min_x, max_x, min_y, max_y) = grid.bounds();
        assert_eq!((min_x, max_x, min_y, max_y), (-1, 2, -1, 1));
    }
}
