//! Bounded admission and per-tool concurrency (SPEC §6, §9.1).

use crate::records::Role;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Global and per-tool concurrency limits.
///
/// "The runtime enforces bounded concurrency and tool-specific limits."
#[derive(Debug, Clone)]
pub struct ConcurrencyLimits {
    global: Arc<Semaphore>,
    per_tool: HashMap<String, Arc<Semaphore>>,
    /// Tool execution is separately limited, since a subprocess is far more
    /// expensive than a model call.
    tools: Arc<Semaphore>,
    global_max: usize,
}

impl Default for ConcurrencyLimits {
    fn default() -> Self {
        Self::new(4, 1)
    }
}

impl ConcurrencyLimits {
    pub fn new(global: usize, tools: usize) -> Self {
        Self {
            global: Arc::new(Semaphore::new(global.max(1))),
            per_tool: HashMap::new(),
            tools: Arc::new(Semaphore::new(tools.max(1))),
            global_max: global.max(1),
        }
    }

    /// Set a limit for one named tool.
    pub fn with_tool_limit(mut self, tool: &str, limit: usize) -> Self {
        self.per_tool
            .insert(tool.to_string(), Arc::new(Semaphore::new(limit.max(1))));
        self
    }

    pub fn global_max(&self) -> usize {
        self.global_max
    }

    /// Currently available global slots.
    pub fn available(&self) -> usize {
        self.global.available_permits()
    }

    /// Acquire the permits a task needs before it may run.
    ///
    /// Held for the task's whole lifetime, so a cancelled or panicking task
    /// still releases its slot when the guard drops.
    pub async fn acquire(&self, role: Role, tool: Option<&str>) -> Permits {
        let global = self
            .global
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore is never closed");

        let tool_permit = if role == Role::Tool {
            let specific = match tool.and_then(|t| self.per_tool.get(t)) {
                Some(sem) => Some(
                    sem.clone()
                        .acquire_owned()
                        .await
                        .expect("semaphore is never closed"),
                ),
                None => None,
            };
            let general = self
                .tools
                .clone()
                .acquire_owned()
                .await
                .expect("semaphore is never closed");
            Some((general, specific))
        } else {
            None
        };

        Permits {
            _global: global,
            _tool: tool_permit,
        }
    }
}

/// Held for a task's lifetime; releases its slots on drop.
#[derive(Debug)]
pub struct Permits {
    _global: OwnedSemaphorePermit,
    _tool: Option<(OwnedSemaphorePermit, Option<OwnedSemaphorePermit>)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn global_limit_bounds_concurrency() {
        let limits = ConcurrencyLimits::new(2, 1);
        let peak = Arc::new(AtomicUsize::new(0));
        let live = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let (limits, peak, live) = (limits.clone(), peak.clone(), live.clone());
            handles.push(tokio::spawn(async move {
                let _permit = limits.acquire(Role::Reflection, None).await;
                let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                live.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert!(peak.load(Ordering::SeqCst) <= 2, "global limit exceeded");
    }

    #[tokio::test]
    async fn tool_limit_is_separate_from_global() {
        let limits = ConcurrencyLimits::new(4, 1);
        let peak = Arc::new(AtomicUsize::new(0));
        let live = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..4 {
            let (limits, peak, live) = (limits.clone(), peak.clone(), live.clone());
            handles.push(tokio::spawn(async move {
                let _permit = limits.acquire(Role::Tool, Some("shell")).await;
                let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                live.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(peak.load(Ordering::SeqCst), 1, "tool limit must be 1");
    }

    #[tokio::test]
    async fn unrelated_work_progresses_while_a_tool_runs() {
        let limits = ConcurrencyLimits::new(4, 1);
        let tool_permit = limits.acquire(Role::Tool, None).await;

        // A model task must not be blocked by the tool's slot being taken.
        let acquired = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            limits.acquire(Role::Reflection, None),
        )
        .await;
        assert!(
            acquired.is_ok(),
            "unrelated work was blocked by a tool task"
        );
        drop(tool_permit);
    }
}
