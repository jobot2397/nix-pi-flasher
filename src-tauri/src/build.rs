use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::spawn::{signal_group, spawn_piped, stream_output, tool_exists};
use crate::state::with_inner;
use crate::validate::{BuildSettings, FlasherConfig, validate_settings};

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

struct BuildGuard {
    app: AppHandle,
}

impl Drop for BuildGuard {
    fn drop(&mut self) {
        with_inner(&self.app, |inner| {
            inner.build_running = false;
            inner.build_pid = None;
        });
    }
}

pub fn flake_root() -> PathBuf {
    if let Ok(path) = std::env::var("NIX_PI_FLASHER_FLAKE") {
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

pub fn nix_build_args(flake_root: &Path, out_link: &Path) -> Vec<String> {
    vec![
        "build".to_string(),
        "--impure".to_string(),
        "--extra-experimental-features".to_string(),
        "nix-command flakes".to_string(),
        format!("{}#packages.aarch64-linux.sdImage", flake_root.display()),
        "--print-build-logs".to_string(),
        "--out-link".to_string(),
        out_link.display().to_string(),
    ]
}

pub fn find_sd_image(out_link: &Path) -> Result<PathBuf, String> {
    let dir = out_link.join("sd-image");
    let entries = fs::read_dir(&dir).map_err(|err| {
        format!(
            "could not read {} for the built image: {err}",
            dir.display()
        )
    })?;
    let mut images = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| format!("could not read image directory: {err}"))?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("img") {
            images.push(path);
        }
    }
    images.sort();
    match images.len() {
        0 => Err(format!("no raw .img file in {}", dir.display())),
        1 => {
            let path = images.remove(0);
            fs::canonicalize(&path)
                .map_err(|err| format!("could not resolve image path {}: {err}", path.display()))
        }
        count => Err(format!(
            "expected one .img in {}, found {count}",
            dir.display()
        )),
    }
}

fn write_config(config: &FlasherConfig) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("nix-pi-flasher");
    fs::create_dir_all(&dir).map_err(|err| format!("could not create {}: {err}", dir.display()))?;
    let path = dir.join("flasher-config.json");
    let mut bytes = serde_json::to_vec_pretty(config)
        .map_err(|err| format!("could not encode config JSON: {err}"))?;
    bytes.push(b'\n');
    fs::write(&path, &bytes).map_err(|err| format!("could not write {}: {err}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    fs::canonicalize(&path).map_err(|err| format!("could not resolve {}: {err}", path.display()))
}

fn emit_log(app: &AppHandle, line: impl Into<String>) {
    let _ = app.emit("build-log", LogPayload { line: line.into() });
}

fn emit_finished(app: &AppHandle, ok: bool, image: Option<String>, error: Option<String>) {
    let _ = app.emit("build-finished", FinishedPayload { ok, image, error });
}

fn clear_running(app: &AppHandle) {
    with_inner(app, |inner| {
        inner.build_running = false;
        inner.build_pid = None;
    });
}

#[tauri::command]
pub fn last_image(app: AppHandle) -> Option<String> {
    with_inner(&app, |inner| inner.last_image.clone())
}

#[tauri::command]
pub fn start_build(app: AppHandle, settings: BuildSettings) -> Result<(), String> {
    let config = validate_settings(&settings)?;
    if !tool_exists("nix") {
        return Err(
            "Nix is not installed or not on PATH. Install Nix with flakes available (https://nixos.org/download). This app passes --extra-experimental-features 'nix-command flakes', so flakes do not have to be enabled in nix.conf.".into(),
        );
    }

    {
        let busy = with_inner(&app, |inner| {
            if inner.build_running {
                return Err("A build is already running".to_string());
            }
            if inner.flash_running {
                return Err("Cannot build while a disk is being flashed".to_string());
            }
            inner.build_running = true;
            inner.cancel_requested = false;
            inner.build_pid = None;
            Ok(())
        });
        busy?;
    }

    let config_path = match write_config(&config) {
        Ok(path) => path,
        Err(err) => {
            clear_running(&app);
            return Err(err);
        }
    };
    let root = flake_root();
    let out_link = std::env::temp_dir().join("nix-pi-flasher").join("result");
    if let Some(parent) = out_link.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            clear_running(&app);
            return Err(format!("could not create {}: {err}", parent.display()));
        }
    }
    let args = nix_build_args(&root, &out_link);

    std::thread::spawn(move || {
        let _guard = BuildGuard { app: app.clone() };
        match run_build(app.clone(), &root, &config_path, &out_link, &args) {
            Ok(image) => {
                with_inner(&app, |inner| inner.last_image = Some(image.clone()));
                emit_finished(&app, true, Some(image), None);
            }
            Err(err) => emit_finished(&app, false, None, Some(err)),
        }
    });

    Ok(())
}

fn run_build(
    app: AppHandle,
    root: &Path,
    config_path: &Path,
    out_link: &Path,
    args: &[String],
) -> Result<String, String> {
    emit_log(
        &app,
        format!(
            "nix build --impure {} (FLASHER_CONFIG={})",
            args.iter()
                .find(|arg| arg.contains("#packages.aarch64-linux.sdImage"))
                .map(String::as_str)
                .unwrap_or("packages.aarch64-linux.sdImage"),
            config_path.display()
        ),
    );

    let mut command = Command::new("nix");
    command
        .args(args)
        .current_dir(root)
        .env("FLASHER_CONFIG", config_path);
    let mut child =
        spawn_piped(command, true).map_err(|err| format!("failed to start nix: {err}"))?;
    let pid = child.id();

    let cancel_now = with_inner(&app, |inner| {
        if inner.cancel_requested {
            true
        } else {
            inner.build_pid = Some(pid);
            false
        }
    });
    #[cfg(unix)]
    if cancel_now {
        let _ = signal_group(pid, libc::SIGTERM);
    }
    #[cfg(not(unix))]
    let _ = cancel_now;

    let app_log = app.clone();
    let status = stream_output(&mut child, move |line| emit_log(&app_log, line))
        .map_err(|err| format!("failed while waiting for nix: {err}"))?;

    let cancelled = with_inner(&app, |inner| inner.cancel_requested);
    if status.success() {
        let image = find_sd_image(out_link)?;
        Ok(image.display().to_string())
    } else if cancelled {
        Err("Build cancelled".into())
    } else if let Some(code) = status.code() {
        Err(format!("nix exited with status {code}"))
    } else {
        Err(format!("nix exited: {status}"))
    }
}

#[tauri::command]
pub fn cancel_build(app: AppHandle) -> Result<(), String> {
    let pid = with_inner(&app, |inner| {
        if !inner.build_running {
            return Err("No build is running".to_string());
        }
        inner.cancel_requested = true;
        Ok(inner.build_pid)
    })?;

    #[cfg(unix)]
    {
        if let Some(pid) = pid {
            signal_group(pid, libc::SIGTERM)?;
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(2));
                let _ = signal_group(pid, libc::SIGKILL);
            });
        }
        Ok(())
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
        Err("Cancelling a build is only supported on Unix".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_args_are_an_argv_array() {
        let args = nix_build_args(Path::new("/repo"), Path::new("/tmp/nix-pi-flasher/result"));
        assert_eq!(
            args,
            vec![
                "build",
                "--impure",
                "--extra-experimental-features",
                "nix-command flakes",
                "/repo#packages.aarch64-linux.sdImage",
                "--print-build-logs",
                "--out-link",
                "/tmp/nix-pi-flasher/result",
            ]
        );
        assert!(args.iter().all(|arg| !arg.contains("sh -c")));
    }

    #[test]
    fn finds_the_raw_image_only() {
        let root = std::env::temp_dir().join(format!("nix-pi-flasher-img-{}", std::process::id()));
        let dir = root.join("sd-image");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("nixos.img.zst"), b"compressed").unwrap();
        fs::write(dir.join("nixos.img"), b"raw").unwrap();
        let found = find_sd_image(&root).unwrap();
        assert_eq!(
            found.file_name().and_then(|name| name.to_str()),
            Some("nixos.img")
        );
        let _ = fs::remove_dir_all(&root);
    }
}
