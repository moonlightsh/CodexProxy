//! 文件工具：所有落盘写入统一走“临时文件 + 原子替换”（设计 §7）。

use std::io::Write;
use std::path::Path;

/// 原子写入：在目标同目录创建临时文件，写入并落盘后替换目标。
///
/// - 目标所在目录不存在时自动创建。
/// - 目标已存在时保留其权限。
/// - Windows 上目标可能被其他进程短暂占用（如 Codex 正在读取），替换失败时有限重试。
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(parent)?;

    let permissions = match std::fs::metadata(path) {
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };

    let mut temp = tempfile::Builder::new()
        .prefix(".codex-helper-")
        .suffix(".tmp")
        .tempfile_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    if let Some(permissions) = permissions {
        temp.as_file().set_permissions(permissions)?;
    }

    let mut temp = temp;
    let mut attempt = 0;
    loop {
        match temp.persist(path) {
            Ok(_) => return Ok(()),
            Err(error) => {
                attempt += 1;
                let retryable = error.error.kind() == std::io::ErrorKind::PermissionDenied;
                if !retryable || attempt >= 5 {
                    return Err(error.error);
                }
                temp = error.file;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

/// 读取文本文件；不存在返回 `Ok(None)`。
pub fn read_optional(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// 删除文件；不存在视为成功。返回是否真的删除了文件。
pub fn remove_file_if_exists(path: &Path) -> std::io::Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_creates_parent_and_replaces_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("file.txt");
        atomic_write(&path, b"first").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        atomic_write(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
    }

    #[test]
    fn atomic_write_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        atomic_write(&path, b"a = 1\n").unwrap();
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["config.toml".to_string()]);
    }

    #[test]
    fn read_optional_and_remove_tolerate_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing");
        assert_eq!(read_optional(&path).unwrap(), None);
        assert!(!remove_file_if_exists(&path).unwrap());
        std::fs::write(&path, "x").unwrap();
        assert_eq!(read_optional(&path).unwrap().as_deref(), Some("x"));
        assert!(remove_file_if_exists(&path).unwrap());
    }
}
