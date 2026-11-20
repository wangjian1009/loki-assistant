pub mod error;
pub mod network;
pub mod pairing;
mod storage;
mod workspace;
pub use workspace::{Workspace, WorkspaceOptions};

use serde_json::json;
use std::ffi::{CStr, CString, c_char};

/// # Safety
/// directory 必须指向调用期间有效的 UTF-8 C 字符串，error 必须是可写指针。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_workspace_open(
    directory: *const c_char,
    error: *mut *mut c_char,
) -> *mut Workspace {
    if error.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        *error = std::ptr::null_mut();
    }
    let result = std::panic::catch_unwind(|| {
        if directory.is_null() {
            return Err("工作空间目录无效".into());
        }
        let directory = unsafe { CStr::from_ptr(directory) }
            .to_str()
            .map_err(|_| "工作空间目录无效")?;
        Workspace::open(WorkspaceOptions::new(directory))
    })
    .unwrap_or_else(|_| Err("工作空间初始化失败".into()));
    match result {
        Ok(workspace) => Box::into_raw(Box::new(workspace)),
        Err(message) => {
            unsafe {
                *error = CString::new(message).unwrap().into_raw();
            }
            std::ptr::null_mut()
        }
    }
}
/// # Safety
/// workspace 必须是本库返回且未释放的对象，其余参数为有效 UTF-8 C 字符串。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_workspace_execute(
    workspace: *const Workspace,
    operation: *const c_char,
    payload: *const c_char,
) -> *mut c_char {
    let value = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if workspace.is_null() || operation.is_null() || payload.is_null() {
            return json!({"ok":false,"error":"工作空间参数无效"});
        }
        match unsafe {
            (
                CStr::from_ptr(operation).to_str(),
                CStr::from_ptr(payload).to_str(),
            )
        } {
            (Ok(operation), Ok(payload)) => unsafe { &*workspace }.execute(operation, payload),
            _ => json!({"ok":false,"error":"工作空间参数无效"}),
        }
    }))
    .unwrap_or_else(|_| json!({"ok":false,"error":"工作空间任务未完成"}));
    CString::new(value.to_string()).unwrap().into_raw()
}
/// # Safety
/// workspace 必须为本库创建且尚未释放的对象。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_workspace_begin_stop(workspace: *const Workspace) {
    if !workspace.is_null() {
        unsafe { &*workspace }.begin_stop();
    }
}
/// # Safety
/// 仅接受本库返回且未释放的对象；不得与其他调用并发释放。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_workspace_free(workspace: *mut Workspace) {
    if !workspace.is_null() {
        drop(unsafe { Box::from_raw(workspace) });
    }
}
/// # Safety
/// 仅接受本库返回且未释放的字符串，或空指针。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_workspace_string_free(value: *mut c_char) {
    if !value.is_null() {
        drop(unsafe { CString::from_raw(value) });
    }
}

#[cfg(test)]
mod tests;
