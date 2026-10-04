//! Lifecycle für ressourcenintensive Dienste (LLM, Whisper, HTTP-Sessions …).
//!
//! ```text
//! OFF ─► LOADING ─► READY ─► ACTIVE ─► IDLE ─► UNLOADING ─► OFF
//!          │                   ▲         │
//!          ▼                   └─────────┘
//!        ERROR ─► (LOADING | OFF)
//! ```
//! Ein Dienst wird erst geladen, wenn ein Tool ihn braucht, und nach Ablauf
//! seines Idle-Timeouts wieder entladen.

use async_trait::async_trait;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ServiceState {
    Off,
    Loading,
    Ready,
    Active,
    Idle,
    Unloading,
    Error,
}

impl ServiceState {
    pub fn can_transition_to(self, next: ServiceState) -> bool {
        use ServiceState::*;
        matches!(
            (self, next),
            (Off, Loading)
                | (Loading, Ready)
                | (Loading, Error)
                | (Ready, Active)
                | (Ready, Unloading)
                | (Active, Idle)
                | (Active, Error)
                | (Idle, Active)
                | (Idle, Unloading)
                | (Unloading, Off)
                | (Unloading, Error)
                | (Error, Loading)
                | (Error, Off)
        )
    }
}

/// Ein Dienst, der geladen und entladen werden kann.
#[async_trait]
pub trait Service: Send + Sync {
    fn name(&self) -> &'static str;
    /// Nach dieser Zeit ohne Nutzung wird der Dienst entladen.
    fn idle_timeout(&self) -> Duration;
    async fn load(&self) -> Result<(), String>;
    async fn unload(&self) -> Result<(), String>;
}

struct Entry {
    service: Arc<dyn Service>,
    state: ServiceState,
    users: usize,
    last_used: Instant,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceStatus {
    pub name: &'static str,
    pub state: ServiceState,
    pub users: usize,
    pub idle_for_ms: u128,
    pub last_error: Option<String>,
}

/// Verwaltet alle Dienste und ihre Zustände.
#[derive(Clone, Default)]
pub struct ServiceManager {
    inner: Arc<Mutex<HashMap<&'static str, Entry>>>,
    load_lock: Arc<tokio::sync::Mutex<()>>,
}

/// Solange der Guard lebt, ist der Dienst ACTIVE; beim Drop wird er IDLE.
pub struct ServiceGuard {
    manager: ServiceManager,
    name: &'static str,
}

impl Drop for ServiceGuard {
    fn drop(&mut self) {
        let mut map = self.manager.inner.lock().unwrap();
        if let Some(e) = map.get_mut(self.name) {
            e.users = e.users.saturating_sub(1);
            e.last_used = Instant::now();
            if e.users == 0 && e.state == ServiceState::Active {
                e.state = ServiceState::Idle;
            }
        }
    }
}

impl ServiceManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, service: Arc<dyn Service>) {
        let mut map = self.inner.lock().unwrap();
        map.entry(service.name()).or_insert(Entry {
            service,
            state: ServiceState::Off,
            users: 0,
            last_used: Instant::now(),
            last_error: None,
        });
    }

    pub fn state(&self, name: &str) -> Option<ServiceState> {
        self.inner.lock().unwrap().get(name).map(|e| e.state)
    }

    pub fn status(&self) -> Vec<ServiceStatus> {
        let map = self.inner.lock().unwrap();
        let mut v: Vec<_> = map
            .iter()
            .map(|(n, e)| ServiceStatus {
                name: n,
                state: e.state,
                users: e.users,
                idle_for_ms: e.last_used.elapsed().as_millis(),
                last_error: e.last_error.clone(),
            })
            .collect();
        v.sort_by_key(|s| s.name);
        v
    }

    fn set(&self, name: &str, next: ServiceState) -> Result<(), String> {
        let mut map = self.inner.lock().unwrap();
        let e = map.get_mut(name).ok_or_else(|| format!("unbekannter Dienst '{name}'"))?;
        if e.state == next {
            return Ok(());
        }
        if !e.state.can_transition_to(next) {
            return Err(format!("ungültiger Übergang {:?} -> {:?} für '{name}'", e.state, next));
        }
        e.state = next;
        Ok(())
    }

    /// Lädt den Dienst bei Bedarf und markiert ihn als ACTIVE.
    pub async fn acquire(&self, name: &'static str) -> Result<ServiceGuard, String> {
        let _l = self.load_lock.lock().await;
        let (service, state) = {
            let map = self.inner.lock().unwrap();
            let e = map.get(name).ok_or_else(|| format!("unbekannter Dienst '{name}'"))?;
            (e.service.clone(), e.state)
        };
        if matches!(state, ServiceState::Off | ServiceState::Error) {
            self.set(name, ServiceState::Loading)?;
            match service.load().await {
                Ok(()) => self.set(name, ServiceState::Ready)?,
                Err(err) => {
                    self.set(name, ServiceState::Error)?;
                    self.inner.lock().unwrap().get_mut(name).unwrap().last_error = Some(err.clone());
                    return Err(format!("Dienst '{name}' konnte nicht geladen werden: {err}"));
                }
            }
        }
        let mut map = self.inner.lock().unwrap();
        let e = map.get_mut(name).unwrap();
        if matches!(e.state, ServiceState::Ready | ServiceState::Idle) {
            e.state = ServiceState::Active;
        }
        e.users += 1;
        e.last_used = Instant::now();
        e.last_error = None;
        drop(map);
        Ok(ServiceGuard { manager: self.clone(), name })
    }

    async fn unload_one(&self, name: &'static str) -> Result<(), String> {
        let service = {
            let map = self.inner.lock().unwrap();
            map.get(name).map(|e| e.service.clone()).ok_or("unbekannt")?
        };
        self.set(name, ServiceState::Unloading)?;
        match service.unload().await {
            Ok(()) => self.set(name, ServiceState::Off),
            Err(err) => {
                self.set(name, ServiceState::Error)?;
                self.inner.lock().unwrap().get_mut(name).unwrap().last_error = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Entlädt alle Dienste, deren Idle-Timeout abgelaufen ist.
    /// Gibt die Namen der entladenen Dienste zurück.
    pub async fn reap_idle(&self) -> Vec<&'static str> {
        let _l = self.load_lock.lock().await;
        let candidates: Vec<&'static str> = {
            let map = self.inner.lock().unwrap();
            map.iter()
                .filter(|(_, e)| {
                    e.users == 0
                        && matches!(e.state, ServiceState::Idle | ServiceState::Ready)
                        && e.last_used.elapsed() >= e.service.idle_timeout()
                })
                .map(|(n, _)| *n)
                .collect()
        };
        let mut done = vec![];
        for n in candidates {
            if self.unload_one(n).await.is_ok() {
                done.push(n);
            }
        }
        done
    }

    /// Entlädt sofort alle nicht genutzten Dienste (z. B. bei Speicherdruck).
    pub async fn unload_all_unused(&self) -> Vec<&'static str> {
        let _l = self.load_lock.lock().await;
        let candidates: Vec<&'static str> = {
            let map = self.inner.lock().unwrap();
            map.iter()
                .filter(|(_, e)| e.users == 0 && matches!(e.state, ServiceState::Idle | ServiceState::Ready))
                .map(|(n, _)| *n)
                .collect()
        };
        let mut done = vec![];
        for n in candidates {
            if self.unload_one(n).await.is_ok() {
                done.push(n);
            }
        }
        done
    }

    /// Startet einen Hintergrund-Task, der regelmäßig Idle-Dienste entlädt.
    pub fn spawn_reaper(&self, every: Duration) -> tokio::task::JoinHandle<()> {
        let me = self.clone();
        tokio::spawn(async move {
            let mut t = tokio::time::interval(every);
            loop {
                t.tick().await;
                me.reap_idle().await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct Dummy {
        loads: AtomicUsize,
        unloads: AtomicUsize,
        fail: AtomicBool,
        timeout: Duration,
    }

    #[async_trait]
    impl Service for Dummy {
        fn name(&self) -> &'static str {
            "dummy"
        }
        fn idle_timeout(&self) -> Duration {
            self.timeout
        }
        async fn load(&self) -> Result<(), String> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                Err("kaputt".into())
            } else {
                Ok(())
            }
        }
        async fn unload(&self) -> Result<(), String> {
            self.unloads.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn dummy(ms: u64) -> Arc<Dummy> {
        Arc::new(Dummy {
            loads: 0.into(),
            unloads: 0.into(),
            fail: false.into(),
            timeout: Duration::from_millis(ms),
        })
    }

    #[tokio::test]
    async fn full_lifecycle_off_to_off() {
        let d = dummy(0);
        let m = ServiceManager::new();
        m.register(d.clone());
        assert_eq!(m.state("dummy"), Some(ServiceState::Off));
        let g = m.acquire("dummy").await.unwrap();
        assert_eq!(m.state("dummy"), Some(ServiceState::Active));
        // Während aktiver Nutzung wird nicht entladen.
        assert!(m.reap_idle().await.is_empty());
        drop(g);
        assert_eq!(m.state("dummy"), Some(ServiceState::Idle));
        assert_eq!(m.reap_idle().await, vec!["dummy"]);
        assert_eq!(m.state("dummy"), Some(ServiceState::Off));
        assert_eq!(d.loads.load(Ordering::SeqCst), 1);
        assert_eq!(d.unloads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn idle_service_is_reused_without_reload() {
        let d = dummy(60_000);
        let m = ServiceManager::new();
        m.register(d.clone());
        drop(m.acquire("dummy").await.unwrap());
        drop(m.acquire("dummy").await.unwrap());
        assert_eq!(d.loads.load(Ordering::SeqCst), 1);
        assert!(m.reap_idle().await.is_empty(), "Timeout noch nicht erreicht");
        assert_eq!(m.unload_all_unused().await, vec!["dummy"]);
    }

    #[tokio::test]
    async fn load_failure_goes_to_error_and_recovers() {
        let d = dummy(0);
        d.fail.store(true, Ordering::SeqCst);
        let m = ServiceManager::new();
        m.register(d.clone());
        assert!(m.acquire("dummy").await.is_err());
        assert_eq!(m.state("dummy"), Some(ServiceState::Error));
        assert!(m.status()[0].last_error.is_some());
        d.fail.store(false, Ordering::SeqCst);
        let _g = m.acquire("dummy").await.unwrap();
        assert_eq!(m.state("dummy"), Some(ServiceState::Active));
    }

    #[test]
    fn invalid_transitions_rejected() {
        use ServiceState::*;
        assert!(!Off.can_transition_to(Active));
        assert!(!Active.can_transition_to(Off));
        assert!(!Idle.can_transition_to(Off));
        assert!(Idle.can_transition_to(Unloading));
    }
}
