//! Installing and uninstalling despegate system-wide.
//!
//! Installed, the daemon is a Windows service running as SYSTEM. That is what
//! makes it hard to stop: it refuses stop requests, an ordinary user cannot
//! end its process, and the service manager restarts it if someone with
//! administrator rights does. Uninstalling is the one sanctioned way out, and
//! it shows the user's own reasons first.

use std::ffi::{OsStr, OsString};
use std::io::{self, BufRead, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::null_mut;
use std::time::Duration;

use windows_service::service::{
    Service, ServiceAccess, ServiceAction, ServiceActionType, ServiceErrorControl,
    ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState,
    ServiceType,
};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows_sys::Win32::UI::Shell::{
    IsUserAnAdmin, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, SMTO_ABORTIFHUNG, SW_SHOWNORMAL, SendMessageTimeoutW, WM_SETTINGCHANGE,
};
use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE};
use winreg::{RegKey, RegValue};

use crate::config::Config;
use crate::i18n::Lang;
use crate::paths::Paths;
use crate::session::DAEMON_EXE;
use crate::store::Store;
use crate::ui::UI_EXE;
use crate::winsvc::SERVICE_NAME;
use crate::{tr, wide};

const CLI_EXE: &str = "despegate.exe";
const UNINSTALL_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\despegate";
const ENVIRONMENT_KEY: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn install_dir() -> PathBuf {
    std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"))
        .join("despegate")
}

/// Installs despegate. Needs administrator rights and asks for them (UAC)
/// when the caller does not have them.
pub fn install(paths: &Paths, lang: Lang) -> Result<(), String> {
    if !is_elevated() {
        println!("{}", tr!(lang, "install.needs_admin"));
        return run_elevated(&["install", "--pause"], lang);
    }

    let source = std::env::current_exe().map_err(|e| e.to_string())?;
    let source = source
        .parent()
        .ok_or_else(|| tr!(lang, "install.missing_exe", exe = CLI_EXE))?;
    let target = install_dir();
    for exe in [CLI_EXE, DAEMON_EXE, UI_EXE] {
        if !source.join(exe).exists() {
            return Err(tr!(lang, "install.missing_exe", exe = exe));
        }
    }

    println!("{}", tr!(lang, "install.stopping"));
    stop_daemon(paths);
    if source != target {
        std::fs::create_dir_all(&target)
            .map_err(|e| tr!(lang, "install.failed", what = target.display(), error = e))?;
        for exe in [CLI_EXE, DAEMON_EXE, UI_EXE] {
            copy_with_retry(&source.join(exe), &target.join(exe)).map_err(|e| {
                tr!(
                    lang,
                    "install.failed",
                    what = target.join(exe).display(),
                    error = e
                )
            })?;
        }
    }

    // Only SYSTEM and administrators may change anything in the data
    // directory; everyone else can read it.
    std::fs::create_dir_all(&paths.home).map_err(|e| {
        tr!(
            lang,
            "install.failed",
            what = paths.home.display(),
            error = e
        )
    })?;
    run(
        "icacls",
        &[
            &paths.home.to_string_lossy(),
            "/inheritance:r",
            "/grant:r",
            "*S-1-5-18:(OI)(CI)F",
            "*S-1-5-32-544:(OI)(CI)F",
            "*S-1-5-32-545:(OI)(CI)RX",
        ],
    )
    .map_err(|e| tr!(lang, "install.failed", what = "icacls", error = e))?;

    let service = register_service(&target.join(DAEMON_EXE))
        .map_err(|e| tr!(lang, "install.failed", what = "service", error = e))?;
    if let Err(e) = start_menu_shortcut(Some(&target.join(UI_EXE))) {
        println!(
            "{}",
            tr!(lang, "install.failed", what = "Start menu", error = e)
        );
    }
    register_uninstaller(&target)
        .map_err(|e| tr!(lang, "install.failed", what = "registry", error = e))?;
    let dir = target.to_string_lossy().into_owned();
    edit_machine_path(|path| path_with(path, &dir))
        .map_err(|e| tr!(lang, "install.failed", what = "PATH", error = e))?;

    let _ = std::fs::remove_file(paths.stop_marker());
    service.start(&[] as &[&OsStr]).map_err(|e| {
        tr!(
            lang,
            "install.failed",
            what = "service start",
            error = describe(&e)
        )
    })?;

    println!();
    println!("{}", tr!(lang, "install.done"));
    println!("{}", tr!(lang, "install.program", path = target.display()));
    println!(
        "{}",
        tr!(lang, "install.config", path = paths.config().display())
    );
    println!();
    println!("{}", tr!(lang, "install.next"));
    println!("  {}", tr!(lang, "install.example_reasons"));
    println!("  despegate rule add sleep --from 23:00 --to 07:00 --lock");
    Ok(())
}

/// Removes despegate after showing the user's reasons and asking them to
/// confirm in writing.
pub fn uninstall(paths: &Paths, lang: Lang) -> Result<(), String> {
    let target = install_dir();
    if !target.exists() && open_service(ServiceAccess::QUERY_STATUS).is_err() {
        return Err(tr!(lang, "uninstall.not_installed"));
    }
    if !is_elevated() {
        println!("{}", tr!(lang, "uninstall.needs_admin"));
        return run_elevated(&["uninstall", "--pause"], lang);
    }

    let config = Store::<Config>::peek(&paths.config());
    let reasons = config.reasons;
    let phrase = tr!(lang, "uninstall.phrase");
    let rule = "-".repeat(64);
    println!("{rule}");
    println!("{}", tr!(lang, "uninstall.about"));
    println!();
    if reasons.is_empty() {
        println!("{}", tr!(lang, "uninstall.no_reasons"));
    } else {
        println!("{}", tr!(lang, "uninstall.your_reasons"));
        println!();
        for line in reasons.lines() {
            println!("    {line}");
        }
    }
    if !config.media.is_empty() {
        println!();
        println!(
            "    {}",
            tr!(lang, "uninstall.media", list = config.media.join(", "))
        );
    }
    println!("{rule}");
    println!();
    println!("{}", tr!(lang, "uninstall.confirm", phrase = phrase));
    print!("> ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|e| e.to_string())?;
    if answer.trim() != phrase {
        return Err(tr!(lang, "uninstall.not_confirmed"));
    }

    stop_daemon(paths);
    if let Ok(service) = open_service(ServiceAccess::DELETE) {
        let _ = service.delete();
    }
    let _ = RegKey::predef(HKEY_LOCAL_MACHINE).delete_subkey_all(UNINSTALL_KEY);
    let _ = start_menu_shortcut(None);
    let dir = target.to_string_lossy().into_owned();
    if let Err(e) = edit_machine_path(|path| path_without(path, &dir)) {
        println!(
            "{}",
            tr!(lang, "uninstall.path_failed", dir = dir, error = e)
        );
    }
    let _ = std::fs::remove_dir_all(&paths.home);
    // The agent's log lives in the profile of whoever is uninstalling.
    if let Some(agent_dir) = paths.agent_log().parent() {
        let _ = std::fs::remove_dir_all(agent_dir);
    }

    let running_from_target = std::env::current_exe().is_ok_and(|exe| exe.starts_with(&target));
    if running_from_target {
        // A running executable cannot delete itself, so a detached shell does
        // it a moment after this process is gone.
        let _ = Command::new("cmd")
            .raw_arg(format!(
                "/c ping -n 4 127.0.0.1 >nul & rmdir /s /q \"{dir}\""
            ))
            .current_dir(std::env::temp_dir())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    } else {
        let _ = std::fs::remove_dir_all(&target);
    }
    println!("{}", tr!(lang, "uninstall.done"));
    Ok(())
}

/// Waits for the user before a console window opened by elevation closes.
pub fn pause(lang: Lang) {
    println!();
    print!("{}", tr!(lang, "pause.prompt"));
    let _ = io::stdout().flush();
    let _ = io::stdin().lock().read_line(&mut String::new());
}

fn is_elevated() -> bool {
    unsafe { IsUserAnAdmin() != 0 }
}

/// Runs this executable again with administrator rights and waits for it.
fn run_elevated(args: &[&str], lang: Lang) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = wide(&exe.to_string_lossy());
    let verb = wide("runas");
    let parameters = wide(&args.join(" "));
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = exe.as_ptr();
    info.lpParameters = parameters.as_ptr();
    info.nShow = SW_SHOWNORMAL;
    unsafe {
        if ShellExecuteExW(&mut info) == 0 || info.hProcess.is_null() {
            return Err(tr!(lang, "install.denied"));
        }
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 1;
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
        if code == 0 {
            Ok(())
        } else {
            Err(tr!(lang, "install.elevated_failed"))
        }
    }
}

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    Err(format!("{} {}", stderr.trim(), stdout.trim()))
}

/// The service manager's errors hide the Windows error they wrap.
fn describe(error: &windows_service::Error) -> String {
    match std::error::Error::source(error) {
        Some(source) => format!("{error}: {source}"),
        None => error.to_string(),
    }
}

fn open_service(access: ServiceAccess) -> Result<Service, String> {
    ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .and_then(|manager| manager.open_service(SERVICE_NAME, access))
        .map_err(|e| describe(&e))
}

/// Creates the service, or points an existing one at the new executable.
fn register_service(daemon: &Path) -> Result<Service, String> {
    let info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from("despegate"),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: daemon.to_path_buf(),
        launch_arguments: vec![OsString::from("service")],
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };
    let access = ServiceAccess::CHANGE_CONFIG | ServiceAccess::START | ServiceAccess::QUERY_STATUS;
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .map_err(|e| describe(&e))?;
    let service = match manager.open_service(SERVICE_NAME, access) {
        Ok(service) => {
            service.change_config(&info).map_err(|e| describe(&e))?;
            service
        }
        Err(_) => manager
            .create_service(&info, access)
            .map_err(|e| describe(&e))?,
    };
    service
        .set_description("Keeps despegate running. To remove it, run: despegate uninstall")
        .map_err(|e| describe(&e))?;
    // Started again a second after it dies, however often that happens: past
    // the last listed action the service manager repeats it.
    let restart = ServiceAction {
        action_type: ServiceActionType::Restart,
        delay: Duration::from_secs(1),
    };
    service
        .update_failure_actions(ServiceFailureActions {
            reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(60)),
            reboot_msg: None,
            command: None,
            actions: Some(vec![restart.clone(), restart.clone(), restart]),
        })
        .map_err(|e| describe(&e))?;
    service
        .set_failure_actions_on_non_crash_failures(true)
        .map_err(|e| describe(&e))?;
    Ok(service)
}

/// Asks the daemon to exit and waits until it has. The stop marker is left in
/// place; the caller removes it when the daemon may run again.
fn stop_daemon(paths: &Paths) {
    let Ok(service) = open_service(ServiceAccess::QUERY_STATUS) else {
        return;
    };
    let stopped = || {
        service
            .query_status()
            .is_ok_and(|s| s.current_state == ServiceState::Stopped)
    };
    if stopped() {
        return;
    }
    let _ = std::fs::create_dir_all(&paths.home);
    let _ = std::fs::write(paths.stop_marker(), b"");
    for _ in 0..60 {
        if stopped() {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    if !stopped() {
        // It restarts, finds the marker and stops cleanly.
        let _ = run("taskkill", &["/f", "/im", DAEMON_EXE]);
        std::thread::sleep(Duration::from_secs(3));
    }
}

/// The previous daemon's executable can stay locked for a moment after it exits.
fn copy_with_retry(from: &Path, to: &Path) -> io::Result<()> {
    let mut attempts = 0;
    loop {
        match std::fs::copy(from, to) {
            Ok(_) => return Ok(()),
            Err(_) if attempts < 20 => {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Creates (or, with `None`, removes) the shortcut to the settings window in
/// the Start menu of every user.
fn start_menu_shortcut(target: Option<&Path>) -> Result<(), String> {
    let programs = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join(r"Microsoft\Windows\Start Menu\Programs");
    let link = programs.join("despegate.lnk");
    let Some(target) = target else {
        let _ = std::fs::remove_file(&link);
        return Ok(());
    };
    // Shortcuts are COM objects; the scripting shell writes one in a line.
    let script = format!(
        "$s = (New-Object -ComObject WScript.Shell).CreateShortcut('{}'); $s.TargetPath = '{}'; $s.WorkingDirectory = '{}'; $s.Save()",
        link.display(),
        target.display(),
        target
            .parent()
            .map(Path::display)
            .map(|d| d.to_string())
            .unwrap_or_default()
    );
    run(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", &script],
    )
}

fn register_uninstaller(target: &Path) -> io::Result<()> {
    let (key, _) = RegKey::predef(HKEY_LOCAL_MACHINE).create_subkey(UNINSTALL_KEY)?;
    let cli = target.join(CLI_EXE);
    key.set_value("DisplayName", &"despegate")?;
    key.set_value("DisplayVersion", &env!("CARGO_PKG_VERSION"))?;
    key.set_value("Publisher", &"despegate")?;
    key.set_value("InstallLocation", &target.to_string_lossy().as_ref())?;
    key.set_value("DisplayIcon", &cli.to_string_lossy().as_ref())?;
    key.set_value(
        "UninstallString",
        &format!("\"{}\" uninstall --pause", cli.display()),
    )?;
    key.set_value("NoModify", &1u32)?;
    key.set_value("NoRepair", &1u32)?;
    Ok(())
}

/// Rewrites the machine-wide PATH, keeping its registry type (it is usually
/// REG_EXPAND_SZ and must stay so). `edit` returns `None` to leave it as is.
fn edit_machine_path(edit: impl Fn(&str) -> Option<String>) -> io::Result<()> {
    let key = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(ENVIRONMENT_KEY, KEY_READ | KEY_WRITE)?;
    let raw = key.get_raw_value("Path")?;
    let units: Vec<u16> = raw
        .bytes
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    let current = String::from_utf16(&units).map_err(io::Error::other)?;
    let Some(updated) = edit(current.trim_end_matches('\0')) else {
        return Ok(());
    };
    let bytes: Vec<u8> = updated
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    key.set_raw_value(
        "Path",
        &RegValue {
            bytes: bytes.into(),
            vtype: raw.vtype,
        },
    )?;
    // Tell running programs (Explorer, so new terminals) that PATH changed.
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            wide("Environment").as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            2000,
            null_mut(),
        );
    }
    Ok(())
}

fn same_dir(a: &str, b: &str) -> bool {
    a.trim()
        .trim_end_matches('\\')
        .eq_ignore_ascii_case(b.trim_end_matches('\\'))
}

fn path_with(path: &str, dir: &str) -> Option<String> {
    if path.split(';').any(|entry| same_dir(entry, dir)) {
        return None;
    }
    Some(format!("{};{dir}", path.trim_end_matches(';')))
}

fn path_without(path: &str, dir: &str) -> Option<String> {
    if !path.split(';').any(|entry| same_dir(entry, dir)) {
        return None;
    }
    Some(
        path.split(';')
            .filter(|entry| !same_dir(entry, dir))
            .collect::<Vec<_>>()
            .join(";"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: &str = r"C:\Program Files\despegate";

    #[test]
    fn path_gains_the_directory_once() {
        let path = r"C:\Windows;%SystemRoot%\system32;";
        let updated = path_with(path, DIR).unwrap();
        assert_eq!(
            updated,
            r"C:\Windows;%SystemRoot%\system32;C:\Program Files\despegate"
        );
        assert_eq!(path_with(&updated, DIR), None);
        assert_eq!(
            path_with(r"c:\program files\DESPEGATE\;C:\Windows", DIR),
            None
        );
    }

    #[test]
    fn path_loses_only_the_directory() {
        let path = r"C:\Windows;C:\Program Files\despegate;%SystemRoot%\system32";
        assert_eq!(
            path_without(path, DIR).unwrap(),
            r"C:\Windows;%SystemRoot%\system32"
        );
        assert_eq!(path_without(r"C:\Windows", DIR), None);
    }
}
