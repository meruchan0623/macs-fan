//! Independent local configuration. This module never imports or calls hardware code.
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

const MAX_BYTES: u64 = 1024 * 1024;
const MAX_REVISION: u64 = 9_007_199_254_740_991;
const TARGET: &str = "preferences.json";
const LOCK: &str = ".preferences.lock";
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Document {
    pub schema_version: u32,
    pub revision: u64,
    pub presets: Vec<Preset>,
    #[serde(deserialize_with = "required_nullable")]
    pub editor_preset_id: Option<String>,
    pub view: View,
}
impl Default for Document {
    fn default() -> Self {
        Self {
            schema_version: 1,
            revision: 0,
            presets: vec![],
            editor_preset_id: None,
            view: View::default(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub model: String,
    pub architecture: String,
    pub fans: Vec<Fan>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PresetInput {
    #[serde(deserialize_with = "required_nullable")]
    pub id: Option<String>,
    pub name: String,
    pub model: String,
    pub architecture: String,
    pub fans: Vec<Fan>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Fan {
    pub fan_id: String,
    pub mode: Mode,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum Mode {
    Automatic {},
    Fixed {
        rpm: u32,
    },
    Sensor {
        #[serde(rename = "sensorId")]
        sensor_id: String,
        #[serde(rename = "minRpm")]
        min_rpm: u32,
        #[serde(rename = "maxRpm")]
        max_rpm: u32,
        #[serde(rename = "lowerTemp")]
        lower_temp: f64,
        #[serde(rename = "upperTemp")]
        upper_temp: f64,
    },
}
macro_rules! choice {
    ($name:ident { $($variant:ident => $wire:literal),+ }) => {
        #[derive(Clone, Debug, PartialEq, Serialize)]
        #[serde(rename_all = "camelCase")]
        pub enum $name { $($variant),+ }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
                // Derived externally-tagged unit enums also accept {"variant":null}.
                // The configuration contract deliberately permits strings only.
                let value = String::deserialize(deserializer)?;
                match value.as_str() {
                    $($wire => Ok(Self::$variant),)+
                    _ => Err(serde::de::Error::unknown_variant(&value, &[$($wire),+])),
                }
            }
        }
    };
}
choice!(Scope { Overview => "overview", Components => "components", All => "all" });
choice!(Sort { Name => "name", Temperature => "temperature" });
choice!(Unit { Celsius => "celsius", Fahrenheit => "fahrenheit" });
choice!(Precision { Whole => "whole", Decimal => "decimal" });
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct View {
    pub scope: Scope,
    pub sort: Sort,
    pub unit: Unit,
    pub precision: Precision,
}
impl Default for View {
    fn default() -> Self {
        Self {
            scope: Scope::Overview,
            sort: Sort::Name,
            unit: Unit::Celsius,
            precision: Precision::Decimal,
        }
    }
}
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum Update {
    SavePreset {
        preset: PresetInput,
    },
    DeletePreset {
        id: String,
    },
    SelectPreset {
        #[serde(deserialize_with = "required_nullable")]
        id: Option<String>,
    },
    SetView {
        view: View,
    },
}

fn required_nullable<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

fn invalid(message: &str) -> String {
    format!("配置无效：{message}")
}
fn io_error(action: &str, error: std::io::Error) -> String {
    format!("配置{action}失败：{error}")
}
fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn validate_preset(p: &Preset) -> Result<()> {
    if !valid_id(&p.id) {
        return Err(invalid("预设 ID"));
    }
    if p.name.trim() != p.name
        || p.name.is_empty()
        || p.name.chars().count() > 64
        || p.name.len() > 256
        || p.name.chars().any(char::is_control)
    {
        return Err(invalid("名称须为 1–64 字符且无控制字符"));
    }
    if p.model.is_empty()
        || p.model.len() > 64
        || !p
            .model
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b',')
    {
        return Err(invalid("机型"));
    }
    if p.architecture.is_empty()
        || p.architecture.len() > 32
        || !p
            .architecture
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_')
    {
        return Err(invalid("架构"));
    }
    if p.fans.is_empty() || p.fans.len() > 16 {
        return Err(invalid("风扇数量须为 1–16"));
    }
    let mut ids = HashSet::new();
    for fan in &p.fans {
        let bytes = fan.fan_id.as_bytes();
        if bytes.len() != 2
            || bytes[0] != b'F'
            || !(bytes[1].is_ascii_digit() || (b'A'..=b'F').contains(&bytes[1]))
            || !ids.insert(&fan.fan_id)
        {
            return Err(invalid("风扇 ID 或重复风扇"));
        }
        match &fan.mode {
            Mode::Automatic {} => {}
            Mode::Fixed { rpm } if *rpm <= 30_000 => {}
            Mode::Sensor {
                sensor_id,
                min_rpm,
                max_rpm,
                lower_temp,
                upper_temp,
            } if !sensor_id.is_empty()
                && sensor_id.len() <= 256
                && !sensor_id.chars().any(char::is_control)
                && min_rpm < max_rpm
                && *max_rpm <= 30_000
                && lower_temp.is_finite()
                && upper_temp.is_finite()
                && *lower_temp >= 0.0
                && *upper_temp <= 127.0
                && lower_temp < upper_temp => {}
            _ => return Err(invalid("转速或传感器范围")),
        }
    }
    Ok(())
}
fn validate(doc: &Document) -> Result<()> {
    if doc.schema_version != 1 {
        return Err(invalid("不支持的 schemaVersion，原文件已保留"));
    }
    if doc.revision > MAX_REVISION || doc.presets.len() > 64 {
        return Err(invalid("修订号或预设数量超限"));
    }
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    for p in &doc.presets {
        validate_preset(p)?;
        if !ids.insert(&p.id) || !names.insert(p.name.to_lowercase()) {
            return Err(invalid("预设 ID 或名称重复"));
        }
    }
    if doc
        .editor_preset_id
        .as_ref()
        .is_some_and(|id| !ids.contains(id))
    {
        return Err(invalid("所选预设不存在"));
    }
    Ok(())
}
fn random_id() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| "无法生成安全随机预设 ID".to_string())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn apply(mut doc: Document, update: Update) -> Result<Document> {
    match update {
        Update::SavePreset { preset } => {
            if preset.name.chars().any(char::is_control) {
                return Err(invalid("名称含控制字符"));
            }
            let id = match preset.id {
                Some(id) if doc.presets.iter().any(|p| p.id == id) => id,
                Some(_) => return Err(invalid("要保存的预设不存在")),
                None => loop {
                    let id = random_id()?;
                    if !doc.presets.iter().any(|p| p.id == id) {
                        break id;
                    }
                },
            };
            let saved = Preset {
                id: id.clone(),
                name: preset.name.trim().to_string(),
                model: preset.model,
                architecture: preset.architecture,
                fans: preset.fans,
            };
            if let Some(index) = doc.presets.iter().position(|p| p.id == id) {
                doc.presets[index] = saved;
            } else {
                doc.presets.push(saved);
            }
            doc.editor_preset_id = Some(id);
        }
        Update::DeletePreset { id } => {
            let index = doc
                .presets
                .iter()
                .position(|p| p.id == id)
                .ok_or_else(|| invalid("要删除的预设不存在"))?;
            doc.presets.remove(index);
            if doc.editor_preset_id.as_ref() == Some(&id) {
                doc.editor_preset_id = None;
            }
        }
        Update::SelectPreset { id } => {
            doc.editor_preset_id = id;
        }
        Update::SetView { view } => {
            doc.view = view;
        }
    }
    validate(&doc)?;
    Ok(doc)
}

/// A path supplied only by the trusted host (Tauri app_config_dir), never IPC.
pub struct Store {
    directory: PathBuf,
}
impl Store {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }
    pub fn read(&self) -> Result<Document> {
        match open_directory(&self.directory, false)? {
            Some(dir) => read_document(&dir),
            None => Ok(Document::default()),
        }
    }
    pub fn update(&self, expected_revision: u64, update: Update) -> Result<Document> {
        self.update_with_sync(expected_revision, update, |dir| dir.sync_all())
    }
    fn update_with_sync(
        &self,
        expected_revision: u64,
        update: Update,
        sync_parent: impl FnOnce(&File) -> std::io::Result<()>,
    ) -> Result<Document> {
        // Reject out-of-range revisions before creating directories or lock files.
        if expected_revision > MAX_REVISION {
            return Err(invalid("修订号超限"));
        }
        let dir = open_directory(&self.directory, true)?.ok_or_else(|| invalid("目录不存在"))?;
        let lock = open_file(&dir, LOCK, libc::O_RDWR | libc::O_CREAT, 0o600)?
            .ok_or_else(|| invalid("锁文件不存在"))?;
        check_regular(&lock)?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io_error("加锁", std::io::Error::last_os_error()));
        }
        // File drop releases flock on every exit, including validation failures.
        let current = read_document(&dir)?;
        if current.revision != expected_revision {
            return Err("配置已被其他窗口或进程修改，请重新读取后再保存；当前草稿可保留".into());
        }
        let mut next = apply(current.clone(), update)?;
        if next == current {
            return Ok(current);
        }
        next.revision = current
            .revision
            .checked_add(1)
            .filter(|n| *n <= MAX_REVISION)
            .ok_or_else(|| invalid("修订号已耗尽"))?;
        let bytes = serde_json::to_vec_pretty(&next).map_err(|e| format!("配置序列化失败：{e}"))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("文件超过 1 MiB"));
        }
        let temporary = format!(".preferences-{}.tmp", random_id()?);
        let mut file = open_file(
            &dir,
            &temporary,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )?
        .ok_or_else(|| invalid("临时文件不存在"))?;
        let result = (|| {
            file.write_all(&bytes).map_err(|e| io_error("写入", e))?;
            file.sync_all().map_err(|e| io_error("同步文件", e))?;
            // Validate target again before replacing it. Relative operations stay on the opened directory.
            if let Some(target) = open_file(&dir, TARGET, libc::O_RDONLY, 0)? {
                check_regular(&target)?;
            }
            let old = cstr(&temporary)?;
            let new = cstr(TARGET)?;
            if unsafe {
                libc::renameat(dir.as_raw_fd(), old.as_ptr(), dir.as_raw_fd(), new.as_ptr())
            } != 0
            {
                return Err(io_error("原子替换", std::io::Error::last_os_error()));
            }
            sync_parent(&dir).map_err(|e| {
                format!("配置可能已提交，但目录同步失败；请重新读取确认后再操作：{e}")
            })?;
            Ok(next)
        })();
        // After rename this name is absent; before rename remove only our private temporary file.
        if let Ok(name) = cstr(&temporary) {
            unsafe {
                libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0);
            }
        }
        result
    }
}
fn cstr(value: &str) -> Result<CString> {
    CString::new(value).map_err(|_| invalid("路径含空字符"))
}
fn open_directory(path: &Path, create: bool) -> Result<Option<File>> {
    if !path.is_absolute() {
        return Err(invalid("配置目录必须是绝对路径"));
    }
    let root = cstr("/")?;
    let fd = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io_error("打开根目录", std::io::Error::last_os_error()));
    }
    let mut dir = unsafe { File::from_raw_fd(fd) };
    for component in path.components() {
        let name = match component {
            Component::RootDir => continue,
            Component::Normal(name) => {
                CString::new(name.as_bytes()).map_err(|_| invalid("目录名"))?
            }
            _ => return Err(invalid("配置目录含相对组件")),
        };
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let mut fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            if !create {
                return Ok(None);
            }
            let status = unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o700) };
            if status != 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(io_error("创建目录", std::io::Error::last_os_error()));
            }
            if status == 0 {
                dir.sync_all().map_err(|e| io_error("同步新目录", e))?;
            }
            fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
        }
        if fd < 0 {
            return Err(io_error(
                "打开目录（不允许符号链接）",
                std::io::Error::last_os_error(),
            ));
        }
        dir = unsafe { File::from_raw_fd(fd) };
    }
    let metadata = dir.metadata().map_err(|e| io_error("读取目录属性", e))?;
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
        return Err(invalid(
            "专用配置目录必须由当前用户拥有且不可由其他用户写入",
        ));
    }
    Ok(Some(dir))
}
fn open_file(dir: &File, name: &str, flags: i32, mode: libc::mode_t) -> Result<Option<File>> {
    let name = cstr(name)?;
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            mode as libc::c_uint,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(io_error("打开文件（不允许符号链接）", error));
    }
    Ok(Some(unsafe { File::from_raw_fd(fd) }))
}
fn check_regular(file: &File) -> Result<()> {
    let meta = file.metadata().map_err(|e| io_error("读取文件属性", e))?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.nlink() != 1 {
        return Err(invalid("配置和锁必须是当前用户的普通单链接文件"));
    }
    Ok(())
}
fn read_document(dir: &File) -> Result<Document> {
    let Some(file) = open_file(dir, TARGET, libc::O_RDONLY, 0)? else {
        return Ok(Document::default());
    };
    check_regular(&file)?;
    if file.metadata().map_err(|e| io_error("读取大小", e))?.len() > MAX_BYTES {
        return Err(invalid("文件超过 1 MiB，原文件已保留"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_error("读取", e))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("文件超过 1 MiB，原文件已保留"));
    }
    let doc: Document = serde_json::from_slice(&bytes)
        .map_err(|e| format!("配置文件无法解析，原文件已保留：{e}"))?;
    validate(&doc)?;
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::process::{Command, Stdio};

    struct Temporary(PathBuf);
    impl Temporary {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("macs-fan-prefs-test-{}", random_id().unwrap()));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
        fn store(&self) -> Store {
            Store::new(self.0.join("app"))
        }
    }
    impl Drop for Temporary {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn input(name: &str) -> PresetInput {
        PresetInput {
            id: None,
            name: name.into(),
            model: "Mac16,8".into(),
            architecture: "arm64".into(),
            fans: vec![Fan {
                fan_id: "F0".into(),
                mode: Mode::Automatic {},
            }],
        }
    }
    fn save(name: &str) -> Update {
        Update::SavePreset {
            preset: input(name),
        }
    }
    fn write_raw(tmp: &Temporary, bytes: &[u8]) {
        fs::create_dir_all(tmp.store().directory.clone()).unwrap();
        fs::write(tmp.store().directory.join(TARGET), bytes).unwrap();
    }
    #[test]
    fn missing_read_does_not_create_anything() {
        let tmp = Temporary::new();
        assert_eq!(tmp.store().read().unwrap(), Document::default());
        assert!(!tmp.store().directory.exists());
    }
    #[test]
    fn crud_restart_ids_noops_and_private_files() {
        let tmp = Temporary::new();
        let first = tmp.store().update(0, save(" 安静 ")).unwrap();
        assert_eq!(first.revision, 1);
        assert_eq!(first.presets[0].name, "安静");
        let id = first.presets[0].id.clone();
        assert!(valid_id(&id));
        assert_eq!(first.editor_preset_id, Some(id.clone()));
        assert_eq!(tmp.store().read().unwrap(), first);
        assert_eq!(
            tmp.store()
                .update(
                    1,
                    Update::SelectPreset {
                        id: Some(id.clone())
                    }
                )
                .unwrap()
                .revision,
            1
        );
        let mut edited = input("高转速");
        edited.id = Some(id.clone());
        edited.fans[0].mode = Mode::Fixed { rpm: 4000 };
        let second = tmp
            .store()
            .update(1, Update::SavePreset { preset: edited })
            .unwrap();
        assert_eq!(second.presets[0].id, id);
        assert_eq!(second.revision, 2);
        let view = View {
            unit: Unit::Fahrenheit,
            scope: Scope::All,
            ..View::default()
        };
        let third = tmp
            .store()
            .update(2, Update::SetView { view: view.clone() })
            .unwrap();
        assert_eq!(tmp.store().read().unwrap().view, view);
        assert_eq!(third.revision, 3);
        let fourth = tmp.store().update(3, Update::DeletePreset { id }).unwrap();
        assert!(fourth.presets.is_empty());
        assert_eq!(fourth.editor_preset_id, None);
        assert_eq!(
            fs::metadata(tmp.store().directory.join(TARGET))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(tmp.store().directory.join(LOCK))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(tmp.store().directory).unwrap().mode() & 0o777,
            0o700
        );
    }
    #[test]
    fn stale_conflicts_and_failed_mutations_preserve_bytes() {
        let tmp = Temporary::new();
        tmp.store().update(0, save("Work")).unwrap();
        let before = fs::read(tmp.store().directory.join(TARGET)).unwrap();
        assert!(tmp
            .store()
            .update(0, save("Other"))
            .unwrap_err()
            .contains("配置已被其他窗口或进程修改"));
        assert!(tmp.store().update(1, save("WORK")).is_err());
        assert!(tmp
            .store()
            .update(
                1,
                Update::SelectPreset {
                    id: Some("missing".into())
                }
            )
            .is_err());
        assert!(tmp
            .store()
            .update(
                1,
                Update::DeletePreset {
                    id: "missing".into()
                }
            )
            .is_err());
        let mut missing = input("Missing");
        missing.id = Some("0".repeat(32));
        assert!(tmp
            .store()
            .update(1, Update::SavePreset { preset: missing })
            .is_err());
        assert_eq!(
            fs::read(tmp.store().directory.join(TARGET)).unwrap(),
            before
        );
    }
    #[test]
    fn malformed_future_unknown_and_oversize_are_preserved() {
        let tmp = Temporary::new();
        let mut future = serde_json::to_value(Document::default()).unwrap();
        future["schemaVersion"] = 2.into();
        let mut unknown = serde_json::to_value(Document::default()).unwrap();
        unknown["extra"] = true.into();
        for bytes in [
            b"{broken".to_vec(),
            serde_json::to_vec(&future).unwrap(),
            serde_json::to_vec(&unknown).unwrap(),
            vec![b' '; MAX_BYTES as usize + 1],
        ] {
            write_raw(&tmp, &bytes);
            assert!(tmp.store().read().is_err());
            assert!(tmp.store().update(0, save("No")).is_err());
            assert_eq!(fs::read(tmp.store().directory.join(TARGET)).unwrap(), bytes);
        }
    }
    #[test]
    fn ipc_and_disk_schema_reject_missing_fields_unknown_types_and_numbers() {
        for raw in [
            r#"{"type":"setView","view":{"scope":"overview","sort":"name","unit":"celsius","precision":"decimal","extra":true}}"#,
            r#"{"type":"selectPreset"}"#,
            r#"{"type":"selectPreset","id":null,"extra":1}"#,
            r#"{"type":"applyPreset","id":null}"#,
        ] {
            assert!(serde_json::from_str::<Update>(raw).is_err(), "{raw}");
        }
        for raw in [
            r#"{"type":"automatic","rpm":2500}"#,
            r#"{"type":"fixed","rpm":-1}"#,
            r#"{"type":"fixed","rpm":2.5}"#,
        ] {
            assert!(serde_json::from_str::<Mode>(raw).is_err(), "{raw}");
        }
        let mut missing = serde_json::to_value(Document::default()).unwrap();
        missing.as_object_mut().unwrap().remove("editorPresetId");
        assert!(serde_json::from_value::<Document>(missing).is_err());
    }
    #[test]
    fn view_choices_are_strings_only_and_invalid_files_are_preserved() {
        let tmp = Temporary::new();
        for (field, choice) in [
            ("scope", "overview"),
            ("sort", "name"),
            ("unit", "celsius"),
            ("precision", "decimal"),
        ] {
            for invalid_value in [
                serde_json::json!({ (choice): null }),
                serde_json::Value::Null,
            ] {
                let mut document = serde_json::to_value(Document::default()).unwrap();
                document["view"][field] = invalid_value;
                assert!(
                    serde_json::from_value::<Document>(document.clone()).is_err(),
                    "nonstring choice accepted for {field}"
                );
                let update = serde_json::json!({"type":"setView", "view":document["view"]});
                assert!(
                    serde_json::from_value::<Update>(update).is_err(),
                    "nonstring choice accepted in update for {field}"
                );
                let bytes = serde_json::to_vec(&document).unwrap();
                write_raw(&tmp, &bytes);
                assert!(tmp.store().read().is_err());
                assert!(tmp.store().update(0, save("Must preserve")).is_err());
                assert_eq!(fs::read(tmp.store().directory.join(TARGET)).unwrap(), bytes);
            }
        }
        let view = serde_json::to_value(View::default()).unwrap();
        for field in ["scope", "sort", "unit", "precision"] {
            assert!(view[field].is_string());
            let mut null_view = view.clone();
            null_view[field] = serde_json::Value::Null;
            assert!(serde_json::from_value::<View>(null_view).is_err());
        }
    }
    #[test]
    fn bounded_portable_validation() {
        let tmp = Temporary::new();
        for name in [
            "".into(),
            " ".into(),
            "a".repeat(65),
            "a\nb".into(),
            "\nName".into(),
        ] {
            assert!(tmp.store().update(0, save(&name)).is_err());
        }
        let base = Preset {
            id: "a".repeat(32),
            name: "Good".into(),
            model: "Mac16,8".into(),
            architecture: "arm64".into(),
            fans: input("Good").fans,
        };
        let mut p = base.clone();
        p.model = "Mac/16".into();
        assert!(validate_preset(&p).is_err());
        let mut p = base.clone();
        p.architecture = "arm-64".into();
        assert!(validate_preset(&p).is_err());
        let mut p = base.clone();
        p.fans.push(p.fans[0].clone());
        assert!(validate_preset(&p).is_err());
        let mut p = base.clone();
        p.fans[0].fan_id = "F10".into();
        assert!(validate_preset(&p).is_err());
        for mode in [
            Mode::Fixed { rpm: 30001 },
            Mode::Sensor {
                sensor_id: "T".into(),
                min_rpm: 2000,
                max_rpm: 2000,
                lower_temp: 20.,
                upper_temp: 80.,
            },
            Mode::Sensor {
                sensor_id: "T".into(),
                min_rpm: 0,
                max_rpm: 30000,
                lower_temp: f64::NAN,
                upper_temp: 80.,
            },
            Mode::Sensor {
                sensor_id: "T".into(),
                min_rpm: 0,
                max_rpm: 30000,
                lower_temp: 20.,
                upper_temp: 128.,
            },
            Mode::Sensor {
                sensor_id: "T\n".into(),
                min_rpm: 0,
                max_rpm: 30000,
                lower_temp: 20.,
                upper_temp: 80.,
            },
        ] {
            let mut p = base.clone();
            p.fans[0].mode = mode;
            assert!(validate_preset(&p).is_err());
        }
        let mut p = base.clone();
        p.fans[0].mode = Mode::Sensor {
            sensor_id: "Tp01".into(),
            min_rpm: 0,
            max_rpm: 30000,
            lower_temp: 0.,
            upper_temp: 127.,
        };
        assert!(validate_preset(&p).is_ok());
        let mut doc = Document::default();
        for i in 0..64 {
            let mut p = base.clone();
            p.id = format!("{i:032x}");
            p.name = format!("Preset {i}");
            doc.presets.push(p);
        }
        assert!(validate(&doc).is_ok());
        let mut extra = base;
        extra.id = "f".repeat(32);
        doc.presets.push(extra);
        assert!(validate(&doc).is_err());
        let exhausted = Document {
            revision: MAX_REVISION,
            ..Document::default()
        };
        write_raw(&tmp, &serde_json::to_vec(&exhausted).unwrap());
        assert!(tmp.store().update(MAX_REVISION, save("Over")).is_err());
        assert_eq!(tmp.store().read().unwrap(), exhausted);
    }
    #[test]
    fn rejects_symlink_directory_target_lock_and_nonregular_files() {
        let tmp = Temporary::new();
        let outside = tmp.0.join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, tmp.store().directory).unwrap();
        assert!(tmp.store().read().is_err());
        assert!(tmp.store().update(0, save("No")).is_err());
        fs::remove_file(tmp.store().directory).unwrap();
        fs::create_dir(tmp.store().directory).unwrap();
        let sentinel = outside.join("sentinel");
        fs::write(&sentinel, b"untouched").unwrap();
        for name in [TARGET, LOCK] {
            let _ = fs::remove_file(tmp.store().directory.join(name));
            symlink(&sentinel, tmp.store().directory.join(name)).unwrap();
            assert!(tmp.store().update(0, save("No")).is_err());
            if name == TARGET {
                assert!(tmp.store().read().is_err());
            }
            assert_eq!(fs::read(&sentinel).unwrap(), b"untouched");
            fs::remove_file(tmp.store().directory.join(name)).unwrap();
        }
        fs::create_dir(tmp.store().directory.join(TARGET)).unwrap();
        assert!(tmp.store().read().is_err());
        assert!(tmp.store().update(0, save("No")).is_err());
    }
    #[test]
    fn post_rename_sync_failure_reports_ambiguity_and_read_recovers() {
        let tmp = Temporary::new();
        let error = tmp
            .store()
            .update_with_sync(0, save("Committed"), |_| {
                Err(std::io::Error::other("injected directory fsync failure"))
            })
            .unwrap_err();
        assert!(error.contains("可能已提交"));
        let recovered = tmp.store().read().unwrap();
        assert_eq!(recovered.revision, 1);
        assert_eq!(recovered.presets[0].name, "Committed");
        assert!(tmp
            .store()
            .update(0, save("Retry"))
            .unwrap_err()
            .contains("配置已被其他窗口或进程修改"));
        assert!(fs::read_dir(tmp.store().directory).unwrap().all(|f| !f
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
    }
    #[test]
    fn precommit_io_failure_preserves_original() {
        let tmp = Temporary::new();
        tmp.store().update(0, save("Original")).unwrap();
        let before = fs::read(tmp.store().directory.join(TARGET)).unwrap();
        fs::set_permissions(
            tmp.store().directory.clone(),
            fs::Permissions::from_mode(0o500),
        )
        .unwrap();
        let result = tmp.store().update(1, save("Blocked"));
        fs::set_permissions(
            tmp.store().directory.clone(),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        assert!(result.is_err());
        assert_eq!(
            fs::read(tmp.store().directory.join(TARGET)).unwrap(),
            before
        );
    }
    #[test]
    #[ignore = "launched only by cross_process_writers_do_not_lose_updates"]
    fn subprocess_writer() {
        let Some(path) = std::env::var_os("MACS_FAN_TEST_PREFERENCES_DIR") else {
            return;
        };
        if let Some(ready) = std::env::var_os("MACS_FAN_TEST_READY") {
            fs::write(ready, b"ready").unwrap();
        }
        let result = Store::new(PathBuf::from(path)).update(0, save("Child"));
        match result {
            Ok(doc) => println!("PREFERENCE_COMMITTED:{}", doc.revision),
            Err(error) if error.contains("配置已被其他窗口或进程修改") => {
                println!("PREFERENCE_CONFLICT")
            }
            Err(error) => panic!("unexpected: {error}"),
        }
    }
    #[test]
    fn flock_blocks_a_separate_process_until_release() {
        let tmp = Temporary::new();
        tmp.store().update(0, save("Parent")).unwrap();
        let dir = open_directory(&tmp.store().directory, false)
            .unwrap()
            .unwrap();
        let lock = open_file(&dir, LOCK, libc::O_RDWR, 0).unwrap().unwrap();
        assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
        let ready = tmp.0.join("ready");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "preferences::tests::subprocess_writer",
                "--ignored",
                "--nocapture",
            ])
            .env("MACS_FAN_TEST_PREFERENCES_DIR", tmp.store().directory)
            .env("MACS_FAN_TEST_READY", &ready)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !ready.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(ready.exists(), "child did not reach store update");
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            child.try_wait().unwrap().is_none(),
            "child bypassed held flock"
        );
        drop(lock);
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("PREFERENCE_CONFLICT"));
        assert_eq!(tmp.store().read().unwrap().presets[0].name, "Parent");
    }
    #[test]
    fn cross_process_writers_do_not_lose_updates() {
        let tmp = Temporary::new();
        let mut children = vec![];
        for _ in 0..2 {
            children.push(
                Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "preferences::tests::subprocess_writer",
                        "--ignored",
                        "--nocapture",
                    ])
                    .env("MACS_FAN_TEST_PREFERENCES_DIR", tmp.store().directory)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap(),
            );
        }
        let outputs: Vec<_> = children
            .into_iter()
            .map(|child| {
                let out = child.wait_with_output().unwrap();
                assert!(
                    out.status.success(),
                    "{}",
                    String::from_utf8_lossy(&out.stderr)
                );
                String::from_utf8(out.stdout).unwrap()
            })
            .collect();
        assert_eq!(
            outputs
                .iter()
                .filter(|s| s.contains("PREFERENCE_COMMITTED:1"))
                .count(),
            1
        );
        assert_eq!(
            outputs
                .iter()
                .filter(|s| s.contains("PREFERENCE_CONFLICT"))
                .count(),
            1
        );
        assert_eq!(tmp.store().read().unwrap().revision, 1);
    }
}
