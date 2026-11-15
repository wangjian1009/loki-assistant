use std::{
    ffi::CString,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use super::model::ExecutableMetadata;

pub(super) fn find(metadata: &ExecutableMetadata, search_path: &[PathBuf]) -> Option<String> {
    validate(
        &metadata.default_program,
        search_path,
        &metadata.search_paths,
    )
    .ok()
}

pub(super) fn validate(
    value: &str,
    search_path: &[PathBuf],
    suggested_paths: &[String],
) -> Result<String, String> {
    if value.trim().is_empty() || value.contains('\0') {
        return Err("请选择可执行文件".to_owned());
    }
    if value.contains('/') || value == "." || value == ".." {
        return validate_file(Path::new(value));
    }
    let mut failure = None;
    for directory in search_path
        .iter()
        .cloned()
        .chain(suggested_paths.iter().map(PathBuf::from))
    {
        let candidate = directory.join(value);
        if candidate.symlink_metadata().is_ok() {
            match validate_file(&candidate) {
                Ok(path) => return Ok(path),
                Err(error) => failure = Some(error),
            }
        }
    }
    Err(failure.unwrap_or_else(|| format!("未找到可执行程序：{value}")))
}

fn validate_file(path: &Path) -> Result<String, String> {
    let metadata = path.metadata().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!("文件不存在：{}", path.display())
        } else {
            format!("无法检查文件 {}：{error}", path.display())
        }
    })?;
    if !metadata.is_file() {
        return Err(format!("请选择普通文件：{}", path.display()));
    }
    if !is_executable(path) {
        return Err(format!("文件没有执行权限：{}", path.display()));
    }
    std::path::absolute(path)
        .map_err(|error| error.to_string())?
        .into_os_string()
        .into_string()
        .map_err(|_| "程序路径不是有效的 UTF-8".to_owned())
}

fn is_executable(path: &Path) -> bool {
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // access 按服务进程的用户和组检查权限，不运行被查找的程序。
    unsafe { libc::access(path.as_ptr(), libc::X_OK) == 0 }
}
