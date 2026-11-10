//! What native scanning needs from Windows: administrator rights, and the WIA service
//! (`stisvc`) stopped while a scan runs. Epson's WIA driver polls the scanner every second,
//! which breaks into our sessions.
//!
//! An app elevates once at launch ([`relaunch_elevated`]); each scan then holds a
//! [`WiaPause`], which stops the service and starts it again when dropped. Call
//! [`restore_wia`] before exiting, in case a scan was still running. Windows starts the
//! elevated copy with a fresh environment, so the relaunch passes [`FORWARDED_ENV`] on as
//! arguments, which [`apply_forwarded_env`] sets again first thing.

use std::sync::Mutex;

use scan_core::ScanError;

/// Whether a scan currently has the WIA service stopped (and so must start it again).
static PAUSED: Mutex<bool> = Mutex::new(false);

/// Keeps the WIA service stopped while it lives; dropping it starts the service again if it
/// was running before.
#[derive(Debug)]
pub struct WiaPause {
    restart: bool,
}

impl WiaPause {
    /// Stop the WIA service if it runs. Needs administrator rights.
    pub fn new() -> Result<Self, ScanError> {
        let restart = imp::stop_wia()?;
        if restart {
            log::info!("stopped the WIA service for native scanning");
            *PAUSED.lock().unwrap_or_else(|e| e.into_inner()) = true;
        }
        Ok(Self { restart })
    }
}

impl Drop for WiaPause {
    fn drop(&mut self) {
        if self.restart {
            restore_wia();
        }
    }
}

/// Start the WIA service again if a [`WiaPause`] stopped it. Safe to call at any time.
pub fn restore_wia() {
    let mut paused = PAUSED.lock().unwrap_or_else(|e| e.into_inner());
    if *paused {
        match imp::start_wia() {
            Ok(()) => log::info!("restarted the WIA service"),
            Err(e) => log::error!("couldn't restart the WIA service (stisvc): {e}"),
        }
        *paused = false;
    }
}

pub use imp::{is_elevated, relaunch_elevated};

/// Environment variables the elevated copy keeps: the calibration dump and the log filter.
pub const FORWARDED_ENV: &[&str] = &["EPSON_CALIBRATION_DUMP", "RUST_LOG"];
/// Carries one of [`FORWARDED_ENV`] as `--env=NAME=VALUE`.
const ENV_ARG: &str = "--env=";

/// Set the [`FORWARDED_ENV`] variables a relaunch passed on. Call first thing in `main`,
/// before any other thread starts.
pub fn apply_forwarded_env() {
    for arg in std::env::args_os().skip(1) {
        let Some((name, value)) = arg
            .to_str()
            .and_then(|a| a.strip_prefix(ENV_ARG))
            .and_then(|a| a.split_once('='))
        else {
            continue;
        };
        if FORWARDED_ENV.contains(&name) {
            // SAFETY: called before any other thread starts.
            unsafe { std::env::set_var(name, value) };
        }
    }
}

/// The arguments for an elevated copy: this process's own, without any forwarded variables,
/// then the [`FORWARDED_ENV`] variables set now.
#[cfg_attr(not(windows), allow(dead_code))]
fn relaunch_args() -> Vec<String> {
    let forwarded = FORWARDED_ENV
        .iter()
        .filter_map(|&name| Some(format!("{ENV_ARG}{name}={}", std::env::var(name).ok()?)));
    std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with(ENV_ARG))
        .chain(forwarded)
        .collect()
}

/// Quote `arg` for a Windows command line, as `CommandLineToArgvW` splits it: backslashes
/// are only doubled before a quote.
#[cfg_attr(not(windows), allow(dead_code))]
fn quote_arg(arg: &str) -> String {
    let mut out = String::from('"');
    let mut backslashes = 0;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes + 1));
                backslashes = 0;
            }
            _ => backslashes = 0,
        }
        out.push(ch);
    }
    out.extend(std::iter::repeat_n('\\', backslashes));
    out.push('"');
    out
}

#[cfg(windows)]
mod imp {
    use std::time::{Duration, Instant};

    use scan_core::ScanError;
    use windows::Win32::Foundation::{CloseHandle, ERROR_SERVICE_ALREADY_RUNNING, HANDLE};
    use windows::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows::Win32::System::Services::{
        CloseServiceHandle, ControlService, OpenSCManagerW, OpenServiceW, QueryServiceStatus,
        SC_HANDLE, SC_MANAGER_CONNECT, SERVICE_CONTROL_STOP, SERVICE_QUERY_STATUS, SERVICE_RUNNING,
        SERVICE_START, SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STOP, SERVICE_STOPPED,
        StartServiceW,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, w};

    const SERVICE_TIMEOUT: Duration = Duration::from_secs(20);

    fn error(what: &str, e: windows::core::Error) -> ScanError {
        ScanError::Backend(format!("{what}: {e}"))
    }

    /// A service handle and its manager's, closed on drop.
    struct Service {
        manager: SC_HANDLE,
        service: SC_HANDLE,
    }

    impl Service {
        fn open_wia() -> Result<Self, ScanError> {
            // SAFETY: plain SCM calls; both handles are closed in `drop`.
            unsafe {
                let manager = OpenSCManagerW(None, None, SC_MANAGER_CONNECT)
                    .map_err(|e| error("open the service manager", e))?;
                match OpenServiceW(
                    manager,
                    w!("stisvc"),
                    SERVICE_STOP | SERVICE_START | SERVICE_QUERY_STATUS,
                ) {
                    Ok(service) => Ok(Self { manager, service }),
                    Err(e) => {
                        let _ = CloseServiceHandle(manager);
                        Err(ScanError::Backend(format!(
                            "native scanning needs the WIA service (stisvc) stopped, which \
                             needs administrator rights: {e}"
                        )))
                    }
                }
            }
        }

        fn state(&self) -> Result<SERVICE_STATUS_CURRENT_STATE, ScanError> {
            let mut status = SERVICE_STATUS::default();
            // SAFETY: `status` outlives the call.
            unsafe { QueryServiceStatus(self.service, &mut status) }
                .map_err(|e| error("query the WIA service", e))?;
            Ok(status.dwCurrentState)
        }

        fn wait_for(&self, state: SERVICE_STATUS_CURRENT_STATE) -> Result<(), ScanError> {
            let start = Instant::now();
            while self.state()? != state {
                if start.elapsed() > SERVICE_TIMEOUT {
                    return Err(ScanError::Backend(format!(
                        "the WIA service didn't reach state {} in time",
                        state.0
                    )));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(())
        }
    }

    impl Drop for Service {
        fn drop(&mut self) {
            // SAFETY: both handles came from the SCM and are closed once.
            unsafe {
                let _ = CloseServiceHandle(self.service);
                let _ = CloseServiceHandle(self.manager);
            }
        }
    }

    /// Stop the WIA service; returns whether it was running.
    pub fn stop_wia() -> Result<bool, ScanError> {
        let service = Service::open_wia()?;
        if service.state()? == SERVICE_STOPPED {
            return Ok(false);
        }
        let mut status = SERVICE_STATUS::default();
        // SAFETY: `status` outlives the call.
        unsafe { ControlService(service.service, SERVICE_CONTROL_STOP, &mut status) }
            .map_err(|e| error("stop the WIA service", e))?;
        service.wait_for(SERVICE_STOPPED)?;
        Ok(true)
    }

    pub fn start_wia() -> Result<(), ScanError> {
        let service = Service::open_wia()?;
        // SAFETY: no arguments are passed.
        match unsafe { StartServiceW(service.service, None) } {
            Ok(()) => {}
            Err(e) if e.code() == ERROR_SERVICE_ALREADY_RUNNING.to_hresult() => {}
            Err(e) => return Err(error("start the WIA service", e)),
        }
        service.wait_for(SERVICE_RUNNING)
    }

    /// Whether this process runs with administrator rights.
    pub fn is_elevated() -> bool {
        let mut token = HANDLE::default();
        let mut elevation = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        // SAFETY: the token handle is closed below; `elevation` outlives the call.
        unsafe {
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
                return false;
            }
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                Some((&raw mut elevation).cast()),
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut len,
            )
            .is_ok();
            let _ = CloseHandle(token);
            ok && elevation.TokenIsElevated != 0
        }
    }

    /// Start this program again with administrator rights (a UAC prompt) and the same
    /// arguments. Returns `Ok` once the elevated copy has started; the caller should exit.
    pub fn relaunch_elevated() -> Result<(), ScanError> {
        let exe = std::env::current_exe()?;
        let args: Vec<String> = super::relaunch_args()
            .iter()
            .map(|a| super::quote_arg(a))
            .collect();
        let cwd = std::env::current_dir()?;
        // SAFETY: all strings live for the call.
        let result = unsafe {
            ShellExecuteW(
                None,
                w!("runas"),
                &HSTRING::from(exe.as_os_str()),
                &HSTRING::from(args.join(" ")),
                &HSTRING::from(cwd.as_os_str()),
                SW_SHOWNORMAL,
            )
        };
        // ShellExecute returns a value above 32 on success.
        if result.0 as usize > 32 {
            Ok(())
        } else {
            Err(ScanError::Backend(format!(
                "couldn't start elevated (code {})",
                result.0 as usize
            )))
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use scan_core::ScanError;

    pub fn stop_wia() -> Result<bool, ScanError> {
        Ok(false)
    }

    pub fn start_wia() -> Result<(), ScanError> {
        Ok(())
    }

    pub fn is_elevated() -> bool {
        false
    }

    pub fn relaunch_elevated() -> Result<(), ScanError> {
        Err(ScanError::Unsupported(
            "elevation is only needed on Windows".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_for_the_windows_command_line() {
        assert_eq!(quote_arg("plain"), r#""plain""#);
        // A trailing backslash would otherwise escape the closing quote.
        assert_eq!(quote_arg(r"C:\tests\cal 6400\"), r#""C:\tests\cal 6400\\""#);
        assert_eq!(quote_arg(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_arg(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(
            quote_arg(r"--env=EPSON_CALIBRATION_DUMP=C:\x"),
            r#""--env=EPSON_CALIBRATION_DUMP=C:\x""#
        );
    }
}
