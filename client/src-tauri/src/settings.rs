//! 앱 설정(계층 5, 엔진 ⑦-2). Tauri를 모른다 — 경로 조회(`app_config_dir` 등)와 대화상자는 어댑터(`commands`)에 있다.

use std::path::PathBuf;

use parking_lot::Mutex;

/// 녹음 폴더 설정 파일 이름(앱 설정 폴더 아래).
pub const RECORD_DIR_FILE: &str = "recording.json";

/// 녹음 저장 폴더(P2-4). 우선순위: 고른 폴더 → 저장된 폴더 → 기본.
/// 저장 형식은 `{ "folder": "<절대 경로>" }`. 상대 경로는 무시한다(기본 폴더로).
pub struct RecordDir {
    config_file: Option<PathBuf>,
    picked: Mutex<Option<PathBuf>>,
}

impl RecordDir {
    /// `config_file`: 설정 파일 경로(보통 앱 설정 폴더/`RECORD_DIR_FILE`). 없으면 저장·읽기를 하지 않는다.
    pub fn new(config_file: Option<PathBuf>) -> Self {
        Self { config_file, picked: Mutex::new(None) }
    }

    /// 현재 녹음 폴더: 고른 폴더 → 저장된 폴더(읽으면 기억) → `default()`.
    pub fn current(&self, default: impl FnOnce() -> Result<PathBuf, String>) -> Result<PathBuf, String> {
        if let Some(p) = self.picked.lock().clone() {
            return Ok(p);
        }
        let saved = self
            .config_file
            .as_ref()
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| v.get("folder").and_then(|x| x.as_str()).map(PathBuf::from))
            .filter(|p| p.is_absolute());
        if let Some(p) = saved {
            *self.picked.lock() = Some(p.clone());
            return Ok(p);
        }
        default()
    }

    /// 고른 폴더로 바꾸고 설정 파일에 남긴다. 저장에 실패해도 이번 실행 동안은 고른 폴더를 쓴다(오류는 돌려준다).
    pub fn set(&self, path: PathBuf) -> std::io::Result<()> {
        *self.picked.lock() = Some(path.clone());
        let Some(f) = &self.config_file else {
            return Ok(());
        };
        if let Some(dir) = f.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(f, serde_json::json!({ "folder": path }).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("band-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn fallback() -> Result<PathBuf, String> {
        Ok(PathBuf::from("DEFAULT"))
    }

    #[test]
    fn record_dir_saved_then_read_by_new_instance() {
        let dir = tmp_dir("roundtrip");
        // 설정 폴더가 아직 없어도 만든다
        let file = dir.join("cfg").join(RECORD_DIR_FILE);
        let picked = dir.join("rec");
        assert!(picked.is_absolute());

        let a = RecordDir::new(Some(file.clone()));
        assert_eq!(a.current(fallback).unwrap(), PathBuf::from("DEFAULT"));
        a.set(picked.clone()).unwrap();
        assert_eq!(a.current(fallback).unwrap(), picked);

        // 형식: { "folder": .. }
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(v["folder"].as_str().map(PathBuf::from), Some(picked.clone()));

        let b = RecordDir::new(Some(file));
        assert_eq!(b.current(|| panic!("저장된 폴더가 있으면 기본을 부르지 않는다")).unwrap(), picked);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_dir_rejects_relative_saved_path() {
        let dir = tmp_dir("relative");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(RECORD_DIR_FILE);
        std::fs::write(&file, r#"{ "folder": "relative/rec" }"#).unwrap();
        assert_eq!(RecordDir::new(Some(file.clone())).current(fallback).unwrap(), PathBuf::from("DEFAULT"));
        // 깨진 파일도 기본으로
        std::fs::write(&file, b"not json").unwrap();
        assert_eq!(RecordDir::new(Some(file)).current(fallback).unwrap(), PathBuf::from("DEFAULT"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_dir_saved_and_memory_only() {
        let dir = tmp_dir("priority");
        let file = dir.join(RECORD_DIR_FILE);
        let saved = dir.join("saved");
        RecordDir::new(Some(file.clone())).set(saved.clone()).unwrap();

        let r = RecordDir::new(Some(file));
        assert_eq!(r.current(fallback).unwrap(), saved);
        // 설정 파일 없음 = 저장하지 않고 이번 실행만
        let mem = RecordDir::new(None);
        let p = dir.join("picked");
        mem.set(p.clone()).unwrap();
        assert_eq!(mem.current(fallback).unwrap(), p);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_dir_default_error_passes_through() {
        let r = RecordDir::new(None);
        assert!(r.current(|| Err("없음".into())).is_err());
    }
}
