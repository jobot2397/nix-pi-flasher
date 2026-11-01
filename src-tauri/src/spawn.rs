use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::thread;

pub fn tool_exists(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

#[cfg(unix)]
pub fn configure_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            let rc = libc::setpgid(0, 0);
            if rc != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
pub fn configure_process_group(_cmd: &mut Command) {}

#[cfg(unix)]
pub fn signal_group(pid: u32, sig: i32) -> Result<(), String> {
    if pid == 0 {
        return Err("refusing to signal process group 0".into());
    }
    let rc = unsafe { libc::kill(-(pid as i32), sig) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(format!("failed to signal process group {pid}: {err}"))
    }
}

#[cfg(not(unix))]
pub fn signal_group(_pid: u32, _sig: i32) -> Result<(), String> {
    Err("process groups are only supported on Unix".into())
}

pub fn read_lines(reader: impl Read, emit: Arc<dyn Fn(String) + Send + Sync>) {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {
                if buf.last() == Some(&b'\n') {
                    buf.pop();
                }
                if buf.last() == Some(&b'\r') {
                    buf.pop();
                }
                let text = String::from_utf8_lossy(&buf);
                let line = match text.rsplit_once('\r') {
                    Some((_, last)) => last.to_string(),
                    None => text.into_owned(),
                };
                if !line.is_empty() {
                    emit(line);
                }
            }
            Err(_) => break,
        }
    }
}

pub fn spawn_piped(mut cmd: Command, new_group: bool) -> std::io::Result<Child> {
    if new_group {
        configure_process_group(&mut cmd);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

pub fn stream_output(
    child: &mut Child,
    emit: impl Fn(String) + Send + Sync + 'static,
) -> std::io::Result<ExitStatus> {
    let emit = Arc::new(emit);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_thread = stdout.map(|pipe| {
        let emit = Arc::clone(&emit);
        thread::spawn(move || read_lines(pipe, emit))
    });
    let stderr_thread = stderr.map(|pipe| {
        let emit = Arc::clone(&emit);
        thread::spawn(move || read_lines(pipe, emit))
    });
    let status = child.wait()?;
    if let Some(handle) = stdout_thread {
        let _ = handle.join();
    }
    if let Some(handle) = stderr_thread {
        let _ = handle.join();
    }
    Ok(status)
}
