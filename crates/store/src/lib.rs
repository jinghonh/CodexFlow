use codexflow_domain::{AppError, Preferences};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

pub struct PreferenceStore {
    path: PathBuf,
}

impl PreferenceStore {
    pub fn new(app_data_dir: PathBuf) -> Self {
        Self {
            path: app_data_dir.join("preferences.json"),
        }
    }

    pub fn load(&self) -> Result<Preferences, AppError> {
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| {
                AppError::store("显示设置文件无法读取，请检查应用数据目录中的 preferences.json。")
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Preferences::default()),
            Err(_) => Err(AppError::store(
                "读取显示设置失败，请检查应用数据目录权限。",
            )),
        }
    }

    pub fn save(&self, preferences: &Preferences) -> Result<(), AppError> {
        let parent = self.path.parent().expect("preferences path has parent");
        fs::create_dir_all(parent)
            .map_err(|_| AppError::store("创建应用数据目录失败，请检查目录权限。"))?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp = parent.join(format!("preferences.{nonce}.tmp"));
        let bytes =
            serde_json::to_vec(preferences).map_err(|_| AppError::store("保存显示设置失败。"))?;
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temp, &self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result.map_err(|_| AppError::store("保存显示设置失败，请检查应用数据目录权限。"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codexflow_domain::DisplayTheme;

    #[test]
    fn selection_and_theme_survive_reopen() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("codexflow-store-test-{nonce}"));
        let store = PreferenceStore::new(dir.clone());
        store
            .save(&Preferences {
                selected_binary: Some("/tmp/codex".into()),
                theme: DisplayTheme::Dark,
            })
            .unwrap();
        let loaded = PreferenceStore::new(dir.clone()).load().unwrap();
        assert_eq!(loaded.selected_binary.as_deref(), Some("/tmp/codex"));
        assert!(matches!(loaded.theme, DisplayTheme::Dark));
        let _ = fs::remove_dir_all(dir);
    }
}
