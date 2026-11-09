use crate::game::Game;
use std::collections::HashMap;
use std::io::Read;
use std::net::IpAddr;
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

#[derive(Clone, Debug)]
pub struct CmdOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub serial: String,
    pub state: String,
    pub product: Option<String>,
    pub model: Option<String>,
    pub device: Option<String>,
    pub transport_id: Option<String>,
}

impl Device {
    pub fn is_online(&self) -> bool {
        self.state == "device"
    }

    pub fn is_waydroid(&self) -> bool {
        [
            self.product.as_deref(),
            self.model.as_deref(),
            self.device.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|value| value.to_ascii_lowercase().contains("waydroid"))
    }

    pub fn display_name(&self) -> String {
        if self.is_waydroid() {
            return "Waydroid".to_string();
        }
        self.model
            .clone()
            .or_else(|| self.device.clone())
            .unwrap_or_else(|| "Android device".to_string())
    }

    pub fn picker_label(&self) -> String {
        let state = if self.is_online() {
            String::new()
        } else {
            format!(" ({})", self.state)
        };
        format!("{}{}", self.display_name(), state)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MdnsService {
    pub name: String,
    pub service_type: String,
    pub host: String,
    pub port: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteStat {
    pub mtime: i64,
    pub size: u64,
}

pub fn run(args: &[&str], timeout: Duration) -> Result<CmdOutput, String> {
    let mut command = Command::new("adb");
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "adb was not found on PATH".to_string()
        } else {
            format!("could not run adb: {error}")
        }
    })?;

    let status = match child.wait_timeout(timeout) {
        Ok(Some(status)) => status,
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "adb {} timed out after {} seconds",
                args.join(" "),
                timeout.as_secs()
            ));
        }
        Err(error) => return Err(format!("failed to wait for adb: {error}")),
    };

    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }

    Ok(CmdOutput {
        success: status.success(),
        stdout,
        stderr,
    })
}

fn error_text(output: &CmdOutput) -> String {
    let stderr = output.stderr.trim();
    if !stderr.is_empty() {
        return stderr.to_string();
    }
    let stdout = output.stdout.trim();
    if !stdout.is_empty() {
        return stdout.to_string();
    }
    "no output from adb".to_string()
}

fn ensure_ok(output: CmdOutput, action: &str) -> Result<CmdOutput, String> {
    if output.success {
        Ok(output)
    } else {
        Err(format!("{action} failed: {}", error_text(&output)))
    }
}

pub fn adb_version() -> Result<String, String> {
    let output = run(&["version"], Duration::from_secs(10))?;
    if !output.success {
        return Err(format!("adb version failed: {}", error_text(&output)));
    }
    let line = output
        .stdout
        .lines()
        .find(|line| line.starts_with("Android Debug Bridge"))
        .unwrap_or_else(|| output.stdout.lines().next().unwrap_or("adb"));
    Ok(line.trim().to_string())
}

pub fn shell(serial: &str, command: &str) -> Result<String, String> {
    let output = run(&["-s", serial, "shell", command], Duration::from_secs(30))?;
    let output = ensure_ok(output, &format!("adb shell on {serial}"))?;
    Ok(output.stdout)
}

pub fn list_devices() -> Result<Vec<Device>, String> {
    let output = run(&["devices", "-l"], Duration::from_secs(15))?;
    let output = ensure_ok(output, "adb devices")?;
    Ok(parse_devices(&output.stdout))
}

pub fn parse_devices(output: &str) -> Vec<Device> {
    let mut devices = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with("List of devices")
            || line.starts_with('*')
            || line.starts_with("adb server")
        {
            continue;
        }

        let mut parts = line.split_whitespace();
        let Some(serial) = parts.next() else {
            continue;
        };
        let Some(state) = parts.next() else {
            continue;
        };

        let mut device = Device {
            serial: serial.to_string(),
            state: state.to_string(),
            product: None,
            model: None,
            device: None,
            transport_id: None,
        };

        for part in parts {
            let Some((key, value)) = part.split_once(':') else {
                continue;
            };
            match key {
                "product" => device.product = Some(value.to_string()),
                "model" => device.model = Some(value.to_string()),
                "device" => device.device = Some(value.to_string()),
                "transport_id" => device.transport_id = Some(value.to_string()),
                _ => {}
            }
        }

        devices.push(device);
    }
    devices
}

pub fn split_host_port(value: &str) -> Option<(String, u16)> {
    let value = value.trim();
    if let Some(rest) = value.strip_prefix('[') {
        let (host, port) = rest.split_once("]:")?;
        let host = host.to_string();
        host.parse::<IpAddr>().ok()?;
        return Some((host, port.parse().ok()?));
    }

    let (host, port) = value.rsplit_once(':')?;
    host.parse::<IpAddr>().ok()?;
    Some((host.to_string(), port.parse().ok()?))
}

pub fn is_ip_serial(serial: &str) -> bool {
    split_host_port(serial).is_some()
}

pub fn mdns_services() -> Vec<MdnsService> {
    let Ok(output) = run(&["mdns", "services"], Duration::from_secs(10)) else {
        return Vec::new();
    };
    if !output.success {
        return Vec::new();
    }
    parse_mdns_services(&output.stdout)
}

pub fn parse_mdns_services(output: &str) -> Vec<MdnsService> {
    let mut services = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("List of discovered") {
            continue;
        }
        let columns: Vec<&str> = line.split_whitespace().collect();
        if columns.len() < 3 {
            continue;
        }
        let Some((host, port)) = split_host_port(columns[2]) else {
            continue;
        };
        services.push(MdnsService {
            name: columns[0].to_string(),
            service_type: columns[1].to_string(),
            host,
            port,
        });
    }
    services
}

pub fn mdns_connect_map(services: &[MdnsService]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for service in services {
        if service.service_type == "_adb-tls-connect._tcp" {
            map.entry(service.host.clone())
                .or_insert_with(|| format!("{}.{}", service.name, service.service_type));
        }
    }
    map
}

/// Drop the duplicated `ip:port` transport when the same device is also connected through mDNS.
pub fn dedupe_devices(devices: Vec<Device>, services: &[MdnsService]) -> Vec<Device> {
    let map = mdns_connect_map(services);
    let mdns_serials: std::collections::HashSet<String> = devices
        .iter()
        .filter(|device| device.serial.contains("._adb-tls-connect._tcp"))
        .map(|device| device.serial.clone())
        .collect();

    devices
        .into_iter()
        .filter(|device| {
            let Some((host, _)) = split_host_port(&device.serial) else {
                return true;
            };
            let Some(mdns_serial) = map.get(&host) else {
                return true;
            };
            !mdns_serials.contains(mdns_serial)
        })
        .collect()
}

pub fn serial_for_ip(ip: &str, services: &[MdnsService], devices: &[Device]) -> Option<String> {
    if let Some(serial) = mdns_connect_map(services).get(ip)
        && devices
            .iter()
            .any(|device| device.serial == *serial && device.is_online())
    {
        return Some(serial.clone());
    }

    let prefix = format!("{ip}:");
    devices
        .iter()
        .find(|device| device.is_online() && device.serial.starts_with(&prefix))
        .map(|device| device.serial.clone())
}

pub fn connect(address: &str) -> Result<String, String> {
    let output = run(&["connect", address], Duration::from_secs(20))?;
    let text = format!("{}{}", output.stdout, output.stderr);
    let lower = text.to_ascii_lowercase();
    if lower.contains("connected to") || lower.contains("already connected") {
        Ok(text.trim().to_string())
    } else {
        Err(format!(
            "adb connect {address} failed: {}",
            error_text(&output)
        ))
    }
}

pub fn stat_remote(serial: &str, path: &str) -> Result<Option<RemoteStat>, String> {
    let command = format!("if [ -f '{path}' ]; then stat -c '%Y|%s' '{path}'; fi");
    let stdout = shell(serial, &command)?;
    let Some(line) = stdout.lines().find(|line| !line.trim().is_empty()) else {
        return Ok(None);
    };
    let mut parts = line.trim().split('|');
    let mtime = parts
        .next()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .ok_or_else(|| format!("unexpected stat output: {line}"))?;
    let size = parts
        .next()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .ok_or_else(|| format!("unexpected stat output: {line}"))?;
    Ok(Some(RemoteStat { mtime, size }))
}

pub fn device_is_running(serial: &str, game: &Game) -> Result<bool, String> {
    // `Game` only holds validated package names, so no quoting is needed.
    let stdout = shell(serial, &format!("pidof {} || true", game.package()))?;
    Ok(!stdout.trim().is_empty())
}

pub fn force_stop(serial: &str, game: &Game) -> Result<(), String> {
    shell(serial, &format!("am force-stop {}", game.package()))
        .map(|_| ())
        .map_err(|error| format!("could not stop the game: {error}"))
}

pub fn mkdir_p(serial: &str, path: &str) -> Result<(), String> {
    shell(serial, &format!("mkdir -p '{path}'")).map(|_| ())
}

pub fn pull(serial: &str, remote: &str, local: &std::path::Path) -> Result<(), String> {
    let local = local.to_string_lossy();
    let output = run(
        &["-s", serial, "pull", "-a", remote, &local],
        Duration::from_secs(180),
    )?;
    ensure_ok(output, &format!("pulling {remote}"))?;
    Ok(())
}

pub fn push(serial: &str, local: &std::path::Path, remote_dir: &str) -> Result<(), String> {
    let local = local.to_string_lossy();
    let output = run(
        &["-s", serial, "push", &local, remote_dir],
        Duration::from_secs(180),
    )?;
    ensure_ok(output, &format!("pushing {local}"))?;
    Ok(())
}

pub fn set_mtime(serial: &str, path: &str, mtime: i64) -> Result<(), String> {
    shell(serial, &format!("touch -d @{mtime} '{path}'")).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICES: &str = "\
List of devices attached
192.168.240.112:5555   device product:lineage_waydroid_x86_64 model:WayDroid_x86_64_Device device:waydroid_x86_64 transport_id:3
adb-phone._adb-tls-connect._tcp device product:obsidian_eea model:24116RACCG device:obsidian transport_id:1
broken                  unauthorized usb:1-1 transport_id:9
";

    #[test]
    fn parses_adb_devices() {
        let devices = parse_devices(DEVICES);
        assert_eq!(devices.len(), 3);
        assert_eq!(devices[0].serial, "192.168.240.112:5555");
        assert!(devices[0].is_online());
        assert!(devices[0].is_waydroid());
        assert_eq!(devices[0].display_name(), "Waydroid");
        assert_eq!(devices[1].model.as_deref(), Some("24116RACCG"));
        assert!(!devices[2].is_online());
    }

    #[test]
    fn parses_quoted_ipv6_addresses() {
        assert_eq!(
            split_host_port("[fe80::1]:37000"),
            Some(("fe80::1".to_string(), 37000))
        );
        assert_eq!(
            split_host_port("192.168.2.12:36811"),
            Some(("192.168.2.12".to_string(), 36811))
        );
        assert_eq!(split_host_port("adb-phone._adb-tls-connect._tcp"), None);
    }

    #[test]
    fn parses_mdns_services_and_builds_serial_map() {
        let output = "\
List of discovered mdns services
adb-kj9hbelrkbdyqc8t-g6ahHk _adb-tls-connect._tcp 192.168.2.12:36811
";
        let services = parse_mdns_services(output);
        assert_eq!(services.len(), 1);
        let map = mdns_connect_map(&services);
        assert_eq!(
            map.get("192.168.2.12").map(String::as_str),
            Some("adb-kj9hbelrkbdyqc8t-g6ahHk._adb-tls-connect._tcp")
        );
    }

    #[test]
    fn recognizes_ip_serials() {
        assert!(is_ip_serial("192.168.240.112:5555"));
        assert!(is_ip_serial("[fe80::1]:5555"));
        assert!(!is_ip_serial("adb-phone._adb-tls-connect._tcp"));
    }
}
