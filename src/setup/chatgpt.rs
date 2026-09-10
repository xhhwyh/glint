use std::{
    io::{self, Write},
    process::{Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

#[derive(Clone)]
pub(super) struct BrowserJob(Arc<Mutex<mpsc::Receiver<bool>>>);

impl BrowserJob {
    pub fn start(url: String) -> Self {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(open_browser(&url).is_ok());
        });
        Self(Arc::new(Mutex::new(receiver)))
    }

    pub fn poll(&self) -> Option<bool> {
        self.0.lock().ok()?.try_recv().ok()
    }
}

fn browser_command(wsl: bool) -> Command {
    if wsl {
        let mut command = Command::new("powershell.exe");
        // The URL travels through stdin as data, never as PowerShell source.
        command.args(["-NoProfile", "-NonInteractive", "-Command",
            "$ErrorActionPreference = 'Stop'; $url = [Console]::In.ReadLine(); Start-Process -FilePath $url"]);
        command
    } else {
        Command::new(if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        })
    }
}

fn open_browser(url: &str) -> io::Result<()> {
    let wsl = cfg!(target_os = "linux")
        && std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .is_ok_and(|release| release.to_ascii_lowercase().contains("microsoft"));
    let mut command = browser_command(wsl);
    if !wsl {
        command.arg(url);
    }
    let mut child = command
        .stdin(if wsl { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take()
        && writeln!(stdin, "{url}").is_err()
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err(io::Error::other("Browser launcher input failed"));
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => return Err(io::Error::other("Browser launcher failed")),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::other("Browser launcher timed out"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wsl_browser_command_keeps_login_url_out_of_script() {
        let command = browser_command(true);
        assert_eq!(command.get_program(), "powershell.exe");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>();
        assert!(args.last().unwrap().contains("ReadLine"));
        assert!(!args.last().unwrap().contains("https://"));
    }
}
