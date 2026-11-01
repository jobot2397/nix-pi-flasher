use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter};

use crate::spawn::{spawn_piped, stream_output, tool_exists};
use crate::state::with_inner;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiskInfo {
    pub path: String,
    pub name: String,
    pub size_bytes: u64,
    pub model: Option<String>,
    pub tran: Option<String>,
    pub removable: bool,
    pub mountpoints: Vec<String>,
}

#[derive(Clone, Serialize)]
struct LogPayload {
    line: String,
}

#[derive(Clone, Serialize)]
struct FinishedPayload {
    ok: bool,
    image: Option<String>,
    error: Option<String>,
}

struct FlashGuard {
    app: AppHandle,
}

impl Drop for FlashGuard {
    fn drop(&mut self) {
        with_inner(&self.app, |inner| inner.flash_running = false);
    }
}

fn disk_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^/dev/(sd[a-z]|nvme[0-9]+n[0-9]+|mmcblk[0-9]+)$").unwrap())
}

pub fn disk_path_ok(path: &str) -> bool {
    disk_re().is_match(path)
}

fn as_bool(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_u64() == Some(1),
        Some(Value::String(text)) => text == "1" || text.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

fn as_u64(value: Option<&Value>) -> u64 {
    match value {
        Some(Value::Number(number)) => number.as_u64().unwrap_or(0),
        Some(Value::String(text)) => text.parse().unwrap_or(0),
        _ => 0,
    }
}

fn as_opt_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn mountpoints_of(node: &Value) -> Vec<String> {
    let mut found = Vec::new();
    if let Some(Value::Array(points)) = node.get("mountpoints") {
        for point in points {
            if let Some(text) = point.as_str() {
                if !text.is_empty() {
                    found.push(text.to_string());
                }
            }
        }
    } else if let Some(text) = node.get("mountpoint").and_then(|item| item.as_str()) {
        if !text.is_empty() {
            found.push(text.to_string());
        }
    }
    found
}

fn collect_mounts(node: &Value, out: &mut Vec<String>) {
    out.extend(mountpoints_of(node));
    if let Some(Value::Array(children)) = node.get("children") {
        for child in children {
            collect_mounts(child, out);
        }
    }
}

fn backs_root(node: &Value) -> bool {
    let mut mounts = Vec::new();
    collect_mounts(node, &mut mounts);
    mounts.iter().any(|mount| mount == "/")
}

pub fn filter_disks(lsblk_json: &str) -> Result<Vec<DiskInfo>, String> {
    let value: Value = serde_json::from_str(lsblk_json)
        .map_err(|err| format!("could not parse lsblk JSON: {err}"))?;
    let devices = value
        .get("blockdevices")
        .and_then(|item| item.as_array())
        .ok_or("lsblk JSON has no blockdevices array")?;

    let mut disks = Vec::new();
    for device in devices {
        let kind = device
            .get("type")
            .and_then(|item| item.as_str())
            .unwrap_or("");
        if kind != "disk" {
            continue;
        }
        let Some(path) = device.get("path").and_then(|item| item.as_str()) else {
            continue;
        };
        if !disk_path_ok(path) || backs_root(device) {
            continue;
        }
        let removable = as_bool(device.get("rm"));
        let tran = as_opt_string(device.get("tran"));
        let usb = tran
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("usb"));
        if !(removable || usb) {
            continue;
        }
        let mut mountpoints = Vec::new();
        collect_mounts(device, &mut mountpoints);
        mountpoints.retain(|mount| mount != "/");
        mountpoints.sort();
        mountpoints.dedup();
        disks.push(DiskInfo {
            path: path.to_string(),
            name: device
                .get("name")
                .and_then(|item| item.as_str())
                .unwrap_or("")
                .to_string(),
            size_bytes: as_u64(device.get("size")),
            model: as_opt_string(device.get("model")),
            tran,
            removable,
            mountpoints,
        });
    }
    disks.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(disks)
}

fn run_lsblk() -> Result<String, String> {
    if !tool_exists("lsblk") {
        return Err("lsblk is not installed. Install util-linux to list disks.".into());
    }
    let output = Command::new("lsblk")
        .args([
            "-J",
            "-b",
            "-o",
            "NAME,PATH,SIZE,TYPE,TRAN,RM,RO,MOUNTPOINTS,MODEL,LABEL",
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("failed to run lsblk: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("lsblk failed: {stderr}"));
    }
    String::from_utf8(output.stdout).map_err(|err| format!("lsblk output was not UTF-8: {err}"))
}

fn find_device<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    root.get("blockdevices")
        .and_then(|item| item.as_array())
        .and_then(|devices| {
            devices
                .iter()
                .find(|device| device.get("path").and_then(|item| item.as_str()) == Some(path))
        })
}

#[cfg(unix)]
fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

#[cfg(not(unix))]
fn is_root() -> bool {
    false
}

fn privileged(args: &[String]) -> Result<Command, String> {
    if args.is_empty() {
        return Err("empty command".into());
    }
    if is_root() {
        let mut command = Command::new(&args[0]);
        command.args(&args[1..]);
        return Ok(command);
    }
    if !tool_exists("pkexec") {
        return Err(
            "pkexec is not installed. Install polkit, or run Nix Pi Flasher as root, to flash a disk."
                .into(),
        );
    }
    let mut command = Command::new("pkexec");
    command.args(args);
    Ok(command)
}

fn emit_log(app: &AppHandle, line: impl Into<String>) {
    let _ = app.emit("flash-log", LogPayload { line: line.into() });
}

fn emit_finished(app: &AppHandle, ok: bool, image: Option<String>, error: Option<String>) {
    let _ = app.emit("flash-finished", FinishedPayload { ok, image, error });
}

fn umount_one(mountpoint: &str) -> Result<(), String> {
    if !mountpoint.starts_with('/') || mountpoint.contains('\n') || mountpoint == "/" {
        return Err(format!("refusing to unmount '{mountpoint}'"));
    }
    if !tool_exists("umount") {
        return Err("umount is not installed. Install util-linux.".into());
    }
    let args = vec![
        "umount".to_string(),
        "--".to_string(),
        mountpoint.to_string(),
    ];
    let output = privileged(&args)?
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("failed to run umount: {err}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stdout}{stderr}");
    if combined.to_ascii_lowercase().contains("not mounted") {
        return Ok(());
    }
    Err(format!("umount {mountpoint} failed: {}", combined.trim()))
}

fn mounts_for_device(device: &str) -> Result<Vec<String>, String> {
    let json = run_lsblk()?;
    let value: Value =
        serde_json::from_str(&json).map_err(|err| format!("could not parse lsblk JSON: {err}"))?;
    let node = find_device(&value, device)
        .ok_or_else(|| format!("{device} is no longer present in lsblk"))?;
    let kind = node
        .get("type")
        .and_then(|item| item.as_str())
        .unwrap_or("");
    if kind != "disk" {
        return Err(format!("{device} is not a whole disk"));
    }
    if backs_root(node) {
        return Err(format!("refusing to flash {device}: it backs /"));
    }
    let mut mounts = Vec::new();
    collect_mounts(node, &mut mounts);
    if mounts.iter().any(|mount| mount == "/") {
        return Err(format!("refusing to flash {device}: it backs /"));
    }
    mounts.retain(|mount| mount.starts_with('/') && mount != "/" && !mount.contains('\n'));
    mounts.sort_by_key(|mount| std::cmp::Reverse(mount.len()));
    mounts.dedup();
    Ok(mounts)
}

fn run_flash(app: &AppHandle, device: &str, image: &str) -> Result<(), String> {
    let mounts = mounts_for_device(device)?;
    for mount in &mounts {
        emit_log(app, format!("umount {mount}"));
        umount_one(mount)?;
    }

    if !tool_exists("dd") {
        return Err("dd is not installed".into());
    }
    let args = vec![
        "dd".to_string(),
        format!("if={image}"),
        format!("of={device}"),
        "bs=4M".to_string(),
        "conv=fsync".to_string(),
        "status=progress".to_string(),
    ];
    emit_log(
        app,
        format!(
            "{}dd if={image} of={device} bs=4M conv=fsync status=progress",
            if is_root() { "" } else { "pkexec " }
        ),
    );
    let mut child = spawn_piped(privileged(&args)?, false)
        .map_err(|err| format!("failed to start dd: {err}"))?;
    let app_log = app.clone();
    let status = stream_output(&mut child, move |line| emit_log(&app_log, line))
        .map_err(|err| format!("failed while waiting for dd: {err}"))?;
    if status.success() {
        Ok(())
    } else if let Some(code) = status.code() {
        Err(format!("dd exited with status {code}"))
    } else {
        Err(format!("dd exited: {status}"))
    }
}

#[tauri::command]
pub fn list_disks(app: AppHandle) -> Result<Vec<DiskInfo>, String> {
    let json = run_lsblk()?;
    let disks = filter_disks(&json)?;
    with_inner(&app, |inner| {
        inner.allowed_disks = disks.iter().map(|disk| disk.path.clone()).collect();
    });
    Ok(disks)
}

#[tauri::command]
pub fn flash(app: AppHandle, device: String, confirm: String) -> Result<(), String> {
    if device != confirm {
        return Err(
            "Confirmation does not match the device path. The disk was not written.".into(),
        );
    }
    if !disk_path_ok(&device) {
        return Err(format!(
            "refusing to flash '{device}': path is not a whole disk this app accepts"
        ));
    }
    if !tool_exists("dd") {
        return Err("dd is not installed".into());
    }
    if !is_root() && !tool_exists("pkexec") {
        return Err(
            "pkexec is not installed. Install polkit, or run Nix Pi Flasher as root, to flash a disk."
                .into(),
        );
    }

    let image = with_inner(&app, |inner| {
        if inner.build_running {
            return Err("Cannot flash while a build is running".to_string());
        }
        if inner.flash_running {
            return Err("A flash is already running".to_string());
        }
        if !inner.allowed_disks.iter().any(|path| path == &device) {
            return Err(
                "That device is not in the last disk listing. Refresh the list and choose it again."
                    .to_string(),
            );
        }
        let image = inner.last_image.clone().ok_or_else(|| {
            "No image has been built in this session. Flash only writes the last successful build."
                .to_string()
        })?;
        if image.contains('\n') || !image.starts_with('/') || !image.ends_with(".img") {
            return Err("The stored image path is not a raw .img produced by this app".to_string());
        }
        if !Path::new(&image).is_file() {
            return Err(format!("image file no longer exists: {image}"));
        }
        inner.flash_running = true;
        Ok(image)
    })?;

    std::thread::spawn(move || {
        let _guard = FlashGuard { app: app.clone() };
        match run_flash(&app, &device, &image) {
            Ok(()) => emit_finished(&app, true, Some(image), None),
            Err(err) => emit_finished(&app, false, None, Some(err)),
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_removable_or_usb_disks_that_are_not_root() {
        let json = r#"{
          "blockdevices": [
            {
              "name": "nvme0n1",
              "path": "/dev/nvme0n1",
              "size": 1000,
              "type": "disk",
              "tran": "nvme",
              "rm": false,
              "model": "Internal",
              "children": [
                {"name": "nvme0n1p2", "path": "/dev/nvme0n1p2", "type": "part", "mountpoints": ["/"]}
              ]
            },
            {
              "name": "sda",
              "path": "/dev/sda",
              "size": 8000000000,
              "type": "disk",
              "tran": "usb",
              "rm": true,
              "model": "Flash",
              "mountpoints": [null],
              "children": [
                {"name": "sda1", "path": "/dev/sda1", "type": "part", "mountpoints": ["/media/disk"]}
              ]
            },
            {
              "name": "sdb",
              "path": "/dev/sdb",
              "size": 100,
              "type": "disk",
              "tran": "sata",
              "rm": false,
              "model": "HDD"
            },
            {
              "name": "sdc",
              "path": "/dev/sdc",
              "size": 100,
              "type": "disk",
              "tran": "usb",
              "rm": false,
              "model": "USB HDD",
              "children": [
                {"name": "sdc1", "path": "/dev/sdc1", "type": "part", "mountpoints": ["/"]}
              ]
            },
            {
              "name": "loop0",
              "path": "/dev/loop0",
              "size": 10,
              "type": "loop",
              "tran": "usb",
              "rm": true
            },
            {
              "name": "weird",
              "path": "/dev/sda1",
              "size": 10,
              "type": "disk",
              "tran": "usb",
              "rm": true
            },
            {
              "name": "escape",
              "path": "/dev/../etc/passwd",
              "size": 1,
              "type": "disk",
              "tran": "usb",
              "rm": true
            }
          ]
        }"#;
        let disks = filter_disks(json).unwrap();
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].path, "/dev/sda");
        assert_eq!(disks[0].model.as_deref(), Some("Flash"));
        assert_eq!(disks[0].size_bytes, 8_000_000_000);
        assert_eq!(disks[0].mountpoints, vec!["/media/disk".to_string()]);
    }

    #[test]
    fn rejects_paths_outside_the_disk_pattern() {
        assert!(disk_path_ok("/dev/sdb"));
        assert!(disk_path_ok("/dev/nvme0n1"));
        assert!(disk_path_ok("/dev/mmcblk0"));
        assert!(!disk_path_ok("/dev/sda1"));
        assert!(!disk_path_ok("/dev/nvme0n1p1"));
        assert!(!disk_path_ok("/dev/mmcblk0p1"));
        assert!(!disk_path_ok("/dev/sdaa"));
        assert!(!disk_path_ok("sda"));
        assert!(!disk_path_ok("/dev/../etc/passwd"));
    }
}
