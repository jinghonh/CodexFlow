use std::{
    fs,
    path::{Path, PathBuf},
};

pub(crate) trait PermissionsExt {
    fn from_mode(mode: u32) -> fs::Permissions;
}

impl PermissionsExt for fs::Permissions {
    fn from_mode(mode: u32) -> fs::Permissions {
        #[cfg(unix)]
        {
            std::os::unix::fs::PermissionsExt::from_mode(mode)
        }
        #[cfg(windows)]
        {
            let mut permissions = fs::metadata(".").unwrap().permissions();
            permissions.set_readonly(mode & 0o200 == 0);
            permissions
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = mode;
            fs::Permissions::default()
        }
    }
}

pub(crate) fn fake_codex_binary(script: &Path) -> PathBuf {
    fs::write(
        &script,
        include_bytes!("../../codex/tests/fixtures/fake_codex.py"),
    )
    .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        script
    }

    #[cfg(windows)]
    {
        let launcher = script.with_extension("cmd");
        let script_name = script.file_name().unwrap().to_string_lossy();
        fs::write(
            &launcher,
            format!("@echo off\r\npython \"%~dp0{script_name}\" %*\r\nexit /b %ERRORLEVEL%\r\n"),
        )
        .unwrap();
        launcher
    }
}
