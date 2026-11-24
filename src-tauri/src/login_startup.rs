use crate::constants::APP_ID;
use std::path::{Path, PathBuf};

pub const LABEL: &str = "com.meternest.desktop.tray";
const MARKER: &str = "<!-- MeterNest managed tray login item -->";
pub const TRAY_ONLY_ARG: &str = "--tray-only";

pub struct LoginStartup {
    pub supported: bool,
    packaged: bool,
    home: PathBuf,
    uid: u32,
}

fn uid() -> u32 {
    unsafe { libc::getuid() }
}

impl LoginStartup {
    pub fn new(packaged: bool) -> LoginStartup {
        LoginStartup {
            supported: cfg!(target_os = "macos"),
            packaged,
            home: dirs::home_dir().unwrap_or_default(),
            uid: uid(),
        }
    }

    fn file(&self) -> PathBuf {
        self.home.join("Library/LaunchAgents").join(format!("{}.plist", LABEL))
    }

    /// Resolves the .app bundle containing the running executable.
    fn application_bundle(&self) -> Result<PathBuf, String> {
        let exe = std::env::current_exe().map_err(|_| "无法定位应用位置".to_string())?;
        let bundle = exe
            .ancestors()
            .nth(3)
            .map(Path::to_path_buf)
            .ok_or_else(|| "无法定位应用位置".to_string())?;
        let text = bundle.to_string_lossy().to_string();
        if !text.ends_with(".app") || text.contains("/AppTranslocation/") || text.starts_with("/Volumes/") {
            return Err("请先将 MeterNest 安装到“应用程序”，再开启开机启动".to_string());
        }
        Ok(bundle)
    }

    fn owned_file(&self) -> Result<bool, String> {
        let file = self.file();
        let stat = std::fs::symlink_metadata(&file).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "ENOENT".to_string()
            } else {
                error.to_string()
            }
        })?;
        use std::os::unix::fs::MetadataExt;
        if !stat.is_file() || stat.is_symlink() || stat.size() > 65536 || stat.uid() != self.uid {
            return Err("启动项文件归属异常，未修改".to_string());
        }
        let body = std::fs::read_to_string(&file).map_err(|error| error.to_string())?;
        if !body.contains(MARKER) || !body.contains(&format!("<string>{}</string>", LABEL)) {
            return Err("发现同名非 MeterNest 启动项，未修改".to_string());
        }
        Ok(true)
    }

    fn remove_agent_file(&self) -> Result<(), String> {
        match self.owned_file() {
            Ok(true) => std::fs::remove_file(self.file()).map_err(|error| error.to_string())?,
            Ok(false) => {}
            Err(error) if error == "ENOENT" => {}
            Err(error) => return Err(error),
        }
        Ok(())
    }

    fn write_agent_file(&self) -> Result<(), String> {
        self.application_bundle()?;
        let exe = std::env::current_exe().map_err(|error| error.to_string())?;
        let args = [exe.to_string_lossy().to_string(), TRAY_ONLY_ARG.to_string()];
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{}\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array>{}{}{}</array><key>AssociatedBundleIdentifiers</key><array><string>{}</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><false/></dict></plist>\n",
            MARKER,
            LABEL,
            format!("<string>{}</string>", xml(&args[0])),
            format!("<string>{}</string>", xml(&args[1])),
            "",
            APP_ID
        );
        let file = self.file();
        if let Ok(existing) = std::fs::read_to_string(&file) {
            if existing == body {
                return Ok(());
            }
        }
        let directory = file.parent().map(Path::to_path_buf).unwrap_or_default();
        std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        std::fs::write(&file, body).map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Native SMAppService registration (macOS 13+); unsigned builds fall back
    /// to the LaunchAgent, matching the Electron implementation.
    fn native_status(&self) -> Option<i64> {
        unsafe {
            let cls = objc2::runtime::AnyClass::get(c"SMAppService")?;
            let service: *mut objc2::runtime::AnyObject = objc2::msg_send![cls, mainAppService];
            if service.is_null() {
                return None;
            }
            let status: i64 = objc2::msg_send![service, status];
            Some(status)
        }
    }

    fn enable_native(&self) -> bool {
        unsafe {
            let Some(cls) = objc2::runtime::AnyClass::get(c"SMAppService") else { return false };
            let service: *mut objc2::runtime::AnyObject = objc2::msg_send![cls, mainAppService];
            if service.is_null() {
                return false;
            }
            let mut error: *mut objc2::runtime::AnyObject = std::ptr::null_mut();
            let ok: bool = objc2::msg_send![service, registerAndReturnError: &mut error];
            ok && self.native_status() == Some(1)
        }
    }

    fn disable_native(&self) -> Result<(), String> {
        unsafe {
            if matches!(self.native_status(), Some(1) | Some(2)) {
                let Some(cls) = objc2::runtime::AnyClass::get(c"SMAppService") else {
                    return Ok(());
                };
                let service: *mut objc2::runtime::AnyObject = objc2::msg_send![cls, mainAppService];
                if service.is_null() {
                    return Ok(());
                }
                let mut error: *mut objc2::runtime::AnyObject = std::ptr::null_mut();
                let ok: bool = objc2::msg_send![service, unregisterAndReturnError: &mut error];
                if !ok {
                    return Err("系统未能关闭开机启动，请检查登录项权限后重试".to_string());
                }
            }
            Ok(())
        }
    }

    pub fn set_enabled(&self, enabled: bool) -> Result<(), String> {
        if !self.supported {
            return Err("当前系统不支持开机启动".to_string());
        }
        if enabled && !self.packaged {
            return Err("开机启动仅在已安装的正式应用中可用".to_string());
        }
        if !enabled {
            self.disable_native()?;
            self.remove_agent_file()?;
            return Ok(());
        }
        let exists = match self.owned_file() {
            Ok(exists) => exists,
            Err(error) if error == "ENOENT" => false,
            Err(error) => return Err(error),
        };
        self.application_bundle()?;
        if self.enable_native() {
            if exists {
                let _ = std::fs::remove_file(self.file());
            }
            return Ok(());
        }
        self.write_agent_file()
    }

    /// Set by the LaunchAgent path via --tray-only; the native login item
    /// path cannot pass arguments.
    pub fn was_opened_at_login(&self, argv: &[String]) -> bool {
        self.supported && argv.iter().any(|arg| arg == TRAY_ONLY_ARG)
    }

    /// Re-apply a stored "open at login" preference after an app update.
    pub fn reconcile(&self) {
        if !self.packaged {
            return;
        }
        let _ = self.set_enabled(true);
    }
}

fn xml(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            '\'' => "&apos;".to_string(),
            other => other.to_string(),
        })
        .collect()
}

