//! Tray-resident settings window.

use crate::config::{Config, Mode, Subscription, APP_NAME};
use crate::core::{self, Core, LogBuffer, TrafficMonitor};
use crate::killswitch::KillSwitch;
use crate::link;
use crate::handoff;
use crate::presets;
use crate::profile::{Outbound, Profile, TorTransport};
use crate::tor::Tor;
use crate::tor_browser;

use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use egui::{Color32, RichText, Ui};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use std::time::{Duration, Instant};
use uikit::theme::{BAD, GOOD, MUTED, WARN};
use uikit::tray::{self, CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, TrayEvents};
use uikit::widgets;
use uikit::{icon, tray_icon::TrayIcon};

const ACCENT: Color32 = Color32::from_rgb(167, 139, 250);
const HEALTH_INTERVAL: Duration = Duration::from_secs(1);

/// The window has two states: the one control that matters, and everywhere
/// else. Keeping them apart is what lets the main screen stay a single button.
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Home,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Status,
    Profiles,
    Routing,
    Tor,
    Log,
}

impl Tab {
    const ALL: [(Tab, &'static str); 5] = [
        (Tab::Status, "상태"),
        (Tab::Profiles, "프로필"),
        (Tab::Routing, "라우팅"),
        (Tab::Tor, "Tor"),
        (Tab::Log, "로그"),
    ];
}

#[derive(Clone, Copy, PartialEq)]
enum StatusKind {
    Idle,
    Good,
    Warn,
    Bad,
}

/// Everything a running tunnel owns.
struct Tunnel {
    core: Core,
    tor: Option<Tor>,
    kill: Option<KillSwitch>,
    traffic: TrafficMonitor,
}

struct TrayMenu {
    toggle: CheckMenuItem,
    open: MenuItem,
    quit: MenuItem,
}

pub struct VeilApp {
    config: Config,
    tunnel: Option<Tunnel>,
    log: LogBuffer,
    /// The core died while the kill switch was up. Traffic stays blocked until
    /// the user decides — silently restoring it would leak exactly when it
    /// matters most.
    stranded: Option<KillSwitch>,
    /// A tor daemon running on its own, for browsers only. Deliberately
    /// separate from the tunnel: it changes nothing about system routing.
    tor_only: Option<Tor>,

    tray: TrayIcon,
    tray_menu: TrayMenu,
    events: TrayEvents,
    _hotkeys: Option<GlobalHotKeyManager>,
    hotkey_rx: Receiver<GlobalHotKeyEvent>,

    view: View,
    tab: Tab,
    status: String,
    status_kind: StatusKind,
    last_health: Instant,

    /// Profile currently shown as a QR code, if any.
    qr_for: Option<usize>,
    link_draft: String,
    sub_name: String,
    sub_url: String,
    direct_domain: String,
    proxy_domain: String,
    direct_process: String,
    bridge_draft: String,
    notes: Vec<(String, bool)>,
    fetch: Option<Receiver<Result<String, String>>>,
    quitting: bool,
}

impl VeilApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> anyhow::Result<Self> {
        uikit::theme::install(&cc.egui_ctx, ACCENT);

        let config = Config::load();
        let autostart = config.start_on_launch;
        let hotkey_spec = config.hotkey.clone();

        let events = tray::install_handlers(&cc.egui_ctx);
        let (menu, tray_menu) = build_menu()?;
        let tray = tray::build("Veil — 연결 끊김", &icon::veil(false), menu)?;
        let (_hotkeys, hotkey_rx) = install_hotkey(&hotkey_spec, &cc.egui_ctx);

        let mut notes = Vec::new();
        // A previous crash can leave the firewall denying everything.
        if crate::killswitch::recover_if_needed() {
            notes.push(("이전 실행이 남긴 킬 스위치를 해제했습니다".to_string(), true));
        }

        let mut app = Self {
            config,
            tunnel: None,
            log: core::new_log_buffer(),
            stranded: None,
            tor_only: None,
            tray,
            tray_menu,
            events,
            _hotkeys,
            hotkey_rx,
            view: View::Home,
            tab: Tab::Status,
            status: "연결되지 않음".to_string(),
            status_kind: StatusKind::Idle,
            last_health: Instant::now(),
            qr_for: None,
            link_draft: String::new(),
            sub_name: String::new(),
            sub_url: String::new(),
            direct_domain: String::new(),
            proxy_domain: String::new(),
            direct_process: String::new(),
            bridge_draft: String::new(),
            notes,
            fetch: None,
            quitting: false,
        };

        if autostart {
            app.start();
        }
        Ok(app)
    }

    fn running(&self) -> bool {
        self.tunnel.is_some()
    }

    fn set_status(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.status_kind = kind;
        self.status = message.into();
    }

    fn start(&mut self) {
        if self.running() {
            return;
        }
        if self.stranded.is_some() {
            self.set_status(StatusKind::Bad, "킬 스위치가 걸려 있습니다. 먼저 해제하거나 재연결하세요");
        }
        let Some(profile) = self.config.active_profile().cloned() else {
            self.set_status(StatusKind::Bad, "선택된 프로필이 없습니다");
            return;
        };

        // Tor first, so it bootstraps while the core comes up. sing-box retries
        // the SOCKS upstream until the circuit is ready.
        let tor = if profile.uses_tor() {
            match Tor::start(&self.config.tor) {
                Ok(t) => Some(t),
                Err(e) => {
                    self.set_status(StatusKind::Bad, format!("tor를 시작할 수 없습니다: {e:#}"));
                    return;
                }
            }
        } else {
            None
        };

        let core = match Core::start(&self.config, &profile, self.log.clone()) {
            Ok(c) => c,
            Err(e) => {
                self.set_status(StatusKind::Bad, format!("{e:#}"));
                return;
            }
        };

        // Reuse whatever the previous failure left engaged rather than
        // stacking a second set of rules on top of it.
        let kill = self.stranded.take().or_else(|| {
            if !self.config.kill_switch {
                return None;
            }
            match core::locate_binary().and_then(|b| KillSwitch::engage(&b, self.config.block_ipv6)) {
                Ok(k) => Some(k),
                Err(e) => {
                    tracing::error!("kill switch failed: {e:#}");
                    self.notes.push((format!("킬 스위치를 켤 수 없습니다: {e}"), false));
                    None
                }
            }
        });

        let traffic = TrafficMonitor::start(self.config.clash_api_port);
        self.tunnel = Some(Tunnel { core, tor, kill, traffic });
        self.set_status(StatusKind::Good, format!("{} 연결됨", profile.name));
        self.refresh_tray();
    }

    /// User-initiated stop: the kill switch comes down with the tunnel.
    fn stop(&mut self) {
        if let Some(mut tunnel) = self.tunnel.take() {
            // Restore the network first so a later failure cannot strand it.
            if let Some(mut kill) = tunnel.kill.take() {
                kill.disengage();
            }
            tunnel.core.stop();
            if let Some(tor) = tunnel.tor.as_mut() {
                tor.stop();
            }
        }
        self.set_status(StatusKind::Idle, "연결되지 않음");
        self.refresh_tray();
    }

    /// The tunnel died on its own. Keep the kill switch up: that is the entire
    /// point of having one.
    fn on_failure(&mut self, reason: String) {
        if let Some(mut tunnel) = self.tunnel.take() {
            tunnel.core.stop();
            if let Some(tor) = tunnel.tor.as_mut() {
                tor.stop();
            }
            self.stranded = tunnel.kill.take();
        }
        let suffix = if self.stranded.is_some() {
            " — 킬 스위치가 트래픽을 차단하고 있습니다"
        } else {
            ""
        };
        self.set_status(StatusKind::Bad, format!("{reason}{suffix}"));
        self.refresh_tray();
    }

    fn release_killswitch(&mut self) {
        if let Some(mut kill) = self.stranded.take() {
            kill.disengage();
            self.notes.push(("킬 스위치를 해제했습니다. 트래픽이 보호 없이 나갑니다".to_string(), false));
        }
        self.refresh_tray();
    }

    fn toggle(&mut self) {
        if self.running() {
            self.stop();
        } else {
            self.start();
        }
    }

    /// Start a tor daemon on its own, without touching system routing.
    fn start_tor_only(&mut self) {
        if self.tor_only.is_some() || self.tunnel.as_ref().is_some_and(|t| t.tor.is_some()) {
            return;
        }
        match Tor::start(&self.config.tor) {
            Ok(tor) => {
                self.tor_only = Some(tor);
                self.note("브라우저용 tor를 시작했습니다", true);
            }
            Err(e) => self.note(format!("tor를 시작할 수 없습니다: {e:#}"), false),
        }
    }

    fn stop_tor_only(&mut self) {
        if let Some(mut tor) = self.tor_only.take() {
            tor.stop();
            self.note("브라우저용 tor를 중지했습니다", true);
        }
    }

    /// The SOCKS port of whichever tor is running, if any.
    fn tor_socks_port(&self) -> Option<u16> {
        let ready = self.tor_only.is_some() || self.tunnel.as_ref().is_some_and(|t| t.tor.is_some());
        ready.then_some(self.config.tor.socks_port)
    }

    fn refresh_tray(&self) {
        let art = if self.running() {
            match self.status_kind {
                StatusKind::Warn => icon::warn(false),
                _ => icon::veil(true),
            }
        } else if self.stranded.is_some() {
            icon::warn(false)
        } else {
            icon::veil(false)
        };
        tray::set_icon(&self.tray, &art);
        let tooltip = match (&self.tunnel, &self.stranded) {
            (Some(_), _) => "Veil — 연결됨".to_string(),
            (None, Some(_)) => "Veil — 차단됨 (킬 스위치)".to_string(),
            _ => "Veil — 연결 끊김".to_string(),
        };
        let _ = self.tray.set_tooltip(Some(tooltip));
        self.tray_menu.toggle.set_checked(self.running());
    }

    fn save_config(&mut self) {
        self.config.clamp_active();
        if let Err(e) = self.config.save() {
            tracing::error!("could not save config: {e}");
        }
    }

    fn note(&mut self, message: impl Into<String>, good: bool) {
        self.notes.push((message.into(), good));
        if self.notes.len() > 8 {
            self.notes.remove(0);
        }
    }

    fn import_link(&mut self) {
        let text = self.link_draft.trim().to_string();
        if text.is_empty() {
            return;
        }
        let (profiles, errors) = link::parse_subscription(&text);
        let added = profiles.len();
        self.config.profiles.extend(profiles);
        self.config.clamp_active();
        self.save_config();
        self.link_draft.clear();

        if added > 0 {
            self.note(format!("{added}개 프로필을 추가했습니다"), true);
        }
        for e in errors.into_iter().take(3) {
            self.note(e, false);
        }
    }

    fn refresh_subscription(&mut self, index: usize) {
        let Some((name, url)) =
            self.config.subscriptions.get(index).map(|s| (s.name.clone(), s.url.clone()))
        else {
            return;
        };
        self.note(format!("{name} 갱신 중…"), true);
        self.fetch = Some(fetch(url));
    }

    fn pump_fetch(&mut self) {
        let Some(rx) = &self.fetch else { return };
        let Ok(result) = rx.try_recv() else { return };
        self.fetch = None;
        match result {
            Ok(body) => {
                let (profiles, errors) = link::parse_subscription(&body);
                if profiles.is_empty() {
                    self.note("구독에서 프로필을 찾지 못했습니다".to_string(), false);
                } else {
                    let count = profiles.len();
                    self.config.profiles.extend(profiles);
                    self.config.clamp_active();
                    self.save_config();
                    self.note(format!("구독에서 {count}개를 가져왔습니다"), true);
                }
                for e in errors.into_iter().take(2) {
                    self.note(e, false);
                }
            }
            Err(e) => self.note(format!("구독을 가져올 수 없습니다: {e}"), false),
        }
    }

    fn pump_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.events.menu.try_recv() {
            let id = &event.id;
            if id == self.tray_menu.toggle.id() {
                self.toggle();
                self.tray_menu.toggle.set_checked(self.running());
            } else if id == self.tray_menu.open.id() {
                tray::show_window(ctx);
            } else if id == self.tray_menu.quit.id() {
                self.quitting = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        while let Ok(event) = self.events.tray.try_recv() {
            if tray::is_activation(&event) {
                tray::show_window(ctx);
            }
        }
        while self.hotkey_rx.try_recv().is_ok() {
            self.toggle();
        }
    }

    fn check_health(&mut self) {
        if self.last_health.elapsed() < HEALTH_INTERVAL {
            return;
        }
        self.last_health = Instant::now();

        let failure = self.tunnel.as_mut().and_then(|tunnel| {
            if let Err(e) = tunnel.core.health() {
                return Some(format!("{e:#}"));
            }
            if let Some(tor) = tunnel.tor.as_mut() {
                if let Err(e) = tor.health() {
                    return Some(format!("{e:#}"));
                }
            }
            None
        });
        if let Some(reason) = failure {
            self.on_failure(reason);
            return;
        }

        // Tor is reachable but not yet usable until the circuit is built; say so
        // rather than reporting a healthy connection that drops every request.
        let bootstrap = self.tunnel.as_ref().and_then(|t| t.tor.as_ref()).map(Tor::progress);
        match bootstrap {
            Some(progress) if progress < 100 => {
                self.set_status(StatusKind::Warn, format!("Tor 부트스트랩 {progress}%"));
                self.refresh_tray();
            }
            Some(_) if self.status_kind == StatusKind::Warn => {
                self.set_status(StatusKind::Good, "Tor 회로 준비됨");
                self.refresh_tray();
            }
            _ => {}
        }
    }
}

impl eframe::App for VeilApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump_events(ctx);
        self.pump_fetch();
        self.check_health();

        if uikit::tray::handle_close(ctx, self.quitting) {
            self.save_config();
        }
        if self.quitting {
            self.stop();
            self.stop_tor_only();
            self.release_killswitch();
            self.save_config();
        }

        if self.running() || self.fetch.is_some() || self.tor_only.is_some() {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        match self.view {
            View::Home => self.home(ui),
            View::Settings => self.settings(ui),
        }
    }
}

impl VeilApp {
    /// One button, the profile it will use, and the throughput.
    fn home(&mut self, ui: &mut Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(30.0);
                ui.label(RichText::new("V E I L").size(13.0).color(MUTED).strong());
                ui.add_space(32.0);

                if widgets::power_button(ui, self.running(), ACCENT, 152.0).clicked() {
                    self.toggle();
                }

                ui.add_space(24.0);
                let bootstrap = self.tunnel.as_ref().and_then(|t| t.tor.as_ref()).map(Tor::progress);
                let (headline, colour) = match (self.running(), self.status_kind) {
                    (_, StatusKind::Bad) => ("문제 발생", BAD),
                    (true, StatusKind::Warn) => ("연결 중", WARN),
                    (true, _) => ("연결됨", GOOD),
                    (false, _) => ("끊김", MUTED),
                };
                ui.label(RichText::new(headline).size(20.0).color(colour).strong());

                // Which server this will use — the one thing you must choose.
                match self.config.active_profile() {
                    Some(profile) => {
                        ui.label(
                            RichText::new(format!("{} · {}", profile.name, profile.outbound.protocol_label()))
                                .size(12.0)
                                .color(MUTED),
                        );
                    }
                    None => {
                        ui.label(RichText::new("프로필이 없습니다 — 설정에서 추가하세요").size(12.0).color(WARN));
                    }
                }
                if let Some(progress) = bootstrap.filter(|p| *p < 100) {
                    ui.add_space(6.0);
                    ui.add(
                        egui::ProgressBar::new(progress as f32 / 100.0)
                            .desired_width(220.0)
                            .text(format!("Tor 회로 구성 {progress}%")),
                    );
                }

                ui.add_space(30.0);
                let sample = self.tunnel.as_ref().map(|t| t.traffic.sample()).unwrap_or_default();
                ui.columns(3, |columns| {
                    widgets::stat(&mut columns[0], &core::format_rate(sample.down_bps), "다운로드", ACCENT);
                    widgets::stat(&mut columns[1], &core::format_rate(sample.up_bps), "업로드", ACCENT);
                    widgets::stat(&mut columns[2], &sample.total.connections.to_string(), "연결", ACCENT);
                });

                if self.stranded.is_some() {
                    ui.add_space(18.0);
                    ui.label(RichText::new("킬 스위치가 모든 트래픽을 차단 중입니다").color(BAD).strong());
                    ui.horizontal(|ui| {
                        ui.add_space(ui.available_width() / 2.0 - 90.0);
                        if ui.button("네트워크 복구").clicked() {
                            self.release_killswitch();
                        }
                        if ui.button("재연결").clicked() {
                            self.start();
                        }
                    });
                }

                ui.add_space(26.0);
                if widgets::ghost_button(ui, "설정").clicked() {
                    self.view = View::Settings;
                }
                for (note, good) in self.notes.iter().rev().take(2) {
                    ui.label(RichText::new(note).size(11.0).color(if *good { GOOD } else { WARN }));
                }
            });
        });
    }

    fn settings(&mut self, ui: &mut Ui) {
        egui::Panel::top("settings-header").show(ui, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("← 돌아가기").clicked() {
                    self.view = View::Home;
                }
                ui.label(RichText::new("설정").color(MUTED));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (label, colour) = match self.status_kind {
                        StatusKind::Good => ("연결됨", GOOD),
                        StatusKind::Warn => ("주의", WARN),
                        StatusKind::Bad => ("오류", BAD),
                        StatusKind::Idle => ("끊김", MUTED),
                    };
                    ui.label(RichText::new(label).color(colour).small());
                });
            });
            if !self.status.is_empty() && self.status_kind != StatusKind::Idle {
                let colour = match self.status_kind {
                    StatusKind::Bad => BAD,
                    StatusKind::Warn => WARN,
                    _ => MUTED,
                };
                ui.label(RichText::new(&self.status).color(colour).small());
            }
            for (note, good) in self.notes.iter().rev().take(3) {
                ui.label(RichText::new(note).color(if *good { GOOD } else { WARN }).small());
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                for (tab, label) in Tab::ALL {
                    ui.selectable_value(&mut self.tab, tab, label);
                }
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| match self.tab {
                Tab::Status => self.status_tab(ui),
                Tab::Profiles => self.profiles_tab(ui),
                Tab::Routing => self.routing_tab(ui),
                Tab::Tor => self.tor_tab(ui),
                Tab::Log => self.log_tab(ui),
            });
        });
    }

    fn status_tab(&mut self, ui: &mut Ui) {
        ui.add_space(6.0);
        section(ui, "현재 프로필", |ui| match self.config.active_profile() {
            Some(profile) => {
                ui.label(RichText::new(&profile.name).strong());
                ui.label(
                    RichText::new(format!(
                        "{} · {}",
                        profile.outbound.protocol_label(),
                        profile.outbound.endpoint()
                    ))
                    .color(MUTED),
                );
                let (tier, caveat) = profile.outbound.resistance();
                ui.label(RichText::new(tier).color(GOOD).small());
                ui.label(RichText::new(caveat).color(MUTED).small());
            }
            None => {
                ui.label(RichText::new("프로필 탭에서 서버를 추가하세요.").color(MUTED));
            }
        });

        ui.add_space(10.0);
        section(ui, "트래픽", |ui| match &self.tunnel {
            Some(tunnel) => {
                let sample = tunnel.traffic.sample();
                egui::Grid::new("traffic").num_columns(4).spacing([20.0, 4.0]).show(ui, |ui| {
                    ui.label(RichText::new("업로드").color(MUTED).small());
                    ui.label(RichText::new(core::format_rate(sample.up_bps)).strong());
                    ui.label(RichText::new("다운로드").color(MUTED).small());
                    ui.label(RichText::new(core::format_rate(sample.down_bps)).strong());
                    ui.end_row();
                    ui.label(RichText::new("누적 송신").color(MUTED).small());
                    ui.label(core::format_bytes(sample.total.up));
                    ui.label(RichText::new("누적 수신").color(MUTED).small());
                    ui.label(core::format_bytes(sample.total.down));
                    ui.end_row();
                    ui.label(RichText::new("연결 수").color(MUTED).small());
                    ui.label(sample.total.connections.to_string());
                    ui.end_row();
                });
                if let Some(tor) = &tunnel.tor {
                    let progress = tor.progress();
                    ui.add_space(4.0);
                    ui.add(egui::ProgressBar::new(progress as f32 / 100.0).text(format!("Tor 부트스트랩 {progress}%")));
                }
            }
            None => {
                ui.label(RichText::new("연결되어 있지 않습니다.").color(MUTED));
            }
        });

        ui.add_space(10.0);
        let mut changed = false;
        section(ui, "보호", |ui| {
            changed |= ui
                .checkbox(&mut self.config.kill_switch, "킬 스위치 (터널이 끊기면 전체 차단)")
                .on_hover_text("터널이 죽은 뒤 사용자가 알아차리기 전까지가 실제 주소가 새는 구간입니다.")
                .changed();
            changed |= ui
                .checkbox(&mut self.config.block_ipv6, "IPv6 차단")
                .on_hover_text("터널이 v4 전용일 때, 남아 있는 v6 경로가 터널을 통째로 우회합니다.")
                .changed();
            ui.horizontal(|ui| {
                ui.label("모드");
                for mode in [Mode::Tun, Mode::Proxy] {
                    changed |= ui.selectable_value(&mut self.config.mode, mode, mode.label()).changed();
                }
            });
            ui.label(
                RichText::new(
                    "TUN은 프록시를 무시하는 앱까지 포함해 전부 잡습니다. 로컬 프록시는 드라이버가 \
                     필요 없지만 프록시를 인식하는 앱만 보호되고 나머지는 그대로 샙니다.",
                )
                .color(MUTED)
                .small(),
            );
            ui.horizontal(|ui| {
                ui.label("TUN 스택");
                for stack in ["system", "gvisor", "mixed"] {
                    let mut selected = self.config.tun_stack == stack;
                    if ui.selectable_label(selected, stack).clicked() {
                        selected = true;
                        self.config.tun_stack = stack.to_string();
                        changed = true;
                    }
                    let _ = selected;
                }
            });
            ui.label(
                RichText::new("system이 가장 빠르고, gvisor는 느리지만 호환성이 높으며, mixed는 TCP만 system을 씁니다")
                    .color(MUTED)
                    .small(),
            );
            changed |= ui.checkbox(&mut self.config.start_on_launch, "실행 시 자동 연결").changed();
        });
        if changed {
            self.save_config();
            if self.running() {
                self.note("변경 사항은 재연결 후 적용됩니다".to_string(), true);
            }
        }
    }

    fn profiles_tab(&mut self, ui: &mut Ui) {
        ui.add_space(6.0);
        let mut changed = false;

        section(ui, "서버 추가", |ui| {
            ui.label(
                RichText::new("공유 링크나 구독 내용을 붙여넣으세요 (vless·trojan·ss·hysteria2, 여러 줄 가능)")
                    .color(MUTED)
                    .small(),
            );
            ui.add(egui::TextEdit::multiline(&mut self.link_draft).desired_rows(3).desired_width(f32::INFINITY));
            ui.horizontal(|ui| {
                if ui.button("가져오기").clicked() {
                    self.import_link();
                }
                if ui.button("Tor 프로필 추가").clicked() {
                    self.config.profiles.push(Profile {
                        name: "Tor".to_string(),
                        outbound: Outbound::Tor {
                            transport: self.config.tor.transport,
                            bridges: Vec::new(),
                        },
                    });
                    changed = true;
                    self.note("Tor 프로필을 추가했습니다".to_string(), true);
                }
            });
        });

        ui.add_space(10.0);
        section(ui, "프로필", |ui| {
            if self.config.profiles.is_empty() {
                ui.label(RichText::new("아직 없습니다.").color(MUTED).small());
            }
            let mut remove = None;
            let mut select = None;
            let mut phone = None;
            for (index, profile) in self.config.profiles.iter().enumerate() {
                ui.horizontal(|ui| {
                    let active = self.config.active == Some(index);
                    if ui.radio(active, "").clicked() {
                        select = Some(index);
                    }
                    ui.label(RichText::new(&profile.name).strong());
                    ui.label(
                        RichText::new(format!(
                            "{} · {}",
                            profile.outbound.protocol_label(),
                            profile.outbound.endpoint()
                        ))
                        .color(MUTED)
                        .small(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("삭제").clicked() {
                            remove = Some(index);
                        }
                        if !profile.uses_tor() {
                            if ui.small_button("폰으로").on_hover_text("QR 코드와 설정 파일").clicked() {
                                phone = Some(index);
                            }
                        }
                    });
                });
            }
            if let Some(index) = phone {
                self.qr_for = if self.qr_for == Some(index) { None } else { Some(index) };
            }
            if let Some(index) = self.qr_for {
                self.phone_handoff(ui, index, &mut changed);
            }
            if let Some(index) = select {
                self.config.active = Some(index);
                changed = true;
            }
            if let Some(index) = remove {
                self.config.profiles.remove(index);
                changed = true;
            }
        });

        ui.add_space(10.0);
        section(ui, "구독", |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.sub_name).hint_text("이름").desired_width(120.0));
                ui.add(egui::TextEdit::singleline(&mut self.sub_url).hint_text("https://…").desired_width(300.0));
                if ui.button("추가").clicked() && !self.sub_url.trim().is_empty() {
                    let name = if self.sub_name.trim().is_empty() {
                        "구독".to_string()
                    } else {
                        self.sub_name.trim().to_string()
                    };
                    self.config.subscriptions.push(Subscription {
                        name,
                        url: self.sub_url.trim().to_string(),
                    });
                    self.sub_name.clear();
                    self.sub_url.clear();
                    changed = true;
                }
            });
            let mut remove = None;
            let mut refresh = None;
            for (index, sub) in self.config.subscriptions.iter().enumerate() {
                ui.horizontal(|ui| {
                    if ui.small_button("삭제").clicked() {
                        remove = Some(index);
                    }
                    if ui.small_button("갱신").clicked() {
                        refresh = Some(index);
                    }
                    ui.label(RichText::new(&sub.name).strong());
                    ui.label(RichText::new(&sub.url).color(MUTED).small());
                });
            }
            if let Some(index) = refresh {
                self.refresh_subscription(index);
            }
            if let Some(index) = remove {
                self.config.subscriptions.remove(index);
                changed = true;
            }
        });

        if changed {
            self.save_config();
        }
    }

    /// QR code and file export for moving a profile to a phone.
    fn phone_handoff(&mut self, ui: &mut Ui, index: usize, changed: &mut bool) {
        let Some(profile) = self.config.profiles.get(index).cloned() else { return };
        let link = match handoff::share_link(&profile) {
            Ok(l) => l,
            Err(e) => {
                ui.label(RichText::new(format!("{e}")).color(WARN).small());
                return;
            }
        };

        ui.add_space(6.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(format!("{} — 폰으로 옮기기", profile.name)).strong());
                ui.label(
                    RichText::new("폰의 클라이언트 앱에서 QR 스캔 또는 클립보드 가져오기")
                        .color(MUTED)
                        .small(),
                );
                ui.add_space(6.0);
                match handoff::qr(&link) {
                    Ok(matrix) => draw_qr(ui, &matrix, 240.0),
                    Err(e) => {
                        ui.label(RichText::new(format!("QR 생성 실패: {e}")).color(BAD).small());
                    }
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("링크 복사").clicked() {
                        ui.ctx().copy_text(link.clone());
                        self.note("링크를 복사했습니다", true);
                    }
                    if ui
                        .button("설정 파일 내보내기")
                        .on_hover_text("라우팅 규칙까지 포함한 설정과 도메인 목록을 파일로 저장합니다")
                        .clicked()
                    {
                        let folder = uikit::config::app_dir(crate::config::APP_NAME)
                            .join("phone")
                            .join(sanitise(&profile.name));
                        match handoff::export(&self.config, &profile, &folder) {
                            Ok(out) => {
                                self.note(format!("{} 에 저장했습니다", out.folder.display()), true);
                                open_folder(&out.folder);
                            }
                            Err(e) => self.note(format!("{e:#}"), false),
                        }
                    }
                    if ui.button("닫기").clicked() {
                        self.qr_for = None;
                    }
                });
                ui.label(
                    RichText::new("이 링크는 서버 접속 권한 그 자체입니다. 공개 채널에 올리지 마세요.")
                        .color(WARN)
                        .small(),
                );
            });
        });
        let _ = changed;
    }

    fn routing_tab(&mut self, ui: &mut Ui) {
        ui.add_space(6.0);
        let mut changed = false;

        section(ui, "기본 정책", |ui| {
            changed |= ui
                .radio_value(&mut self.config.routing.default_proxy, true, "기본 터널, 예외만 직결")
                .changed();
            changed |= ui
                .radio_value(&mut self.config.routing.default_proxy, false, "기본 직결, 지정만 터널")
                .changed();
            changed |= ui
                .checkbox(&mut self.config.routing.bypass_private, "사설 대역 직결 (LAN·프린터·NAS)")
                .changed();
            changed |= ui
                .checkbox(&mut self.config.routing.block_quic, "QUIC 차단")
                .on_hover_text("여러 프록시 프로토콜이 UDP를 잘 나르지 못합니다. 조용히 실패하는 QUIC보다 조금 느린 TCP가 낫습니다.")
                .changed();
        });

        ui.add_space(10.0);
        section(ui, "직결 도메인", |ui| {
            ui.label(
                RichText::new(
                    "해외 주소에서 접속하면 깨지거나 차단되는 곳입니다. 은행·증권·정부 사이트는 \
                     외국 IP를 사기로 간주해 계정을 잠그거나 인증서 로그인을 거부합니다.",
                )
                .color(MUTED)
                .small(),
            );
            ui.horizontal(|ui| {
                if ui
                    .button("한국 금융·공공 추가")
                    .on_hover_text("은행·카드·증권·결제·정부(go.kr) 도메인을 한 번에 넣습니다. 꼭 넣으세요.")
                    .clicked()
                {
                    let n = presets::merge_into(
                        &mut self.config.routing.direct_domains,
                        presets::KOREAN_FINANCE,
                    );
                    changed = true;
                    self.note(
                        if n > 0 { format!("금융·공공 도메인 {n}개를 추가했습니다") } else { "이미 모두 들어 있습니다".to_string() },
                        true,
                    );
                }
                if ui
                    .button("국내 서비스 추가")
                    .on_hover_text("네이버·카카오·쿠팡 등. 해외로 돌리면 느려지기만 하는 곳들입니다.")
                    .clicked()
                {
                    let n = presets::merge_into(
                        &mut self.config.routing.direct_domains,
                        presets::KOREAN_DOMESTIC,
                    );
                    changed = true;
                    self.note(
                        if n > 0 { format!("국내 서비스 {n}개를 추가했습니다") } else { "이미 모두 들어 있습니다".to_string() },
                        true,
                    );
                }
            });
            ui.add_space(4.0);
            changed |= string_list(ui, &mut self.config.routing.direct_domains, &mut self.direct_domain, "example.com");
        });

        ui.add_space(10.0);
        section(ui, "직결 프로세스", |ui| {
            changed |= string_list(ui, &mut self.config.routing.direct_processes, &mut self.direct_process, "game.exe");
        });

        ui.add_space(10.0);
        section(ui, "강제 터널 도메인", |ui| {
            changed |= string_list(ui, &mut self.config.routing.proxy_domains, &mut self.proxy_domain, "blocked.example");
        });

        ui.add_space(10.0);
        section(ui, "DNS", |ui| {
            ui.label(
                RichText::new("터널을 통과하는 이름은 터널 안에서 조회되므로 로컬 네트워크는 무엇을 찾는지 볼 수 없습니다")
                    .color(MUTED)
                    .small(),
            );
            ui.horizontal(|ui| {
                ui.label("터널 측");
                changed |= ui.text_edit_singleline(&mut self.config.dns.remote).changed();
            });
            ui.horizontal(|ui| {
                ui.label("직결 측");
                changed |= ui.text_edit_singleline(&mut self.config.dns.local).changed();
            });
            ui.horizontal(|ui| {
                ui.label("전략");
                for strategy in ["prefer_ipv4", "ipv4_only", "prefer_ipv6"] {
                    if ui.selectable_label(self.config.dns.strategy == strategy, strategy).clicked() {
                        self.config.dns.strategy = strategy.to_string();
                        changed = true;
                    }
                }
            });
        });

        if changed {
            self.save_config();
        }
    }

    fn tor_tab(&mut self, ui: &mut Ui) {
        ui.add_space(6.0);
        let mut changed = false;

        section(ui, "브라우저용 Tor", |ui| {
            ui.label(
                RichText::new(
                    "브라우저만 Tor로 보낼 때 씁니다. 터널·TUN·킬 스위치를 건드리지 않고 tor 데몬만 \
                     띄우므로, 나머지 트래픽은 평소와 똑같이 나갑니다.",
                )
                .color(MUTED)
                .small(),
            );
            ui.horizontal(|ui| {
                match &self.tor_only {
                    Some(tor) => {
                        let progress = tor.progress();
                        if ui.button("중지").clicked() {
                            self.stop_tor_only();
                        }
                        ui.add(
                            egui::ProgressBar::new(progress as f32 / 100.0)
                                .desired_width(200.0)
                                .text(format!("부트스트랩 {progress}%")),
                        );
                    }
                    None => {
                        if ui.button("Tor 시작").clicked() {
                            self.start_tor_only();
                        }
                        ui.label(RichText::new("아래 브라우저 실행 전에 먼저 켜세요").color(MUTED).small());
                    }
                }
            });
        });

        ui.add_space(10.0);
        section(ui, "Chrome 계열로 Tor 사용", |ui| {
            ui.label(
                RichText::new(
                    "지정한 브라우저 인스턴스에만 프록시를 걸어 실행합니다. 시스템 라우팅을 전혀 \
                     건드리지 않으므로 평소 쓰던 브라우저 창은 아무 영향도 받지 않습니다 — \
                     'chrome.exe를 터널에서 제외'하는 방식보다 정확합니다. 전용 프로필로 열려서 \
                     기존 쿠키·기록과도 분리됩니다.",
                )
                .color(MUTED)
                .small(),
            );
            let socks = self.tor_socks_port();
            let browsers = tor_browser::find_chromium();
            if browsers.is_empty() {
                ui.label(RichText::new("Chrome·Brave·Edge를 찾지 못했습니다.").color(MUTED).small());
            }
            ui.horizontal(|ui| {
                for browser in &browsers {
                    if ui
                        .add_enabled(socks.is_some(), egui::Button::new(format!("{} 실행", browser.name)))
                        .clicked()
                    {
                        let dir = uikit::config::app_dir(crate::config::APP_NAME)
                            .join("browser-profiles")
                            .join(browser.name);
                        match tor_browser::launch_via_socks(browser, socks.unwrap_or(0), &dir) {
                            Ok(()) => self.notes.push((format!("{} 을(를) Tor로 실행했습니다", browser.name), true)),
                            Err(e) => self.notes.push((format!("{e:#}"), false)),
                        }
                    }
                }
            });
            if socks.is_none() && !browsers.is_empty() {
                ui.label(RichText::new("먼저 위에서 Tor를 시작하세요").color(WARN).small());
            }
            ui.label(
                RichText::new(
                    "다만 IP만 가려집니다. 브라우저 지문 — 글꼴·화면 크기·캔버스·시간대·확장 목록 — 은 \
                     그대로라 추적에는 여전히 노출됩니다. 진짜 익명성이 필요하면 아래 Tor Browser를 쓰세요.",
                )
                .color(WARN)
                .small(),
            );
        });

        ui.add_space(10.0);
        section(ui, "Tor Browser", |ui| {
            ui.label(
                RichText::new(
                    "Tor Browser는 Firefox 기반만 존재합니다 — Chrome 버전은 없습니다. 대신 모든 \
                     사용자가 똑같아 보이도록 지문을 표준화하는데, 그건 네트워크 계층에서는 해결할 수 \
                     없는 부분이라 익명 브라우징에는 이쪽이 정답입니다.",
                )
                .color(MUTED)
                .small(),
            );
            match tor_browser::locate() {
                Some(install) => {
                    ui.label(RichText::new(tor_browser::describe(&install)).color(MUTED).small());
                    if ui.button("Tor Browser 실행").clicked() {
                        // It brings its own tor; sending it through Veil's would
                        // put Tor inside Tor.
                        if tor_browser::ensure_bypassed(&install, &mut self.config.routing.direct_process_paths) {
                            changed = true;
                            self.notes.push((
                                "Tor Browser 실행 파일 경로만 직결로 지정했습니다 (일반 Firefox는 영향 없음)"
                                    .to_string(),
                                true,
                            ));
                        }
                        match tor_browser::launch(&install) {
                            Ok(()) => self.notes.push(("Tor Browser를 실행했습니다".to_string(), true)),
                            Err(e) => self.notes.push((format!("{e:#}"), false)),
                        }
                    }
                }
                None => {
                    ui.label(RichText::new("설치를 찾지 못했습니다.").color(MUTED).small());
                    if ui.button("다운로드 페이지 열기").clicked() {
                        if let Err(e) = tor_browser::open_download_page() {
                            self.notes.push((format!("{e:#}"), false));
                        }
                    }
                }
            }
        });

        ui.add_space(10.0);
        section(ui, "트랜스포트", |ui| {
            ui.label(
                RichText::new("아래는 Veil 자체 Tor 프로필용입니다 — 브라우저가 아닌 앱까지 Tor로 태울 때 씁니다.")
                    .color(MUTED)
                    .small(),
            );
            ui.label(
                RichText::new(
                    "Tor는 서버를 믿을 필요가 없는 유일한 선택지입니다. 어떤 릴레이도 출발지와 \
                     목적지를 동시에 알지 못합니다. 대가는 3홉만큼의 지연입니다.",
                )
                .color(MUTED)
                .small(),
            );
            ui.horizontal(|ui| {
                for transport in TorTransport::ALL {
                    changed |= ui
                        .selectable_value(&mut self.config.tor.transport, *transport, transport.label())
                        .changed();
                }
            });
            let hint = match self.config.tor.transport {
                TorTransport::None => "Tor 자체가 차단되지 않은 곳에서만 동작합니다",
                TorTransport::Obfs4 => "기본 브리지가 내장되어 있어 바로 쓸 수 있습니다",
                TorTransport::WebTunnel => {
                    "가장 잘 통하지만 기본 브리지가 없습니다 — bridges.torproject.org에서 받아 아래에 입력하세요"
                }
                TorTransport::Snowflake => "기본 브리지 내장. 자원봉사자 WebRTC 프록시를 경유합니다",
            };
            ui.label(RichText::new(hint).color(MUTED).small());
        });

        ui.add_space(10.0);
        section(ui, "브리지", |ui| {
            ui.label(RichText::new("비워 두면 번들에 포함된 기본 브리지를 사용합니다").color(MUTED).small());
            ui.add(
                egui::TextEdit::multiline(&mut self.bridge_draft)
                    .hint_text("obfs4 1.2.3.4:443 FINGERPRINT cert=… iat-mode=0")
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
            );
            ui.horizontal(|ui| {
                if ui.button("추가").clicked() {
                    let added: Vec<String> = self
                        .bridge_draft
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(str::to_string)
                        .collect();
                    if !added.is_empty() {
                        self.config.tor.bridges.extend(added);
                        self.bridge_draft.clear();
                        changed = true;
                    }
                }
                if ui.button("전부 지우기").clicked() && !self.config.tor.bridges.is_empty() {
                    self.config.tor.bridges.clear();
                    changed = true;
                }
            });
            let mut remove = None;
            for (index, bridge) in self.config.tor.bridges.iter().enumerate() {
                ui.horizontal(|ui| {
                    if ui.small_button("삭제").clicked() {
                        remove = Some(index);
                    }
                    ui.label(RichText::new(truncate(bridge, 90)).monospace().small());
                });
            }
            if let Some(index) = remove {
                self.config.tor.bridges.remove(index);
                changed = true;
            }
        });

        ui.add_space(10.0);
        section(ui, "포트", |ui| {
            ui.horizontal(|ui| {
                ui.label("SOCKS");
                changed |= ui.add(egui::DragValue::new(&mut self.config.tor.socks_port)).changed();
                ui.label("Control");
                changed |= ui.add(egui::DragValue::new(&mut self.config.tor.control_port)).changed();
            });
        });

        if changed {
            self.save_config();
        }
    }

    fn log_tab(&mut self, ui: &mut Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("코어 출력").strong().color(ACCENT));
            if ui.small_button("지우기").clicked() {
                self.log.lock().clear();
            }
        });
        let lines: Vec<String> = self.log.lock().iter().rev().take(150).cloned().collect();
        if lines.is_empty() {
            ui.label(RichText::new("출력이 없습니다.").color(MUTED).small());
        }
        for line in lines {
            let colour = if line.contains("ERROR") || line.contains("FATAL") {
                BAD
            } else if line.contains("WARN") {
                WARN
            } else {
                MUTED
            };
            ui.label(RichText::new(line).color(colour).monospace().small());
        }

        if let Some(tunnel) = &self.tunnel {
            if tunnel.tor.is_some() {
                ui.add_space(10.0);
                ui.label(RichText::new("Tor 출력").strong().color(ACCENT));
                let lines: Vec<String> = tunnel
                    .tor
                    .as_ref()
                    .map(|t| t.log.lock().iter().rev().take(60).cloned().collect())
                    .unwrap_or_default();
                for line in lines {
                    ui.label(RichText::new(line).color(MUTED).monospace().small());
                }
            }
        }
    }
}

// --- helpers ---------------------------------------------------------------

/// Paint a QR matrix. Drawn as rectangles rather than an image so there is no
/// texture to upload or bitmap dependency to carry.
fn draw_qr(ui: &mut Ui, matrix: &handoff::QrMatrix, size: f32) {
    // A quiet zone is part of the spec; scanners fail without it.
    const QUIET: usize = 3;
    let modules = matrix.size + QUIET * 2;
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, Color32::WHITE);

    let cell = size / modules as f32;
    for y in 0..matrix.size {
        for x in 0..matrix.size {
            if !matrix.is_dark(x, y) {
                continue;
            }
            let corner = rect.min
                + egui::Vec2::new((x + QUIET) as f32 * cell, (y + QUIET) as f32 * cell);
            painter.rect_filled(
                egui::Rect::from_min_size(corner, egui::Vec2::splat(cell)),
                0.0,
                Color32::BLACK,
            );
        }
    }
}

/// Strip characters a folder name cannot contain.
fn sanitise(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if r#"\/:*?"<>|"#.contains(c) { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() { "profile".to_string() } else { trimmed.to_string() }
}

fn open_folder(path: &std::path::Path) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("explorer")
        .arg(path)
        .creation_flags(0x0800_0000)
        .spawn();
}

fn section(ui: &mut Ui, title: &str, body: impl FnOnce(&mut Ui)) {
    ui.label(RichText::new(title).strong().color(ACCENT));
    ui.add_space(2.0);
    egui::Frame::group(ui.style()).show(ui, body);
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!("{}…", text.chars().take(max).collect::<String>())
}

fn string_list(ui: &mut Ui, list: &mut Vec<String>, draft: &mut String, hint: &str) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let entry = ui.add(egui::TextEdit::singleline(draft).hint_text(hint).desired_width(280.0));
        let submitted = entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.button("추가").clicked() || submitted) && !draft.trim().is_empty() {
            list.push(draft.trim().to_string());
            draft.clear();
            changed = true;
        }
    });
    let mut remove = None;
    for (index, item) in list.iter().enumerate() {
        ui.horizontal(|ui| {
            if ui.small_button("삭제").clicked() {
                remove = Some(index);
            }
            ui.label(item);
        });
    }
    if let Some(index) = remove {
        list.remove(index);
        changed = true;
    }
    changed
}

/// Fetch a subscription body off the UI thread.
fn fetch(url: String) -> Receiver<Result<String, String>> {
    let (tx, rx): (Sender<_>, Receiver<_>) = crossbeam_channel::bounded(1);
    std::thread::Builder::new()
        .name("veil-subscription".to_string())
        .spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
                .and_then(|rt| {
                    rt.block_on(async {
                        let client = reqwest::Client::builder()
                            .timeout(Duration::from_secs(20))
                            .user_agent("veil/0.1")
                            .build()
                            .map_err(|e| e.to_string())?;
                        let response = client.get(&url).send().await.map_err(|e| e.to_string())?;
                        if !response.status().is_success() {
                            return Err(format!("HTTP {}", response.status()));
                        }
                        response.text().await.map_err(|e| e.to_string())
                    })
                });
            let _ = tx.send(result);
        })
        .ok();
    rx
}

fn build_menu() -> anyhow::Result<(Menu, TrayMenu)> {
    let menu = Menu::new();
    let toggle = CheckMenuItem::new("연결", true, false, None);
    let open = MenuItem::new("설정 열기", true, None);
    let quit = MenuItem::new("종료", true, None);
    menu.append(&toggle)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&open)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit)?;
    Ok((menu, TrayMenu { toggle, open, quit }))
}

fn install_hotkey(
    spec: &str,
    ctx: &egui::Context,
) -> (Option<GlobalHotKeyManager>, Receiver<GlobalHotKeyEvent>) {
    let (tx, rx) = crossbeam_channel::unbounded();
    let repaint = ctx.clone();
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if event.state == global_hotkey::HotKeyState::Pressed {
            let _ = tx.send(event);
            repaint.request_repaint();
        }
    }));

    let manager = match GlobalHotKeyManager::new() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("global hotkeys unavailable: {e}");
            return (None, rx);
        }
    };
    match spec.parse::<global_hotkey::hotkey::HotKey>() {
        Ok(hotkey) => {
            if let Err(e) = manager.register(hotkey) {
                tracing::warn!("could not register hotkey {spec}: {e}");
            }
        }
        Err(e) => tracing::warn!("hotkey {spec} is not valid: {e}"),
    }
    (Some(manager), rx)
}

pub fn run() -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Veil")
            .with_inner_size([500.0, 640.0])
            .with_min_inner_size([440.0, 540.0])
            .with_visible(false),
        ..Default::default()
    };
    eframe::run_native(APP_NAME, options, Box::new(|cc| Ok(Box::new(VeilApp::new(cc)?))))
        .map_err(|e| anyhow::anyhow!("UI를 시작할 수 없습니다: {e}"))
}
