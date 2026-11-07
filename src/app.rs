use crate::adb::{self, Device};
use crate::config::{Config, Tab};
use crate::pairing::{self, PairEvent, PairHandle};
use crate::qr;
use crate::save::{self, SyncPlan};
use eframe::egui;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug)]
enum StatusKind {
    Success,
    Error,
}

#[derive(Clone, Debug)]
enum Attempt {
    Transfer {
        source: String,
        target: String,
        source_name: String,
        target_name: String,
    },
    AutoSync {
        left: String,
        right: String,
        left_name: String,
        right_name: String,
    },
}

enum JobEvent {
    Progress(String),
    Finished(JobResult),
}

enum JobResult {
    Success(String),
    Failure(String),
    GameRunning {
        attempt: Attempt,
        serials: Vec<String>,
    },
}

enum WorkerFailure {
    GameRunning(Vec<String>),
    Failed(String),
}

#[derive(Clone)]
struct PendingGame {
    attempt: Attempt,
    serials: Vec<String>,
}

struct QrState {
    tab: Tab,
    slot: Slot,
    texture: Option<egui::TextureHandle>,
    handle: Option<PairHandle>,
    status: String,
    error: Option<String>,
}

pub struct SaveSyncApp {
    config: Config,
    tab: Tab,
    devices: Vec<Device>,
    adb_version: Option<String>,
    adb_error: Option<String>,
    last_refresh: Instant,
    busy: Option<String>,
    receiver: Option<Receiver<JobEvent>>,
    status: Option<(StatusKind, String)>,
    pending_game: Option<PendingGame>,
    qr: Option<QrState>,
    about_open: bool,
    startup_prompt_done: bool,
}

impl SaveSyncApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let config = Config::load();
        let tab = if config.remember_tab {
            config.last_tab
        } else {
            Tab::Manual
        };

        let mut app = Self {
            config,
            tab,
            devices: Vec::new(),
            adb_version: None,
            adb_error: None,
            last_refresh: Instant::now(),
            busy: None,
            receiver: None,
            status: None,
            pending_game: None,
            qr: None,
            about_open: false,
            startup_prompt_done: false,
        };

        app.refresh_devices();
        if app.adb_error.is_none() {
            app.reconnect_saved();
        }
        app
    }

    fn refresh_devices(&mut self) {
        self.last_refresh = Instant::now();

        match adb::adb_version() {
            Ok(version) => {
                self.adb_version = Some(version);
                self.adb_error = None;
            }
            Err(error) => {
                self.adb_version = None;
                self.adb_error = Some(error);
                self.devices.clear();
                return;
            }
        }

        let services = adb::mdns_services();
        match adb::list_devices() {
            Ok(devices) => self.devices = adb::dedupe_devices(devices, &services),
            Err(error) => {
                self.devices.clear();
                self.adb_error = Some(error);
            }
        }
    }

    /// Reconnect remembered `ip:port` devices such as Waydroid, which does not
    /// keep an mDNS transport alive between sessions.
    fn reconnect_saved(&mut self) {
        if !self.config.remember_devices {
            return;
        }

        let mut candidates: Vec<String> = Vec::new();
        for serial in [
            self.config.manual_left.as_ref(),
            self.config.manual_right.as_ref(),
            self.config.auto_left.as_ref(),
            self.config.auto_right.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            if adb::is_ip_serial(serial) && !candidates.contains(serial) {
                candidates.push(serial.clone());
            }
        }

        let mut connected = false;
        for serial in candidates {
            if self.device_online(&serial) {
                continue;
            }
            if adb::connect(&serial).is_ok() {
                connected = true;
            }
        }

        if connected {
            self.refresh_devices();
        }
    }

    fn prompt_missing(&mut self, ctx: &egui::Context) {
        if !self.config.remember_devices || self.adb_error.is_some() {
            return;
        }

        for slot in [Slot::Left, Slot::Right] {
            let Some(serial) = self.slot_serial(self.tab, slot).cloned() else {
                continue;
            };
            if !self.device_online(&serial) {
                self.open_qr(ctx, self.tab, slot);
                return;
            }
        }
    }

    fn slot_serial(&self, tab: Tab, slot: Slot) -> Option<&String> {
        match (tab, slot) {
            (Tab::Manual, Slot::Left) => self.config.manual_left.as_ref(),
            (Tab::Manual, Slot::Right) => self.config.manual_right.as_ref(),
            (Tab::Automatic, Slot::Left) => self.config.auto_left.as_ref(),
            (Tab::Automatic, Slot::Right) => self.config.auto_right.as_ref(),
        }
    }

    fn set_slot_device(&mut self, tab: Tab, slot: Slot, serial: Option<String>) {
        match (tab, slot) {
            (Tab::Manual, Slot::Left) => self.config.manual_left = serial,
            (Tab::Manual, Slot::Right) => self.config.manual_right = serial,
            (Tab::Automatic, Slot::Left) => self.config.auto_left = serial,
            (Tab::Automatic, Slot::Right) => self.config.auto_right = serial,
        }
        self.config.save();
    }

    fn device_online(&self, serial: &str) -> bool {
        self.devices
            .iter()
            .any(|device| device.serial == serial && device.is_online())
    }

    fn device_name(&self, serial: &str) -> String {
        self.devices
            .iter()
            .find(|device| device.serial == serial)
            .map(Device::display_name)
            .unwrap_or_else(|| short_serial(serial).to_string())
    }

    fn picker_text(&self, serial: Option<&String>) -> String {
        match serial {
            None => "Select a device".to_string(),
            Some(serial) if self.device_online(serial) => self.device_name(serial),
            Some(serial) => format!("{} (not connected)", self.device_name(serial)),
        }
    }

    fn actions_enabled(&self) -> bool {
        self.adb_error.is_none() && self.busy.is_none() && self.pending_game.is_none()
    }

    fn start_attempt(&mut self, attempt: Attempt) {
        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        self.busy = Some("Checking both devices...".to_string());
        self.status = None;
        std::thread::spawn(move || worker(attempt, sender));
    }

    fn start_stop_and_retry(&mut self, attempt: Attempt, serials: Vec<String>) {
        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        self.busy = Some("Stopping the game...".to_string());
        self.status = None;
        std::thread::spawn(move || stop_worker(attempt, serials, sender));
    }

    fn start_transfer(&mut self, ctx: &egui::Context) {
        if !self.actions_enabled() {
            return;
        }

        let source = self.slot_serial(self.tab, Slot::Left).cloned();
        let target = self.slot_serial(self.tab, Slot::Right).cloned();

        let Some(source) = source else {
            self.status = Some((
                StatusKind::Error,
                "Select a source device first.".to_string(),
            ));
            self.open_qr(ctx, self.tab, Slot::Left);
            return;
        };
        let Some(target) = target else {
            self.status = Some((
                StatusKind::Error,
                "Select a target device first.".to_string(),
            ));
            self.open_qr(ctx, self.tab, Slot::Right);
            return;
        };

        if source == target {
            self.status = Some((
                StatusKind::Error,
                "Choose two different devices before transferring.".to_string(),
            ));
            return;
        }

        if self.require_online(ctx, &[(Slot::Left, &source), (Slot::Right, &target)]) {
            return;
        }

        self.start_attempt(Attempt::Transfer {
            source_name: self.device_name(&source),
            target_name: self.device_name(&target),
            source,
            target,
        });
    }

    fn start_sync(&mut self, ctx: &egui::Context) {
        if !self.actions_enabled() {
            return;
        }

        let left = self.slot_serial(self.tab, Slot::Left).cloned();
        let right = self.slot_serial(self.tab, Slot::Right).cloned();

        let Some(left) = left else {
            self.status = Some((
                StatusKind::Error,
                "Select the first device first.".to_string(),
            ));
            self.open_qr(ctx, self.tab, Slot::Left);
            return;
        };
        let Some(right) = right else {
            self.status = Some((
                StatusKind::Error,
                "Select the second device first.".to_string(),
            ));
            self.open_qr(ctx, self.tab, Slot::Right);
            return;
        };

        if left == right {
            self.status = Some((
                StatusKind::Error,
                "Choose two different devices before syncing.".to_string(),
            ));
            return;
        }

        if self.require_online(ctx, &[(Slot::Left, &left), (Slot::Right, &right)]) {
            return;
        }

        self.start_attempt(Attempt::AutoSync {
            left_name: self.device_name(&left),
            right_name: self.device_name(&right),
            left,
            right,
        });
    }

    /// Open the pairing dialog for the first selected device that is not online.
    /// Returns true when a pair dialog was opened.
    fn require_online(&mut self, ctx: &egui::Context, devices: &[(Slot, &String)]) -> bool {
        for (slot, serial) in devices {
            if self.device_online(serial) {
                continue;
            }
            self.status = Some((
                StatusKind::Error,
                format!(
                    "{} is not connected. Pair it with the QR code or reconnect it, then try again.",
                    self.device_name(serial)
                ),
            ));
            self.open_qr(ctx, self.tab, *slot);
            return true;
        }
        false
    }

    fn open_qr(&mut self, ctx: &egui::Context, tab: Tab, slot: Slot) {
        if self.adb_error.is_some() {
            return;
        }

        let (name, password) = qr::credentials();
        let payload = qr::payload(&name, &password);
        let texture = qr::texture(ctx, &format!("pairing-qr-{name}"), &payload).ok();
        let error = if texture.is_none() {
            Some("The QR code could not be rendered.".to_string())
        } else {
            None
        };
        let handle = pairing::start(&name, &password);

        self.qr = Some(QrState {
            tab,
            slot,
            texture,
            handle: Some(handle),
            status: "Waiting for the phone to scan the QR code...".to_string(),
            error,
        });
    }

    fn finish_pairing(&mut self, tab: Tab, slot: Slot, serial: String) {
        self.refresh_devices();
        let name = self.device_name(&serial);
        self.set_slot_device(tab, slot, Some(serial));
        self.status = Some((
            StatusKind::Success,
            format!(
                "Paired {name}. It is now selected for {}.",
                role_label(tab, slot)
            ),
        ));
    }

    fn poll_job(&mut self, ctx: &egui::Context) {
        let Some(receiver) = self.receiver.take() else {
            return;
        };
        let mut keep = true;

        loop {
            match receiver.try_recv() {
                Ok(JobEvent::Progress(message)) => {
                    self.busy = Some(message);
                    ctx.request_repaint();
                }
                Ok(JobEvent::Finished(result)) => {
                    keep = false;
                    self.busy = None;
                    match result {
                        JobResult::Success(message) => {
                            self.status = Some((StatusKind::Success, message));
                            self.refresh_devices();
                        }
                        JobResult::Failure(message) => {
                            self.status = Some((StatusKind::Error, message));
                        }
                        JobResult::GameRunning { attempt, serials } => {
                            self.pending_game = Some(PendingGame { attempt, serials });
                        }
                    }
                    ctx.request_repaint();
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    keep = false;
                    self.busy = None;
                    self.status = Some((
                        StatusKind::Error,
                        "The background task stopped unexpectedly.".to_string(),
                    ));
                    ctx.request_repaint();
                    break;
                }
            }
        }

        if keep {
            self.receiver = Some(receiver);
            if self.busy.is_some() {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
    }

    fn poll_qr(&mut self, ctx: &egui::Context) {
        let Some(mut qr) = self.qr.take() else {
            return;
        };

        let mut events = Vec::new();
        if let Some(handle) = &qr.handle {
            loop {
                match handle.receiver.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        events.push(PairEvent::Failed(
                            "The pairing task stopped unexpectedly.".to_string(),
                        ));
                        break;
                    }
                }
            }
        }

        let mut keep = true;
        for event in events {
            match event {
                PairEvent::Progress(message) => qr.status = message,
                PairEvent::Done { serial } => {
                    keep = false;
                    let (tab, slot) = (qr.tab, qr.slot);
                    self.finish_pairing(tab, slot, serial);
                    break;
                }
                PairEvent::Failed(message) => {
                    qr.error = Some(message);
                    qr.status = "Pairing failed. Generate a new code and try again.".to_string();
                    qr.handle = None;
                }
                PairEvent::Cancelled => {
                    qr.handle = None;
                }
            }
        }

        if keep {
            if qr.handle.is_some() {
                ctx.request_repaint_after(Duration::from_millis(150));
            }
            self.qr = Some(qr);
        }
    }

    fn show_manual(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.add_space(18.0);

        let tab = self.tab;
        ui.columns(3, |columns| {
            self.device_card(&mut columns[0], tab, Slot::Left, "Source");
            paint_transfer_arrow(&mut columns[1]);
            self.device_card(&mut columns[2], tab, Slot::Right, "Target");
        });

        ui.add_space(22.0);
        ui.vertical_centered(|ui| {
            let clicked = ui
                .add_enabled(
                    self.actions_enabled(),
                    egui::Button::new(egui::RichText::new("Transfer Save").size(16.0))
                        .min_size(egui::vec2(240.0, 42.0)),
                )
                .clicked();
            if clicked {
                self.start_transfer(ctx);
            }
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(
                    "Copies the source save over the target save. The target save is backed up first.",
                )
                .weak(),
            );
        });
    }

    fn show_automatic(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.add_space(18.0);

        let tab = self.tab;
        ui.columns(2, |columns| {
            self.device_card(&mut columns[0], tab, Slot::Left, "Device A");
            self.device_card(&mut columns[1], tab, Slot::Right, "Device B");
        });

        ui.add_space(22.0);
        ui.vertical_centered(|ui| {
            let clicked = ui
                .add_enabled(
                    self.actions_enabled(),
                    egui::Button::new(egui::RichText::new("Sync Save").size(16.0))
                        .min_size(egui::vec2(240.0, 42.0)),
                )
                .clicked();
            if clicked {
                self.start_sync(ctx);
            }
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(
                    "Reads both saves and copies the newer one to the other device.",
                )
                .weak(),
            );
        });
    }

    fn device_card(&mut self, ui: &mut egui::Ui, tab: Tab, slot: Slot, role: &str) {
        let mut selection = self.slot_serial(tab, slot).cloned();
        let previous = selection.clone();
        let devices: Vec<Device> = self
            .devices
            .iter()
            .filter(|device| device.is_online())
            .cloned()
            .collect();
        let selected_text = self.picker_text(selection.as_ref());
        let actions_enabled = self.actions_enabled();
        let missing = selection
            .as_ref()
            .filter(|serial| !devices.iter().any(|device| &device.serial == *serial))
            .map(|serial| format!("{} is not connected", short_serial(serial)));
        let mut show_qr = false;

        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::same(16))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new(role).strong());
                    ui.add_space(6.0);

                    egui::ComboBox::from_id_salt(("device-picker", tab as u8, slot as u8))
                        .width(220.0)
                        .selected_text(selected_text)
                        .show_ui(ui, |ui| {
                            if devices.is_empty() {
                                ui.label(egui::RichText::new("No devices connected").weak());
                            }
                            for device in &devices {
                                ui.selectable_value(
                                    &mut selection,
                                    Some(device.serial.clone()),
                                    device.picker_label(),
                                );
                            }
                        });

                    ui.add_space(4.0);
                    if ui
                        .add_enabled(actions_enabled, egui::Button::new("Show QR code"))
                        .on_hover_text("Pair a phone through Android Wireless debugging")
                        .clicked()
                    {
                        show_qr = true;
                    }

                    if let Some(message) = &missing {
                        ui.add_space(2.0);
                        ui.label(
                            egui::RichText::new(message)
                                .small()
                                .color(ui.visuals().warn_fg_color),
                        );
                    }
                });
            });

        if selection != previous {
            self.set_slot_device(tab, slot, selection);
            self.status = None;
        }
        if show_qr {
            self.open_qr(ui.ctx(), tab, slot);
        }
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Refresh devices").clicked() {
                    self.refresh_devices();
                    ui.close();
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });

            ui.menu_button("Settings", |ui| {
                let mut remember_tab = self.config.remember_tab;
                if ui
                    .checkbox(&mut remember_tab, "Remember active tab")
                    .changed()
                {
                    self.config.remember_tab = remember_tab;
                    self.config.last_tab = self.tab;
                    self.config.save();
                }

                let mut remember_devices = self.config.remember_devices;
                if ui
                    .checkbox(&mut remember_devices, "Remember selected devices")
                    .changed()
                {
                    self.config.remember_devices = remember_devices;
                    self.config.save();
                }
            });

            ui.menu_button("Help", |ui| {
                if ui.button("About").clicked() {
                    self.about_open = true;
                    ui.close();
                }
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(version) = &self.adb_version {
                    ui.label(egui::RichText::new("ADB ready").small().weak())
                        .on_hover_text(version);
                }
            });
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("PVZ2 Reflourished Save Sync")
                    .size(22.0)
                    .strong(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).weak());
            });
        });

        ui.add_space(6.0);
        let previous = self.tab;
        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut self.tab,
                Tab::Manual,
                egui::RichText::new("Manual Sync").size(17.0),
            );
            ui.selectable_value(
                &mut self.tab,
                Tab::Automatic,
                egui::RichText::new("Automatic Sync").size(17.0),
            );
        });
        if self.tab != previous {
            self.config.last_tab = self.tab;
            self.config.save();
        }

        ui.add_space(4.0);
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            if let Some(progress) = &self.busy {
                ui.spinner();
                ui.label(progress);
            } else if let Some((kind, message)) = &self.status {
                let color = match kind {
                    StatusKind::Success => egui::Color32::from_rgb(96, 200, 120),
                    StatusKind::Error => ui.visuals().error_fg_color,
                };
                ui.colored_label(color, message);
            } else {
                ui.label(egui::RichText::new("Ready").weak());
            }
        });
        ui.add_space(2.0);
    }

    fn content(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let adb_error = self.adb_error.clone();
        if let Some(error) = adb_error {
            ui.add_space(18.0);
            ui.label(
                egui::RichText::new("ADB is not available")
                    .size(18.0)
                    .strong(),
            );
            ui.add_space(4.0);
            ui.colored_label(ui.visuals().error_fg_color, error);
            ui.add_space(6.0);
            ui.label("Install Android platform-tools and make sure adb is on PATH, then retry.");
            if ui.button("Retry").clicked() {
                self.refresh_devices();
            }
            return;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| match self.tab {
                Tab::Manual => self.show_manual(ui, ctx),
                Tab::Automatic => self.show_automatic(ui, ctx),
            });
    }

    fn show_qr_window(&mut self, ctx: &egui::Context) {
        let Some(qr) = &self.qr else {
            return;
        };
        let texture = qr.texture.clone();
        let status = qr.status.clone();
        let error = qr.error.clone();
        let tab = qr.tab;
        let slot = qr.slot;

        let mut open = true;
        let mut close = false;
        let mut new_code = false;

        egui::Window::new("Pair a device")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(340.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "Pairing for {}",
                            role_label(tab, slot)
                        ))
                        .strong(),
                    );
                    ui.label(
                        "On the phone: Developer options > Wireless debugging > Pair device with QR code.",
                    );
                    ui.add_space(8.0);

                    match &texture {
                        Some(texture) => {
                            ui.add(
                                egui::Image::new(texture)
                                    .fit_to_exact_size(egui::vec2(260.0, 260.0)),
                            );
                        }
                        None => {
                            ui.label(
                                egui::RichText::new("The QR code could not be rendered.").weak(),
                            );
                        }
                    }

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(&status);
                    });
                    if let Some(error) = &error {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }

                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.button("New code").clicked() {
                            new_code = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
            });

        if close || !open {
            self.qr = None;
        } else if new_code {
            self.qr = None;
            self.open_qr(ctx, tab, slot);
        }
    }

    fn show_game_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_game.clone() else {
            return;
        };

        let names: Vec<String> = pending
            .serials
            .iter()
            .map(|serial| self.device_name(serial))
            .collect();
        let mut open = true;
        let mut stop = false;
        let mut cancel = false;

        egui::Window::new("Close the game")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!(
                    "Reflourished is running on {}.",
                    names.join(" and ")
                ));
                ui.label(
                    "Save files change while the game is running, so syncing now could corrupt progress.",
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Stop game and continue").clicked() {
                        stop = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if stop {
            self.pending_game = None;
            self.start_stop_and_retry(pending.attempt, pending.serials);
        } else if cancel || !open {
            self.pending_game = None;
        }
    }

    fn show_about(&mut self, ctx: &egui::Context) {
        let mut open = self.about_open;
        egui::Window::new("About")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(430.0)
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new("PVZ2 Reflourished Save Sync")
                        .strong()
                        .size(16.0),
                );
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                ui.add_space(6.0);
                ui.label(
                    "Syncs Plants vs. Zombies 2 Reflourished saves between Waydroid and an Android phone over ADB.",
                );
                ui.add_space(6.0);
                ui.label("Save folder:");
                ui.label(egui::RichText::new(adb::SAVE_DIR).monospace());
                ui.label(
                    "The target save is backed up locally before it is overwritten.",
                );
                ui.add_space(6.0);
                ui.label("Backups:");
                ui.label(egui::RichText::new(save::backups_root().display().to_string()).monospace());
            });
        self.about_open = open;
    }
}

impl eframe::App for SaveSyncApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        self.poll_job(&ctx);
        self.poll_qr(&ctx);

        if !self.startup_prompt_done {
            self.startup_prompt_done = true;
            self.prompt_missing(&ctx);
        }

        if self.receiver.is_none()
            && self.qr.is_none()
            && self.pending_game.is_none()
            && self.last_refresh.elapsed() > Duration::from_secs(5)
        {
            self.refresh_devices();
        }

        egui::Panel::top("header").show(ui, |ui| self.header(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::CentralPanel::default().show(ui, |ui| self.content(ui, &ctx));

        self.show_qr_window(&ctx);
        self.show_game_dialog(&ctx);
        self.show_about(&ctx);

        ctx.request_repaint_after(Duration::from_secs(3));
    }
}

fn worker(attempt: Attempt, sender: Sender<JobEvent>) {
    match run_attempt(&attempt, &sender) {
        Ok(message) => {
            let _ = sender.send(JobEvent::Finished(JobResult::Success(message)));
        }
        Err(WorkerFailure::GameRunning(serials)) => {
            let _ = sender.send(JobEvent::Finished(JobResult::GameRunning {
                attempt,
                serials,
            }));
        }
        Err(WorkerFailure::Failed(message)) => {
            let _ = sender.send(JobEvent::Finished(JobResult::Failure(message)));
        }
    }
}

fn stop_worker(attempt: Attempt, serials: Vec<String>, sender: Sender<JobEvent>) {
    for serial in &serials {
        let _ = sender.send(JobEvent::Progress("Stopping the game...".to_string()));
        if let Err(error) = adb::force_stop(serial) {
            let _ = sender.send(JobEvent::Finished(JobResult::Failure(error)));
            return;
        }
    }

    std::thread::sleep(Duration::from_millis(800));
    let _ = sender.send(JobEvent::Progress(
        "Game stopped. Continuing...".to_string(),
    ));
    worker(attempt, sender);
}

fn run_attempt(attempt: &Attempt, sender: &Sender<JobEvent>) -> Result<String, WorkerFailure> {
    match attempt {
        Attempt::Transfer {
            source,
            target,
            source_name,
            target_name,
        } => {
            let _ = sender.send(JobEvent::Progress("Checking both devices...".to_string()));
            let source_probe = save::probe(source).map_err(WorkerFailure::Failed)?;
            let target_probe = save::probe(target).map_err(WorkerFailure::Failed)?;

            check_running(&[
                (source.as_str(), source_probe.running),
                (target.as_str(), target_probe.running),
            ])?;

            if !source_probe.has_save() {
                return Err(WorkerFailure::Failed(format!(
                    "{source_name} has no Reflourished save yet. Start the game once on that device, then try again."
                )));
            }

            copy_and_describe(source, target, source_name, target_name, sender)
        }
        Attempt::AutoSync {
            left,
            right,
            left_name,
            right_name,
        } => {
            let _ = sender.send(JobEvent::Progress("Reading both saves...".to_string()));
            let left_probe = save::probe(left).map_err(WorkerFailure::Failed)?;
            let right_probe = save::probe(right).map_err(WorkerFailure::Failed)?;

            check_running(&[
                (left.as_str(), left_probe.running),
                (right.as_str(), right_probe.running),
            ])?;

            match save::plan(&left_probe, &right_probe) {
                SyncPlan::LeftToRight => {
                    copy_and_describe(left, right, left_name, right_name, sender)
                }
                SyncPlan::RightToLeft => {
                    copy_and_describe(right, left, right_name, left_name, sender)
                }
                SyncPlan::InSync => Ok(format!(
                    "{left_name} and {right_name} already have the same save. Nothing was copied."
                )),
                SyncPlan::Ambiguous {
                    mtime,
                    left_size,
                    right_size,
                } => Err(WorkerFailure::Failed(format!(
                    "{left_name} and {right_name} have the same save time ({}) but different sizes ({left_size} and {right_size} bytes), so no save was copied.",
                    save::format_mtime(mtime)
                ))),
                SyncPlan::NoSave => Err(WorkerFailure::Failed(
                    "Neither device has a Reflourished save yet. Start the game on one device first."
                        .to_string(),
                )),
            }
        }
    }
}

fn check_running(devices: &[(&str, bool)]) -> Result<(), WorkerFailure> {
    let serials: Vec<String> = devices
        .iter()
        .filter(|(_, running)| *running)
        .map(|(serial, _)| (*serial).to_string())
        .collect();

    if serials.is_empty() {
        Ok(())
    } else {
        Err(WorkerFailure::GameRunning(serials))
    }
}

fn copy_and_describe(
    source: &str,
    target: &str,
    source_name: &str,
    target_name: &str,
    sender: &Sender<JobEvent>,
) -> Result<String, WorkerFailure> {
    let mut progress = |message: &str| {
        let _ = sender.send(JobEvent::Progress(message.to_string()));
    };

    let report = save::copy_save(source, target, &mut progress).map_err(WorkerFailure::Failed)?;
    let mut message = format!(
        "Copied {} save entries from {source_name} to {target_name} (save from {}).",
        report.entries,
        save::format_mtime(report.mtime)
    );
    if let Some(backup) = &report.backup {
        message.push_str(&format!(
            " The previous {target_name} save is backed up at {}.",
            backup.display()
        ));
    }
    Ok(message)
}

fn role_label(tab: Tab, slot: Slot) -> &'static str {
    match (tab, slot) {
        (Tab::Manual, Slot::Left) => "Source",
        (Tab::Manual, Slot::Right) => "Target",
        (Tab::Automatic, Slot::Left) => "Device A",
        (Tab::Automatic, Slot::Right) => "Device B",
    }
}

fn short_serial(serial: &str) -> &str {
    serial
        .split("._adb-tls-connect._tcp")
        .next()
        .unwrap_or(serial)
}

fn paint_transfer_arrow(ui: &mut egui::Ui) {
    ui.vertical_centered(|ui| {
        ui.add_space(54.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(80.0, 44.0), egui::Sense::hover());
        let color = ui.visuals().weak_text_color();
        let stroke = egui::Stroke::new(3.0, color);
        let y = rect.center().y;
        let x0 = rect.left() + 8.0;
        let x1 = rect.right() - 8.0;

        let painter = ui.painter();
        painter.line_segment([egui::pos2(x0, y), egui::pos2(x1, y)], stroke);
        painter.line_segment([egui::pos2(x1, y), egui::pos2(x1 - 15.0, y - 10.0)], stroke);
        painter.line_segment([egui::pos2(x1, y), egui::pos2(x1 - 15.0, y + 10.0)], stroke);
    });
}
