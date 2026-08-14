//! The program without a face.
//!
//! Switching the bypass on, keeping the encrypted DNS beside it, and putting the
//! machine's own settings back on the way out — none of that has anything to do
//! with how it is drawn. It lived inside the settings window because that was
//! the only window there was; the shell that is replacing it needs the same
//! thing, and two copies of "start the engine" would be two places for it to be
//! wrong.
//!
//! So it is here, and both faces drive it.

use crate::doh::Forwarder;
use crate::engine::{Engine, Shared};
use crate::sysdns;
use std::sync::Arc;

/// How the last thing that happened should read.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum StatusKind {
    Idle,
    Good,
    Warn,
    Bad,
}

/// The engine, the DNS forwarder beside it, and what to say about them.
pub struct EngineCore {
    pub shared: Arc<Shared>,
    engine: Option<Engine>,
    doh: Option<Forwarder>,
    /// What the machine's DNS was before we changed it, so it can be put back.
    saved_dns: Vec<sysdns::Saved>,
    pub status: String,
    pub status_kind: StatusKind,
}

/// What the machine's DNS was before we changed it, kept where anything can
/// reach it.
///
/// The setting outlives this process — it is written into the adapter — so
/// putting it back cannot depend on a tidy exit. The window's session-end
/// handler and the guard below both come here, and both are safe to run twice.
static SAVED_DNS: std::sync::Mutex<Vec<sysdns::Saved>> = std::sync::Mutex::new(Vec::new());

fn remember_dns(saved: &[sysdns::Saved]) {
    if let Ok(mut kept) = SAVED_DNS.lock() {
        *kept = saved.to_vec();
    }
}

fn forget_dns() {
    if let Ok(mut kept) = SAVED_DNS.lock() {
        kept.clear();
    }
}

/// Put the machine's DNS back, wherever we are called from.
///
/// Called on the way out, and again when Windows says the session is ending —
/// which does not run any of the ordinary teardown. Doing nothing when there is
/// nothing to put back is what makes calling it twice safe.
pub fn restore_system_dns() {
    let Ok(mut kept) = SAVED_DNS.lock() else { return };
    if kept.is_empty() {
        return;
    }
    if let Err(e) = sysdns::restore(&kept) {
        tracing::warn!("DNS restore failed: {e}");
    }
    kept.clear();
}

impl Drop for EngineCore {
    /// The last chance. A panic unwinds past every tidy path, and the DNS is the
    /// one thing we changed that the machine keeps after we are gone.
    fn drop(&mut self) {
        restore_system_dns();
    }
}

impl EngineCore {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            engine: None,
            doh: None,
            saved_dns: Vec::new(),
            status: "대기 중".to_string(),
            status_kind: StatusKind::Idle,
        }
    }

    pub fn running(&self) -> bool {
        self.engine.is_some()
    }

    pub fn set_status(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.status_kind = kind;
        self.status = message.into();
    }

    pub fn start(&mut self) {
        if self.running() {
            return;
        }
        match Engine::start(self.shared.clone()) {
            Ok(engine) => {
                self.engine = Some(engine);
                self.set_status(StatusKind::Good, "우회 동작 중");
            }
            Err(e) => {
                self.set_status(StatusKind::Bad, format!("{e:#}"));
                tracing::error!("engine start failed: {e:#}");
                return;
            }
        }
        self.start_dns();
    }

    fn start_dns(&mut self) {
        let doh_cfg = self.shared.config.read().doh.clone();
        if !doh_cfg.enabled {
            return;
        }
        match Forwarder::start(doh_cfg.clone(), self.shared.clone()) {
            Ok(forwarder) => {
                self.doh = Some(forwarder);
                if doh_cfg.set_system_dns {
                    match sysdns::snapshot() {
                        Ok(saved) => {
                            let server = sysdns::server_from_listen(&doh_cfg.listen).to_string();
                            // Never remember our own address as what the machine
                            // had. A run that ended without putting the setting
                            // back leaves the adapter pointing here; recording
                            // that as "the original" would make every later exit
                            // restore it, and the machine could never resolve
                            // anything again without Shard running.
                            let saved: Vec<sysdns::Saved> = saved
                                .into_iter()
                                .filter(|entry| !sysdns::points_at(entry, &server))
                                .collect();
                            if let Err(e) = sysdns::apply(&server, &saved) {
                                self.set_status(
                                    StatusKind::Warn,
                                    format!("시스템 DNS 변경 실패: {e}"),
                                );
                            } else {
                                remember_dns(&saved);
                                self.saved_dns = saved;
                            }
                        }
                        Err(e) => self.set_status(
                            StatusKind::Warn,
                            format!("DNS 설정을 읽을 수 없습니다: {e}"),
                        ),
                    }
                }
            }
            Err(e) => {
                // The bypass still works without encrypted DNS; say so rather
                // than failing the whole start.
                self.set_status(StatusKind::Warn, format!("우회는 동작하지만 DoH 실패: {e:#}"));
                tracing::warn!("DoH start failed: {e:#}");
            }
        }
    }

    pub fn stop(&mut self) {
        if let Some(mut engine) = self.engine.take() {
            engine.stop();
        }
        if let Some(mut doh) = self.doh.take() {
            doh.stop();
        }
        if !self.saved_dns.is_empty() {
            if let Err(e) = sysdns::restore(&self.saved_dns) {
                tracing::warn!("DNS restore failed: {e}");
            }
            self.saved_dns.clear();
        }
        forget_dns();
        self.set_status(StatusKind::Idle, "정지됨");
    }

    /// Start again, so a setting that is only read at start takes effect.
    ///
    /// Does nothing when it is not running: the new value will be read when it
    /// next starts, and stopping something that was already stopped to "apply" a
    /// setting would be a surprise.
    pub fn restart_if_running(&mut self) {
        if self.running() {
            self.stop();
            self.start();
        }
    }

    pub fn toggle(&mut self) {
        if self.running() {
            self.stop();
        } else {
            self.start();
        }
    }

    pub fn save_config(&self) {
        if let Err(e) = self.shared.config.read().save() {
            tracing::error!("could not save config: {e}");
        }
    }

    /// The two lines the home screen shows, whichever face is drawing it.
    ///
    /// Worked out here rather than in each face: they are a reading of the same
    /// state, and two readings of it would drift.
    pub fn headline(&self) -> (&'static str, &'static str) {
        match (self.running(), self.status_kind) {
            (_, StatusKind::Bad) => ("문제 발생", "bad"),
            (true, StatusKind::Warn) => ("동작 중 · 주의", "warn"),
            (true, _) => ("우회 중", "good"),
            (false, _) => ("꺼짐", "idle"),
        }
    }

    /// The line under it: what to do, or what went wrong.
    pub fn detail(&self) -> String {
        if self.running() {
            "브라우저에서 평소처럼 접속하면 됩니다".to_string()
        } else if self.status_kind == StatusKind::Bad {
            self.status.clone()
        } else {
            "버튼을 눌러 시작하세요".to_string()
        }
    }

    /// The third line, while it is running: whether it is watching for sites
    /// that get blocked, which is the one thing about it worth knowing without
    /// opening the settings.
    pub fn note(&self) -> &'static str {
        if !self.running() {
            return "";
        }
        if self.shared.config.read().auto_learn {
            "막히는 사이트는 알아서 감지하고 학습합니다"
        } else {
            "자동 학습이 꺼져 있습니다 — 설정에서 켤 수 있습니다"
        }
    }
}
