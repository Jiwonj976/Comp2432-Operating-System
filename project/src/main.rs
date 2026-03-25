// src/main.rs
// ─────────────────────────────────────────────────────────────────────────────
// Project Blaze — Medical Care Robot Coordination System
//
// Demonstration entry point. Runs the full scenario:
//   1. Initialise shared resources (task queue, zone controller, health monitor)
//   2. Spawn multiple robot worker threads
//   3. Submit a batch of tasks
//   4. Show concurrent zone access (mutual exclusion)
//   5. Show robot timeout detection (heartbeat failure)
//   6. Collect and display statistics
// ─────────────────────────────────────────────────────────────────────────────

use std::sync::Arc;
use std::time::{Duration, Instant};

use project::health_monitor::HealthMonitor;
use project::robot::{spawn_robot, RobotConfig};
use project::task_queue::{TaskKind, TaskQueue};
use project::zone_control::ZoneController;  

fn main() {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║      Project Blaze — Medical Care Robot Coordination        ║");
    println!("║                  COMP2432 · OS Concepts Demo                ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();

    // ── Configuration ────────────────────────────────────────────────────
    let num_robots = 4;
    let num_tasks = 20;
    let queue_capacity = 32;
    let heartbeat_timeout = Duration::from_millis(300);
    let monitor_interval = Duration::from_millis(100);

    // ── 1. Initialise shared resources ──────────────────────────────────
    let task_queue = TaskQueue::new(queue_capacity);
    let zone_ctrl = ZoneController::new();
    let health_monitor = HealthMonitor::new(heartbeat_timeout);

    // Pre-register hospital zones — intentionally few to force contention
    let zones = ["ICU", "ER", "OR-1"];
    zone_ctrl.register_zones(&zones);
    println!("[System] Registered {} hospital zones: {:?}", zones.len(), zones);
    println!("[System] NOTE: Only {} zones for {} robots → expect contention", zones.len(), num_robots);

    // Set up offline callback → evict robot from all zones
    let zc_for_cb = Arc::clone(&zone_ctrl);
    health_monitor.set_offline_callback(Arc::new(move |robot_id: &str| {
        let evicted = zc_for_cb.evict_robot(robot_id);
        if !evicted.is_empty() {
            println!(
                "[System] Evicted offline robot {} from zones: {:?}",
                robot_id, evicted
            );
        }
    }));

    // Start background health monitor
    let _monitor_handle = health_monitor.start_monitoring(monitor_interval);
    println!("[System] Health monitor started (timeout={:?})", heartbeat_timeout);

    // ── 2. Spawn robot workers ──────────────────────────────────────────
    let mut robot_handles = Vec::new();

    for i in 0..num_robots {
        let robot_id = format!("Robot-{}", i + 1);
        health_monitor.register(&robot_id);

        let config = RobotConfig {
            id: robot_id.clone(),
            work_time_ms: (100, 200),
            heartbeat_interval_ms: 80,
            // Robot-4 will simulate a failure after 3 heartbeats
            simulate_failure_after: if i == num_robots - 1 { Some(3) } else { None },
        };

        println!(
            "[System] Spawning {} {}",
            robot_id,
            if config.simulate_failure_after.is_some() {
                "(will simulate failure)"
            } else {
                ""
            }
        );

        let handle = spawn_robot(
            config,
            Arc::clone(&task_queue),
            Arc::clone(&zone_ctrl),
            Arc::clone(&health_monitor),
        );
        robot_handles.push((robot_id, handle));
    }

    println!();

    // ── 3. Submit tasks ─────────────────────────────────────────────────
    let start = Instant::now();
    // Only 3 zones — multiple robots will fight for the same zones
    let zone_names = ["ICU", "ER", "OR-1"];

    println!("[System] Submitting {} tasks (targeting {} zones — high contention)...", num_tasks, zone_names.len());
    println!("─────────────────────────────────────────────────────────────────");
    for i in 0..num_tasks {
        let kind = match i % 3 {
            0 => TaskKind::Delivery {
                from: "Warehouse".into(),
                to: zone_names[i % zone_names.len()].into(),
            },
            1 => TaskKind::Disinfection {
                zone: zone_names[i % zone_names.len()].into(),
            },
            _ => TaskKind::SurgicalAssist {
                room: zone_names[i % zone_names.len()].into(),
            },
        };
        let priority = (i % 4) as u8;
        if let Some(id) = task_queue.submit(kind.clone(), priority) {
            println!("  [QUEUE] Task#{:02} [pri={}] {}", id, priority, kind);
        }
    }
    println!("─────────────────────────────────────────────────────────────────");

    // Close the queue after all tasks are submitted
    task_queue.close();
    println!("[System] Task queue closed — robots will drain remaining tasks");
    println!();

    // ── 4. Wait for all robots to finish ────────────────────────────────
    let mut total_completed = 0u32;
    let mut total_waits = 0u32;
    let mut total_denials = 0u32;

    println!("\n[System] All tasks submitted — waiting for robots to finish...");
    println!("─────────────────────────────────────────────────────────────────");
    for (robot_id, handle) in robot_handles {
        match handle.join() {
            Ok(stats) => {
                println!(
                    "[Stats] {:8} | completed: {:2} | zone_defers: {:2} | zone_denials: {:2}",
                    robot_id, stats.tasks_completed, stats.zone_waits, stats.zone_denials
                );
                total_completed += stats.tasks_completed;
                total_waits += stats.zone_waits;
                total_denials += stats.zone_denials;
            }
            Err(_) => {
                println!("[Stats] {}: thread panicked", robot_id);
            }
        }
    }
    println!("─────────────────────────────────────────────────────────────────");

    let elapsed = start.elapsed();

    // Give the health monitor time to detect any robots that stopped heartbeating
    println!("[System] Waiting for health monitor to settle...");
    std::thread::sleep(std::time::Duration::from_millis(500));

    // ── 5. Health snapshot ───────────────────────────────────────────────
    println!();
    println!("╔══════════════════════════════════════════════════════════════════╗");
    println!("║                     FINAL SYSTEM REPORT                         ║");
    println!("╠══════════════════════════════════════════════════════════════════╣");
    println!("║  Tasks submitted        : {:>4}                                  ║", num_tasks);
    println!("║  Tasks completed        : {:>4}                                  ║", total_completed);
    println!("║  Zone contention events : {:>4}  (tasks deferred, not wasted)    ║", total_waits);
    println!("║  Zone access denials    : {:>4}  (timed out, task skipped)       ║", total_denials);
    println!("║  Elapsed time           : {:>8.1?}                              ║", elapsed);
    println!("╠══════════════════════════════════════════════════════════════════╣");
    println!("║  Robot Health Snapshot                                           ║");
    println!("╠══════════════════════════════════════════════════════════════════╣");

    let mut snapshot = health_monitor.snapshot();
    snapshot.sort_by(|a, b| a.robot_id.cmp(&b.robot_id));
    for rh in &snapshot {
        let marker = if rh.status == project::health_monitor::RobotStatus::Offline { "OFFLINE" } else { "Online " };
        println!(
            "║  {:<9} : {}  (heartbeats sent: {:>3})                    ║",
            rh.robot_id, marker, rh.total_heartbeats
        );
    }

    println!("╚══════════════════════════════════════════════════════════════════╝");

    // Stop the monitor
    health_monitor.stop();
    println!("\n[System] Coordination system shut down cleanly.");
}
