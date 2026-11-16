//! 2D XZ spatial interest grid scaling bandwidth cleanly up to 100+ concurrent players.

use std::collections::HashMap;

use bevy::math::Vec3;

/// Integer 2D grid cell coordinate on the XZ world plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellCoord {
    pub x: i32,
    pub z: i32,
}

/// Lightweight spatial handle indexed per server tick inside [`InterestGrid`].
#[derive(Debug, Clone, Copy)]
pub struct SpatialPeer {
    pub player_id: u16,
    pub pos: Vec3,
}

/// Uniform 2D spatial hash grid returning distance-sorted AOI peer indices in `O(local_cells)`.
#[derive(Debug, Clone)]
pub struct InterestGrid {
    pub cell_size: f32,
    pub view_radius_cells: i32,
    buckets: HashMap<CellCoord, Vec<usize>>,
}

impl Default for InterestGrid {
    fn default() -> Self {
        Self::new(24.0, 2)
    }
}

impl InterestGrid {
    pub fn new(cell_size: f32, view_radius_cells: i32) -> Self {
        Self {
            cell_size: cell_size.max(1.0),
            view_radius_cells: view_radius_cells.max(0),
            buckets: HashMap::new(),
        }
    }

    pub fn cell_of(&self, pos: Vec3) -> CellCoord {
        CellCoord {
            x: (pos.x / self.cell_size).floor() as i32,
            z: (pos.z / self.cell_size).floor() as i32,
        }
    }

    /// Rebuilds the spatial grid from `peers` without reallocating the bucket map.
    pub fn rebuild(&mut self, peers: &[SpatialPeer]) {
        for list in self.buckets.values_mut() {
            list.clear();
        }
        for (idx, peer) in peers.iter().enumerate() {
            self.buckets
                .entry(self.cell_of(peer.pos))
                .or_default()
                .push(idx);
        }
    }

    /// Returns peer indices visible around `viewer_pos`, sorted nearest-first and capped at
    /// `max_peers`. Excludes `exclude_player_id` when specified.
    pub fn query_nearby(
        &self,
        peers: &[SpatialPeer],
        viewer_pos: Vec3,
        exclude_player_id: Option<u16>,
        max_peers: usize,
    ) -> Vec<usize> {
        let center = self.cell_of(viewer_pos);
        let r = self.view_radius_cells;
        let max_dist = (r as f32 + 1.0) * self.cell_size;
        let max_dist_sq = max_dist * max_dist;

        let mut hits: Vec<(f32, usize)> = Vec::new();
        for dz in -r..=r {
            for dx in -r..=r {
                let cell = CellCoord {
                    x: center.x + dx,
                    z: center.z + dz,
                };
                let Some(bucket) = self.buckets.get(&cell) else {
                    continue;
                };
                for &idx in bucket {
                    let Some(peer) = peers.get(idx) else {
                        continue;
                    };
                    if exclude_player_id == Some(peer.player_id) {
                        continue;
                    }
                    let d_sq = peer.pos.distance_squared(viewer_pos);
                    if d_sq <= max_dist_sq {
                        hits.push((d_sq, idx));
                    }
                }
            }
        }

        hits.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        hits.truncate(max_peers);
        hits.into_iter().map(|(_, idx)| idx).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::protocol::{MAX_PLAYERS, MAX_SNAPSHOT_PEERS};

    #[test]
    fn grid_culls_far_players_and_caps_dense_crowd_for_100_players() {
        let mut grid = InterestGrid::new(25.0, 2);

        // Spread 100 players 40m apart in a line -> observer at origin only sees nearby cells.
        let spread: Vec<SpatialPeer> = (0..MAX_PLAYERS as u16)
            .map(|i| SpatialPeer {
                player_id: i + 1,
                pos: Vec3::new(i as f32 * 40.0, 0.0, 0.0),
            })
            .collect();
        grid.rebuild(&spread);
        let visible_spread = grid.query_nearby(&spread, Vec3::ZERO, Some(1), MAX_SNAPSHOT_PEERS);
        assert!(
            !visible_spread.is_empty() && visible_spread.len() <= 4,
            "expected strict local AOI culling, got {} peers",
            visible_spread.len()
        );

        // Stack all 100 players within a single arena cell -> bounded cleanly by MAX_SNAPSHOT_PEERS.
        let dense: Vec<SpatialPeer> = (0..MAX_PLAYERS as u16)
            .map(|i| SpatialPeer {
                player_id: i + 1,
                pos: Vec3::new((i % 10) as f32, 0.0, (i / 10) as f32),
            })
            .collect();
        grid.rebuild(&dense);
        let visible_dense = grid.query_nearby(&dense, Vec3::ZERO, Some(1), MAX_SNAPSHOT_PEERS);
        assert_eq!(visible_dense.len(), MAX_SNAPSHOT_PEERS);
    }
}
