// src/health_monitor.rs
// ─────────────────────────────────────────────────────────────────────────────
// Health monitor — tracks robot heartbeats and marks unresponsive robots as
// offline. A background thread periodically checks for timeouts.
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

/// Current status of a robot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RobotStatus {
    Online,
    Offline,
}

impl std::fmt::Display for RobotStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RobotStatus::Online => write!(f, "Online"),
            RobotStatus::Offline => write!(f, "Offline"),
        }
    }
}

/// Per-robot health record.
#[derive(Debug, Clone)]
pub struct RobotHealth {
    pub robot_id: String,
    pub status: RobotStatus,
    pub last_heartbeat: Instant,
    pub total_heartbeats: u64,
}

/// Callback invoked when a robot goes offline.
pub type OfflineCallback = Arc<dyn Fn(&str) + Send + Sync>;

/// The health monitor tracks all registered robots.
pub struct HealthMonitor {
    robots: RwLock<HashMap<String, Mutex<RobotHealth>>>,
    timeout: Duration,
    running: Mutex<bool>,
    on_offline: RwLock<Option<OfflineCallback>>,
}

impl HealthMonitor {
    /// Create a new health monitor with the given heartbeat timeout.
    pub fn new(timeout: Duration) -> Arc<Self> {
        Arc::new(Self {
            robots: RwLock::new(HashMap::new()),
            timeout,
            running: Mutex::new(false),
            on_offline: RwLock::new(None),
        })
    }

    /// Set a callback that fires when a robot transitions to Offline.
    pub fn set_offline_callback(&self, cb: OfflineCallback) {
        *self.on_offline.write().unwrap() = Some(cb);
    }

    /// Register a robot (initially online).
    pub fn register(&self, robot_id: &str) {
        let health = RobotHealth {
            robot_id: robot_id.to_string(),
            status: RobotStatus::Online,
            last_heartbeat: Instant::now(),
            total_heartbeats: 0,
        };
        let mut map = self.robots.write().unwrap();
        map.insert(robot_id.to_string(), Mutex::new(health));
    }

    /// Record a heartbeat from a robot.
    pub fn heartbeat(&self, robot_id: &str) -> bool {
        let map = self.robots.read().unwrap();
        if let Some(entry) = map.get(robot_id) {
            let mut h = entry.lock().unwrap();
            h.last_heartbeat = Instant::now();
            h.total_heartbeats += 1;
            if h.status == RobotStatus::Offline {
                h.status = RobotStatus::Online;
                println!("[HealthMonitor] {} came back ONLINE", robot_id);
            }
            true
        } else {
            false
        }
    }

    /// Query the status of a specific robot.
    pub fn get_status(&self, robot_id: &str) -> Option<(RobotStatus, Instant)> {
        let map = self.robots.read().unwrap();
        map.get(robot_id).map(|entry| {
            let h = entry.lock().unwrap();
            (h.status, h.last_heartbeat)
        })
    }

    /// Get a snapshot of all robot health records.
    pub fn snapshot(&self) -> Vec<RobotHealth> {
        let map = self.robots.read().unwrap();
        map.values()
            .map(|entry| entry.lock().unwrap().clone())
            .collect()
    }

    /// Perform a single check cycle: mark timed-out robots as offline.
    /// Returns a list of robot IDs that were newly marked offline.
    pub fn check_timeouts(&self) -> Vec<String> {
        let map = self.robots.read().unwrap();
        let mut newly_offline = Vec::new();
        for (id, entry) in map.iter() {
            let mut h = entry.lock().unwrap();
            if h.status == RobotStatus::Online && h.last_heartbeat.elapsed() > self.timeout {
                h.status = RobotStatus::Offline;
                newly_offline.push(id.clone());
                println!(
                    "[HealthMonitor] {} marked OFFLINE (no heartbeat for {:?})",
                    id,
                    h.last_heartbeat.elapsed()
                );
            }
        }
        // Fire callbacks outside the lock
        drop(map);
        if !newly_offline.is_empty() {
            let cb = self.on_offline.read().unwrap();
            if let Some(ref callback) = *cb {
                for id in &newly_offline {
                    callback(id);
                }
            }
        }
        newly_offline
    }

    /// Start the background monitor thread that periodically checks timeouts.
    /// Returns a handle to stop monitoring.
    pub fn start_monitoring(self: &Arc<Self>, check_interval: Duration) -> MonitorHandle {
        {
            let mut running = self.running.lock().unwrap();
            *running = true;
        }
        let monitor = Arc::clone(self);
        let handle = thread::spawn(move || {
            loop {
                thread::sleep(check_interval);
                let running = monitor.running.lock().unwrap();
                if !*running {
                    break;
                }
                drop(running);
                monitor.check_timeouts();
            }
        });
        MonitorHandle {
            monitor: Arc::clone(self),
            thread: Some(handle),
        }
    }

    /// Stop the background monitor.
    pub fn stop(&self) {
        let mut running = self.running.lock().unwrap();
        *running = false;
    }
}

/// Handle returned by `start_monitoring`; stops the monitor on drop.
pub struct MonitorHandle {
    monitor: Arc<HealthMonitor>,
    thread: Option<thread::JoinHandle<()>>,
}

impl MonitorHandle {
    pub fn stop(mut self) {
        self.monitor.stop();
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}

impl Drop for MonitorHandle {
    fn drop(&mut self) {
        self.monitor.stop();
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_heartbeat() {
        let hm = HealthMonitor::new(Duration::from_millis(100));
        hm.register("R1");
        assert!(hm.heartbeat("R1"));
        assert!(!hm.heartbeat("R_UNKNOWN"));
    }

    #[test]
    fn test_timeout_detection() {
        let hm = HealthMonitor::new(Duration::from_millis(50));
        hm.register("R1");
        hm.register("R2");

        // Send heartbeat for R2 only
        thread::sleep(Duration::from_millis(30));
        hm.heartbeat("R2");
        thread::sleep(Duration::from_millis(30));

        let offline = hm.check_timeouts();
        assert!(offline.contains(&"R1".to_string()));
        assert!(!offline.contains(&"R2".to_string()));
    }

    #[test]
    fn test_robot_comes_back_online() {
        let hm = HealthMonitor::new(Duration::from_millis(30));
        hm.register("R1");

        thread::sleep(Duration::from_millis(50));
        hm.check_timeouts();
        assert_eq!(hm.get_status("R1").unwrap().0, RobotStatus::Offline);

        // Send heartbeat → should come back online
        hm.heartbeat("R1");
        assert_eq!(hm.get_status("R1").unwrap().0, RobotStatus::Online);
    }

    #[test]
    fn test_offline_callback() {
        let hm = HealthMonitor::new(Duration::from_millis(30));
        let notified = Arc::new(Mutex::new(Vec::new()));
        let notified2 = Arc::clone(&notified);

        hm.set_offline_callback(Arc::new(move |id: &str| {
            notified2.lock().unwrap().push(id.to_string());
        }));

        hm.register("R1");
        thread::sleep(Duration::from_millis(50));
        hm.check_timeouts();

        let list = notified.lock().unwrap();
        assert!(list.contains(&"R1".to_string()));
    }

    #[test]
    fn test_background_monitoring() {
        let hm = HealthMonitor::new(Duration::from_millis(40));
        hm.register("R1");

        let handle = hm.start_monitoring(Duration::from_millis(20));

        // Wait for timeout
        thread::sleep(Duration::from_millis(100));
        assert_eq!(hm.get_status("R1").unwrap().0, RobotStatus::Offline);

        handle.stop();
    }

    #[test]
    fn test_snapshot() {
        let hm = HealthMonitor::new(Duration::from_secs(5));
        hm.register("R1");
        hm.register("R2");
        hm.heartbeat("R1");

        let snap = hm.snapshot();
        assert_eq!(snap.len(), 2);
    }
}
