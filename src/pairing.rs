use crate::adb;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

const PAIRING_SERVICE: &str = "_adb-tls-pairing._tcp";
const CONNECT_SERVICE: &str = "_adb-tls-connect._tcp";
const SCAN_TIMEOUT: Duration = Duration::from_secs(120);
const PAIR_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub enum PairEvent {
    Progress(String),
    Done { serial: String },
    Failed(String),
    Cancelled,
}

pub struct PairHandle {
    pub receiver: Receiver<PairEvent>,
    cancel: Arc<AtomicBool>,
}

impl PairHandle {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for PairHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Start the wireless debugging pairing flow in the background.
///
/// The phone advertises the QR service name while the pairing dialog is open.
/// After `adb pair` succeeds, adb normally discovers the TLS endpoint by itself;
/// when it does not, the connect service address is used for an explicit `adb connect`.
pub fn start(name: &str, password: &str) -> PairHandle {
    let (sender, receiver) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let worker_name = name.to_string();
    let worker_password = password.to_string();

    std::thread::spawn(move || {
        let result = run_pairing(&worker_name, &worker_password, &sender, &worker_cancel);
        let event = match result {
            Ok(serial) => PairEvent::Done { serial },
            Err(WorkerError::Cancelled) => PairEvent::Cancelled,
            Err(WorkerError::Failed(message)) => PairEvent::Failed(message),
        };
        let _ = sender.send(event);
    });

    PairHandle { receiver, cancel }
}

enum WorkerError {
    Cancelled,
    Failed(String),
}

fn progress(sender: &Sender<PairEvent>, message: impl Into<String>) {
    let _ = sender.send(PairEvent::Progress(message.into()));
}

fn cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Relaxed)
}

fn run_pairing(
    name: &str,
    password: &str,
    sender: &Sender<PairEvent>,
    cancel: &AtomicBool,
) -> Result<String, WorkerError> {
    progress(sender, "Waiting for the phone to scan the QR code...");
    let pairing = wait_for_service(PAIRING_SERVICE, Some(name), SCAN_TIMEOUT, sender, cancel)?;
    let pair_address = format_address(&pairing.host, pairing.port);

    progress(sender, format!("Pairing with {pair_address}..."));
    let output =
        adb::run(&["pair", &pair_address, password], PAIR_TIMEOUT).map_err(WorkerError::Failed)?;

    let text = format!("{}{}", output.stdout, output.stderr);
    if !output.success || !text.to_ascii_lowercase().contains("successfully paired") {
        return Err(WorkerError::Failed(format!(
            "pairing failed: {}",
            clean_output(&text)
        )));
    }

    progress(
        sender,
        "Paired successfully. Waiting for the device to connect...",
    );
    wait_for_connection(&pairing.host, sender, cancel)
}

fn wait_for_service(
    service_type: &str,
    name: Option<&str>,
    timeout: Duration,
    sender: &Sender<PairEvent>,
    cancel: &AtomicBool,
) -> Result<adb::MdnsService, WorkerError> {
    let deadline = Instant::now() + timeout;
    let mut last_announcement = Instant::now();

    loop {
        if cancelled(cancel) {
            return Err(WorkerError::Cancelled);
        }

        if let Some(service) = adb::mdns_services().into_iter().find(|service| {
            service.service_type == service_type && name.is_none_or(|name| service.name == name)
        }) {
            return Ok(service);
        }

        if Instant::now() >= deadline {
            let what = if service_type == PAIRING_SERVICE {
                "the pairing service did not appear; make sure the phone scanned the current QR code"
            } else {
                "the device did not connect"
            };
            return Err(WorkerError::Failed(format!(
                "timed out after {} seconds: {what}",
                timeout.as_secs()
            )));
        }

        // The mDNS snapshot can lag, but there is no useful per-poll progress to
        // report; refresh the text occasionally so the dialog does not look stuck.
        if last_announcement.elapsed() > Duration::from_secs(20) {
            progress(sender, "Still waiting for the phone...");
            last_announcement = Instant::now();
        }

        std::thread::sleep(Duration::from_millis(400));
    }
}

fn wait_for_connection(
    pairing_host: &str,
    sender: &Sender<PairEvent>,
    cancel: &AtomicBool,
) -> Result<String, WorkerError> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let before: HashSet<String> = adb::list_devices()
        .unwrap_or_default()
        .into_iter()
        .map(|device| device.serial)
        .collect();
    let mut connect_attempt: Option<Instant> = None;

    loop {
        if cancelled(cancel) {
            return Err(WorkerError::Cancelled);
        }

        let devices = adb::list_devices().unwrap_or_default();
        let services = adb::mdns_services();

        if let Some(serial) = adb::serial_for_ip(pairing_host, &services, &devices) {
            return Ok(serial);
        }

        // adb may also report a brand new transport before the connect service is
        // visible in the mdns snapshot. Accept any new device that shares the
        // pairing host.
        for device in devices.iter().filter(|device| device.is_online()) {
            if before.contains(&device.serial) {
                continue;
            }
            if device_host(&device.serial, &services).as_deref() == Some(pairing_host) {
                return Ok(device.serial.clone());
            }
        }

        if let Some(connect) = services
            .iter()
            .find(|service| service.service_type == CONNECT_SERVICE && service.host == pairing_host)
        {
            let due =
                connect_attempt.is_none_or(|attempt| attempt.elapsed() >= Duration::from_secs(4));
            if due {
                connect_attempt = Some(Instant::now());
                let address = format_address(&connect.host, connect.port);
                progress(sender, format!("Connecting to {address}..."));
                let _ = adb::connect(&address);
            }
        }

        if Instant::now() >= deadline {
            return Err(WorkerError::Failed(
                "paired, but the device did not connect. Open Wireless debugging on the phone and try again."
                    .to_string(),
            ));
        }

        std::thread::sleep(Duration::from_millis(500));
    }
}

fn device_host(serial: &str, services: &[adb::MdnsService]) -> Option<String> {
    if let Some((host, _)) = adb::split_host_port(serial) {
        return Some(host);
    }
    let name = serial.strip_suffix(&format!(".{CONNECT_SERVICE}"))?;
    services
        .iter()
        .find(|service| service.service_type == CONNECT_SERVICE && service.name == name)
        .map(|service| service.host.clone())
}

fn format_address(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn clean_output(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        "no output from adb".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_ipv4_and_ipv6_addresses() {
        assert_eq!(format_address("192.168.2.12", 36811), "192.168.2.12:36811");
        assert_eq!(format_address("fe80::1", 5555), "[fe80::1]:5555");
    }

    #[test]
    fn resolves_hosts_for_both_serial_kinds() {
        let services = vec![adb::MdnsService {
            name: "adb-phone".to_string(),
            service_type: "_adb-tls-connect._tcp".to_string(),
            host: "192.168.2.12".to_string(),
            port: 36811,
        }];
        assert_eq!(
            device_host("adb-phone._adb-tls-connect._tcp", &services).as_deref(),
            Some("192.168.2.12")
        );
        assert_eq!(
            device_host("192.168.2.12:36811", &services).as_deref(),
            Some("192.168.2.12")
        );
        assert_eq!(device_host("unrelated", &services), None);
    }
}
