// src/task_queue.rs
// ─────────────────────────────────────────────────────────────────────────────
// Thread-safe task queue for robot task dispatch.
// Uses Mutex + Condvar so worker robots can block-wait for new tasks.
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::fmt;

/// Represents the kind of work a robot can perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskKind {
    Delivery { from: String, to: String },
    Disinfection { zone: String },
    SurgicalAssist { room: String },
}

impl fmt::Display for TaskKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskKind::Delivery { from, to } => write!(f, "Delivery({} → {})", from, to),
            TaskKind::Disinfection { zone } => write!(f, "Disinfection({})", zone),
            TaskKind::SurgicalAssist { room } => write!(f, "SurgicalAssist({})", room),
        }
    }
}

/// A single task to be executed by a robot.
#[derive(Debug, Clone)]
pub struct Task {
    pub id: u64,
    pub kind: TaskKind,
    pub priority: u8, // 0 = highest priority
}

impl fmt::Display for Task {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Task#{} [pri={}] {}", self.id, self.priority, self.kind)
    }
}

/// Thread-safe, bounded task queue with condition-variable signalling.
pub struct TaskQueue {
    inner: Mutex<TaskQueueInner>,
    not_empty: Condvar,
    capacity: usize,
}

struct TaskQueueInner {
    queue: VecDeque<Task>,
    next_id: u64,
    closed: bool,
}

impl TaskQueue {
    /// Create a new task queue with the given capacity.
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(TaskQueueInner {
                queue: VecDeque::with_capacity(capacity),
                next_id: 1,
                closed: false,
            }),
            not_empty: Condvar::new(),
            capacity,
        })
    }

    /// Submit a task into the queue. Returns the assigned task ID.
    /// Returns `None` if the queue is full or closed.
    pub fn submit(&self, kind: TaskKind, priority: u8) -> Option<u64> {
        let mut inner = self.inner.lock().unwrap();
        if inner.closed || inner.queue.len() >= self.capacity {
            return None;
        }
        let id = inner.next_id;
        inner.next_id += 1;
        let task = Task { id, kind, priority };
        // Insert in priority order (lower number = higher priority).
        let pos = inner.queue.iter().position(|t| t.priority > priority);
        match pos {
            Some(i) => inner.queue.insert(i, task),
            None => inner.queue.push_back(task),
        }
        self.not_empty.notify_one();
        Some(id)
    }

    /// Fetch the next task, blocking until one is available.
    /// Returns `None` when the queue is closed and empty.
    pub fn fetch(&self) -> Option<Task> {
        let mut inner = self.inner.lock().unwrap();
        loop {
            if let Some(task) = inner.queue.pop_front() {
                return Some(task);
            }
            if inner.closed {
                return None;
            }
            inner = self.not_empty.wait(inner).unwrap();
        }
    }

    /// Try to fetch a task without blocking.
    pub fn try_fetch(&self) -> Option<Task> {
        let mut inner = self.inner.lock().unwrap();
        inner.queue.pop_front()
    }

    /// Close the queue so that waiting consumers wake up and exit.
    pub fn close(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.closed = true;
        self.not_empty.notify_all();
    }

    /// Returns the current number of pending tasks.
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().queue.len()
    }

    /// Returns `true` if no tasks are pending.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns `true` if the queue has been closed.
    pub fn is_closed(&self) -> bool {
        self.inner.lock().unwrap().closed
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_submit_and_fetch() {
        let q = TaskQueue::new(10);
        let id = q.submit(TaskKind::Disinfection { zone: "ICU".into() }, 1);
        assert!(id.is_some());
        let task = q.try_fetch().unwrap();
        assert_eq!(task.id, 1);
    }

    #[test]
    fn test_priority_ordering() {
        let q = TaskQueue::new(10);
        q.submit(TaskKind::Disinfection { zone: "A".into() }, 5);
        q.submit(TaskKind::Disinfection { zone: "B".into() }, 1);
        q.submit(TaskKind::Disinfection { zone: "C".into() }, 3);

        let t1 = q.try_fetch().unwrap();
        let t2 = q.try_fetch().unwrap();
        let t3 = q.try_fetch().unwrap();
        assert!(t1.priority <= t2.priority);
        assert!(t2.priority <= t3.priority);
    }

    #[test]
    fn test_capacity_limit() {
        let q = TaskQueue::new(2);
        assert!(q.submit(TaskKind::Disinfection { zone: "A".into() }, 1).is_some());
        assert!(q.submit(TaskKind::Disinfection { zone: "B".into() }, 1).is_some());
        assert!(q.submit(TaskKind::Disinfection { zone: "C".into() }, 1).is_none());
    }

    #[test]
    fn test_close_wakes_consumers() {
        let q = TaskQueue::new(10);
        let q2 = Arc::clone(&q);
        let handle = thread::spawn(move || q2.fetch());
        thread::sleep(std::time::Duration::from_millis(50));
        q.close();
        assert!(handle.join().unwrap().is_none());
    }

    #[test]
    fn test_concurrent_submit_fetch() {
        let q = TaskQueue::new(100);
        let mut handles = vec![];

        // 10 producers
        for i in 0..10 {
            let q = Arc::clone(&q);
            handles.push(thread::spawn(move || {
                for j in 0..10 {
                    q.submit(
                        TaskKind::Delivery {
                            from: format!("W{}", i),
                            to: format!("R{}", j),
                        },
                        (j % 5) as u8,
                    );
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(q.len(), 100);

        // 5 consumers drain the queue
        let q2 = Arc::clone(&q);
        let count = Arc::new(Mutex::new(0u64));
        let mut consumers = vec![];
        q.close(); // close after all submitted so fetch returns None when empty
        for _ in 0..5 {
            let q = Arc::clone(&q2);
            let count = Arc::clone(&count);
            consumers.push(thread::spawn(move || {
                while let Some(_task) = q.fetch() {
                    *count.lock().unwrap() += 1;
                }
            }));
        }
        for c in consumers {
            c.join().unwrap();
        }
        assert_eq!(*count.lock().unwrap(), 100);
    }
}
