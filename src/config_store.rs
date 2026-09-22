//! P9-3/P9-4 配置存储：config.yml 的读写序列化 + 原子落盘。
//!
//! 设计（DESIGN.md I9 精神的最小落地）：
//! - 设备增删改走"改内存 AivxYaml → 序列化写回 config.yml"单一入口，
//!   不做文本 patch（YAML 手工插行易碎；serde_yaml 全量写回才可靠）。
//! - 原子写：先写 `config.yml.tmp` 再 rename（进程中途崩溃不留半截文件）。
//! - CameraYaml 等结构 `Serialize` 后由本模块拼 AivxYaml 全量输出。
//!   auth 段原样保留（读时留着，写时带回——不在 UI 改口令，只保不丢）。
//! - 设备改动后需重启生效：线程束（T1/T2/T3）是启动期拉起的 OS 线程，
//!   运行期增删设备需 systemd 重启（UI 提示）；配置热改只含 UI 侧字段。

use std::path::Path;

use crate::cameras::{
    AivxYaml, CameraYaml, DetectClassesYaml, DetectYaml, FfmpegYaml, InputYaml, RecordYaml,
    SnapshotsYaml,
};

/// config.yml 全量写回（原子：tmp + rename）。
pub fn save(path: &Path, yaml: &AivxYaml) -> anyhow::Result<()> {
    // Serialize 顺序：serde_yaml 0.9 HashMap 无序——用 BTreeMap 排序不可行
    // （cameras 是 HashMap<String, CameraYaml>）。写前按 key 排序进
    // serde_yaml::Mapping，保证文件 diff 稳定可读。
    let mut root = serde_yaml::Mapping::new();
    if let Some(auth) = &yaml.auth {
        let mut a = serde_yaml::Mapping::new();
        a.insert(
            serde_yaml::Value::String("password_sha256".into()),
            serde_yaml::Value::String(auth.password_sha256.clone()),
        );
        root.insert(
            serde_yaml::Value::String("auth".into()),
            serde_yaml::Value::Mapping(a),
        );
    }
    let mut cams = serde_yaml::Mapping::new();
    let mut names: Vec<&String> = yaml.cameras.keys().collect();
    names.sort();
    for name in names {
        let cam = &yaml.cameras[name];
        cams.insert(
            serde_yaml::Value::String(name.clone()),
            serde_yaml::to_value(cam)?,
        );
    }
    root.insert(
        serde_yaml::Value::String("cameras".into()),
        serde_yaml::Value::Mapping(cams),
    );
    let out = serde_yaml::to_string(&serde_yaml::Value::Mapping(root))?;
    let tmp = path.with_extension("yml.tmp");
    std::fs::write(&tmp, out)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// 新设备的缺省 CameraYaml（UI 表单 → 这里构造；detect 分辨率默认 1280×720）。
pub fn new_camera(rtsp_url: &str, record_mode: &str, retain_days: u32) -> CameraYaml {
    let record = match record_mode {
        "always" => RecordYaml {
            enabled: true,
            motion: None,
        },
        "motion" => RecordYaml {
            enabled: true,
            motion: Some(crate::cameras::RecordMotionYaml {
                days: retain_days.max(1),
            }),
        },
        _ => RecordYaml::default(),
    };
    CameraYaml {
        enabled: true,
        ffmpeg: FfmpegYaml {
            inputs: vec![InputYaml {
                path: rtsp_url.to_string(),
                roles: vec!["detect".into()],
            }],
        },
        detect: DetectYaml {
            enabled: true,
            width: 1280,
            height: 720,
            model: None,
            classes: None,
        },
        record,
        snapshots: SnapshotsYaml::default(),
    }
}

/// 改录像模式（P9-4：不重启即可改的运行时字段走 CameraHandle；写回同时落 YAML）。
pub fn set_record_mode(cam: &mut CameraYaml, mode: &str, retain_days: u32) {
    cam.record = match mode {
        "always" => RecordYaml {
            enabled: true,
            motion: None,
        },
        "motion" => RecordYaml {
            enabled: true,
            motion: Some(crate::cameras::RecordMotionYaml {
                days: if retain_days > 0 { retain_days } else { 7 },
            }),
        },
        _ => RecordYaml::default(),
    };
}

/// 改检测类别（P9-2 的 UI 落地：类名列表写进 detect.classes）。
pub fn set_detect_classes(cam: &mut CameraYaml, classes: Vec<String>) {
    cam.detect.classes = if classes.is_empty() {
        None
    } else {
        Some(DetectClassesYaml { classes })
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn tmp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("aivx-cfg-{}-{}", name, std::process::id()))
    }

    /// 写回 → 重读 round-trip：设备/auth 段不丢。
    #[test]
    fn save_and_reload_roundtrip() {
        let path = tmp_path("roundtrip");
        let mut yaml = AivxYaml {
            auth: Some(crate::cameras::AuthYaml {
                password_sha256: "abc123".into(),
            }),
            ..Default::default()
        };
        yaml.cameras.insert(
            "cam-1".into(),
            new_camera("rtsp://a@1.2.3.4/stream1", "always", 0),
        );
        yaml.cameras.insert(
            "cam-2".into(),
            new_camera("rtsp://a@1.2.3.5/stream1", "motion", 7),
        );
        save(&path, &yaml).unwrap();

        let re = crate::cameras::CameraManager::parse_yaml(&path).unwrap();
        assert_eq!(re.auth.as_ref().unwrap().password_sha256, "abc123");
        assert_eq!(re.cameras.len(), 2);
        assert_eq!(re.cameras["cam-1"].record.mode(), "always");
        assert_eq!(re.cameras["cam-2"].record.mode(), "motion");
        assert_eq!(re.cameras["cam-2"].record.motion.unwrap().days, 7);
        let _ = std::fs::remove_file(&path);
    }

    /// 录像模式切换写回。
    #[test]
    fn record_mode_switch() {
        let path = tmp_path("mode");
        let mut yaml = AivxYaml::default();
        yaml.cameras
            .insert("c".into(), new_camera("rtsp://x/stream1", "always", 0));
        set_record_mode(yaml.cameras.get_mut("c").unwrap(), "motion", 3);
        save(&path, &yaml).unwrap();
        let re = crate::cameras::CameraManager::parse_yaml(&path).unwrap();
        assert_eq!(re.cameras["c"].record.mode(), "motion");
        assert_eq!(re.cameras["c"].record.motion.unwrap().days, 3);
        let _ = std::fs::remove_file(&path);
    }

    /// 类别写回 + 清空恢复 None。
    #[test]
    fn classes_write_and_clear() {
        let path = tmp_path("classes");
        let mut yaml = AivxYaml::default();
        yaml.cameras
            .insert("c".into(), new_camera("rtsp://x/stream1", "off", 0));
        set_detect_classes(
            yaml.cameras.get_mut("c").unwrap(),
            vec!["person".into(), "cat".into()],
        );
        save(&path, &yaml).unwrap();
        let re = crate::cameras::CameraManager::parse_yaml(&path).unwrap();
        assert_eq!(
            re.cameras["c"].detect.classes.as_ref().unwrap().classes,
            vec!["person".to_string(), "cat".to_string()]
        );
        set_detect_classes(yaml.cameras.get_mut("c").unwrap(), Vec::new());
        assert!(yaml.cameras["c"].detect.classes.is_none());
        let _ = std::fs::remove_file(&path);
    }

    /// 空配置写回 → 重读为空（不炸）。
    #[test]
    fn empty_config_roundtrip() {
        let path = tmp_path("empty");
        let yaml: AivxYaml = AivxYaml {
            cameras: HashMap::new(),
            auth: None,
        };
        save(&path, &yaml).unwrap();
        let re = crate::cameras::CameraManager::parse_yaml(&path).unwrap();
        assert!(re.cameras.is_empty());
        let _ = std::fs::remove_file(&path);
    }
}
