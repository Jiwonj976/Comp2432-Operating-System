// src/robot.rs
// ─────────────────────────────────────────────────────────────────────────────
// Robot worker — each robot runs in its own thread, fetching tasks from the
// queue, entering/leaving zones, and sending heartbeats.
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::VecDeque;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::health_monitor::HealthMonitor;
use crate::task_queue::{TaskKind, TaskQueue};
use crate::zone_control::ZoneController;

/// Configuration for a robot worker.
#[derive(Debug, Clone)]
pub struct RobotConfig {
    pub id: String,
    /// Simulated task execution time range (min, max) in milliseconds.
    pub work_time_ms: (u64, u64),
    /// Heartbeat interval in milliseconds.
    pub heartbeat_interval_ms: u64,
    /// Whether this robot should simulate a failure (stop sending heartbeats).
    pub simulate_failure_after: Option<u32>,
}

impl RobotConfig {
    pub fn new(id: &str) -> Self {
        Self {
            id: id.to_string(),
            work_time_ms: (50, 150),
            heartbeat_interval_ms: 100,
            simulate_failure_after: None,
        }
    }
}

/// Statistics collected while a robot processes tasks.
#[derive(Debug, Default)]
pub struct RobotStats {
    pub tasks_completed: u32,
    pub zone_waits: u32,
    pub zone_denials: u32,
}

/// Spawn a robot worker thread that processes tasks from the queue.
pub fn spawn_robot(
    config: RobotConfig,
    task_queue: Arc<TaskQueue>,
    zone_ctrl: Arc<ZoneController>,
    health_monitor: Arc<HealthMonitor>,
) -> thread::JoinHandle<RobotStats> {
    thread::spawn(move || {
        let mut stats = RobotStats::default();
        let mut tasks_done: u32 = 0;
        let robot_id = &config.id;

        println!("[{}] Robot started", robot_id);

        // Heartbeat sender in a separate thread
        let hm = Arc::clone(&health_monitor);
        let rid = robot_id.to_string();
        let hb_interval = Duration::from_millis(config.heartbeat_interval_ms);
        let fail_after = config.simulate_failure_after;
        let hb_task_queue = Arc::clone(&task_queue);

        let _heartbeat_thread = thread::spawn(move || {
            let mut beats = 0u32;
            loop {
                // Check failure simulation FIRST so it fires even if queue is already drained
                if let Some(limit) = fail_after {
                    if beats >= limit {
                        println!("[{}] !! Simulating failure — stopping heartbeats", rid);
                        return;
                    }
                }
                if hb_task_queue.is_closed() && hb_task_queue.is_empty() {
                    break;
                }
                hm.heartbeat(&rid);
                beats += 1;
                thread::sleep(hb_interval);
            }
        });

        // Local deferred queue — tasks whose zone was blocked are parked here
        // and retried once fresher tasks have been processed (deadlock prevention).
        let mut deferred: VecDeque<crate::task_queue::Task> = VecDeque::new();

        // Main task loop
        loop {
            // Prefer fresh tasks from the shared queue; fall back to deferred.
            let (task, is_retry) = if deferred.is_empty() {
                // Nothing deferred: block-wait for the next shared task.
                match task_queue.fetch() {
                    Some(t) => (t, false),
                    None => break, // queue closed and empty
                }
            } else {
                // Have deferred tasks: try shared queue first (non-blocking).
                match task_queue.try_fetch() {
                    Some(t) => (t, false),
                    None => {
                        // No fresh tasks available — sleep briefly then retry deferred.
                        thread::sleep(Duration::from_millis(20));
                        match deferred.pop_front() {
                            Some(t) => (t, true),
                            None => continue, // shouldn't happen; guard anyway
                        }
                    }
                }
            };

            if is_retry {
                println!("[{}] ~~~ Retrying deferred {} ~~~", robot_id, task);
            } else {
                println!("[{}] Fetched {}", robot_id, task);
            }

            // Determine which zone the task requires
            let zone = match &task.kind {
                TaskKind::Delivery { from: _, to } => Some(to.clone()),
                TaskKind::Disinfection { zone } => Some(zone.clone()),
                TaskKind::SurgicalAssist { room } => Some(room.clone()),
            };

            if let Some(zone_name) = &zone {
                use crate::zone_control::ZoneResult;

                match zone_ctrl.enter_zone(zone_name, robot_id) {
                    ZoneResult::Occupied(ref by) => {
                        // Zone is busy — defer this task and work on something else.
                        println!(
                            "[{}] *** BLOCKED: zone '{}' held by {} — deferring, moving to next task ***",
                            robot_id, zone_name, by
                        );
                        stats.zone_waits += 1;
                        deferred.push_back(task);
                        continue;
                    }
                    ZoneResult::Entered => {
                        println!("[{}] Entered zone '{}'", robot_id, zone_name);
                    }
                    ZoneResult::AlreadyInside => {
                        println!("[{}] Already in zone '{}'", robot_id, zone_name);
                    }
                }
            }

            // Simulate doing the work
            let work_ms = config.work_time_ms.0
                + (rand::random::<u64>() % (config.work_time_ms.1 - config.work_time_ms.0 + 1));
            thread::sleep(Duration::from_millis(work_ms));

            // Leave the zone
            if let Some(zone_name) = &zone {
                zone_ctrl.leave_zone(zone_name, robot_id);
                println!("[{}] Left zone '{}'", robot_id, zone_name);
            }

            tasks_done += 1;
            stats.tasks_completed += 1;
            println!("[{}] Completed {} (total: {})", robot_id, task, tasks_done);
        }

        println!("[{}] Robot shutting down — {} tasks completed", robot_id, tasks_done);
        stats
    })
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_robot_processes_tasks() {
        let tq = TaskQueue::new(10);
        let zc = ZoneController::new();
        let hm = HealthMonitor::new(Duration::from_secs(5));

        zc.register_zones(&["ICU", "Lab"]);
        hm.register("R1");

        tq.submit(TaskKind::Disinfection { zone: "ICU".into() }, 1);
        tq.submit(TaskKind::Disinfection { zone: "Lab".into() }, 2);
        tq.close();

        let config = RobotConfig {
            id: "R1".into(),
            work_time_ms: (10, 20),
            heartbeat_interval_ms: 50,
            simulate_failure_after: None,
        };

        let handle = spawn_robot(config, tq, zc, hm);
        let stats = handle.join().unwrap();
        assert_eq!(stats.tasks_completed, 2);
    }
}
