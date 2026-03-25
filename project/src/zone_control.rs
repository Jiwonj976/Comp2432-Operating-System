// src/zone_control.rs
// ─────────────────────────────────────────────────────────────────────────────
// Zone access control — ensures no two robots occupy the same zone at once.
// Uses a per-zone Mutex behind a RwLock-protected HashMap so zones can be
// created dynamically while existing zones are accessed concurrently.
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::fmt;
use std::time::{Duration, Instant};

/// Represents which robot currently occupies a zone, if any.
#[derive(Debug, Clone)]
pub struct ZoneOccupancy {
    pub robot_id: String,
    pub entered_at: Instant,
}

/// A single hospital zone that can be occupied by at most one robot.
#[derive(Debug)]
struct Zone {
    occupant: Mutex<Option<ZoneOccupancy>>,
}

impl Zone {
    fn new() -> Self {
        Self {
            occupant: Mutex::new(None),
        }
    }
}

/// Result of attempting to enter a zone.
#[derive(Debug, PartialEq, Eq)]
pub enum ZoneResult {
    /// Successfully entered the zone.
    Entered,
    /// Zone is currently occupied by another robot.
    Occupied(String),
    /// The robot was already in this zone.
    AlreadyInside,
}

impl fmt::Display for ZoneResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ZoneResult::Entered => write!(f, "Entered"),
            ZoneResult::Occupied(by) => write!(f, "Occupied by {}", by),
            ZoneResult::AlreadyInside => write!(f, "Already inside"),
        }
    }
}

/// Zone access controller for the entire hospital.
pub struct ZoneController {
    zones: RwLock<HashMap<String, Arc<Zone>>>,
}

impl ZoneController {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            zones: RwLock::new(HashMap::new()),
        })
    }

    /// Pre-register a set of zone names.
    pub fn register_zones(&self, names: &[&str]) {
        let mut map = self.zones.write().unwrap();
        for name in names {
            map.entry(name.to_string())
                .or_insert_with(|| Arc::new(Zone::new()));
        }
    }

    /// Get or create a zone (lazy initialization).
    fn get_zone(&self, zone_name: &str) -> Arc<Zone> {
        // Fast path: read lock
        {
            let map = self.zones.read().unwrap();
            if let Some(z) = map.get(zone_name) {
                return Arc::clone(z);
            }
        }
        // Slow path: write lock to insert
        let mut map = self.zones.write().unwrap();
        Arc::clone(map.entry(zone_name.to_string()).or_insert_with(|| Arc::new(Zone::new())))
    }

    /// Attempt to enter a zone. Returns `ZoneResult` indicating outcome.
    pub fn enter_zone(&self, zone_name: &str, robot_id: &str) -> ZoneResult {
        let zone = self.get_zone(zone_name);
        let mut occupant = zone.occupant.lock().unwrap();
        match occupant.as_ref() {
            Some(occ) if occ.robot_id == robot_id => ZoneResult::AlreadyInside,
            Some(occ) => ZoneResult::Occupied(occ.robot_id.clone()),
            None => {
                *occupant = Some(ZoneOccupancy {
                    robot_id: robot_id.to_string(),
                    entered_at: Instant::now(),
                });
                ZoneResult::Entered
            }
        }
    }

    /// Leave a zone. Returns `true` if the robot was actually occupying it.
    pub fn leave_zone(&self, zone_name: &str, robot_id: &str) -> bool {
        let zone = self.get_zone(zone_name);
        let mut occupant = zone.occupant.lock().unwrap();
        if let Some(occ) = occupant.as_ref() {
            if occ.robot_id == robot_id {
                *occupant = None;
                return true;
            }
        }
        false
    }

    /// Try to enter a zone, retrying with back-off up to `timeout`.
    pub fn enter_zone_with_retry(
        &self,
        zone_name: &str,
        robot_id: &str,
        timeout: Duration,
        retry_interval: Duration,
    ) -> ZoneResult {
        let start = Instant::now();
        loop {
            let result = self.enter_zone(zone_name, robot_id);
            match &result {
                ZoneResult::Entered | ZoneResult::AlreadyInside => return result,
                ZoneResult::Occupied(_) => {
                    if start.elapsed() >= timeout {
                        return result;
                    }
                    std::thread::sleep(retry_interval);
                }
            }
        }
    }

    /// Query who currently occupies a zone.
    pub fn query_zone(&self, zone_name: &str) -> Option<String> {
        let zone = self.get_zone(zone_name);
        let occupant = zone.occupant.lock().unwrap();
        occupant.as_ref().map(|o| o.robot_id.clone())
    }

    /// List all registered zone names.
    pub fn list_zones(&self) -> Vec<String> {
        let map = self.zones.read().unwrap();
        map.keys().cloned().collect()
    }

    /// Force-evict a robot from all zones (used when robot goes offline).
    pub fn evict_robot(&self, robot_id: &str) -> Vec<String> {
        let map = self.zones.read().unwrap();
        let mut evicted_from = Vec::new();
        for (name, zone) in map.iter() {
            let mut occupant = zone.occupant.lock().unwrap();
            if let Some(occ) = occupant.as_ref() {
                if occ.robot_id == robot_id {
                    *occupant = None;
                    evicted_from.push(name.clone());
                }
            }
        }
        evicted_from
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_enter_and_leave() {
        let zc = ZoneController::new();
        assert_eq!(zc.enter_zone("ICU", "R1"), ZoneResult::Entered);
        assert_eq!(
            zc.enter_zone("ICU", "R2"),
            ZoneResult::Occupied("R1".into())
        );
        assert!(zc.leave_zone("ICU", "R1"));
        assert_eq!(zc.enter_zone("ICU", "R2"), ZoneResult::Entered);
    }

    #[test]
    fn test_already_inside() {
        let zc = ZoneController::new();
        assert_eq!(zc.enter_zone("Lab", "R1"), ZoneResult::Entered);
        assert_eq!(zc.enter_zone("Lab", "R1"), ZoneResult::AlreadyInside);
    }

    #[test]
    fn test_evict_robot() {
        let zc = ZoneController::new();
        zc.enter_zone("ICU", "R1");
        zc.enter_zone("Lab", "R1");
        let evicted = zc.evict_robot("R1");
        assert_eq!(evicted.len(), 2);
        // Both zones should be free now
        assert_eq!(zc.enter_zone("ICU", "R2"), ZoneResult::Entered);
        assert_eq!(zc.enter_zone("Lab", "R2"), ZoneResult::Entered);
    }

    #[test]
    fn test_concurrent_zone_access() {
        let zc = ZoneController::new();
        zc.register_zones(&["ER"]);
        let mut handles = vec![];
        let success_count = Arc::new(Mutex::new(0u32));

        for i in 0..10 {
            let zc = Arc::clone(&zc);
            let sc = Arc::clone(&success_count);
            handles.push(thread::spawn(move || {
                let result = zc.enter_zone("ER", &format!("R{}", i));
                if result == ZoneResult::Entered {
                    *sc.lock().unwrap() += 1;
                    // Simulate work
                    thread::sleep(Duration::from_millis(10));
                    zc.leave_zone("ER", &format!("R{}", i));
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }
        // At least one robot must have entered
        assert!(*success_count.lock().unwrap() >= 1);
    }

    #[test]
    fn test_enter_with_retry() {
        let zc = ZoneController::new();
        zc.enter_zone("OR", "R1");

        let zc2 = Arc::clone(&zc);
        let handle = thread::spawn(move || {
            // R1 leaves after 50ms
            thread::sleep(Duration::from_millis(50));
            zc2.leave_zone("OR", "R1");
        });

        let result = zc.enter_zone_with_retry(
            "OR",
            "R2",
            Duration::from_millis(200),
            Duration::from_millis(20),
        );
        assert_eq!(result, ZoneResult::Entered);
        handle.join().unwrap();
    }
}
