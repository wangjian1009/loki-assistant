pub mod error;
mod http_server;
pub mod network;
pub mod pairing;
mod storage;
mod workspace;
pub use workspace::{Workspace, WorkspaceOptions};

use serde_json::json;
use std::ffi::{CStr, CString, c_char};

struct HttpCallback {
    context: usize,
    dispatch: extern "C" fn(usize, u64, *const c_char, *const c_char) -> bool,
    release: extern "C" fn(usize),
}

impl HttpCallback {
    fn send(&self, id: u64, method: &str, body: &str) -> bool {
        let (Ok(method), Ok(body)) = (CString::new(method), CString::new(body)) else {
            return false;
        };
        (self.dispatch)(self.context, id, method.as_ptr(), body.as_ptr())
    }
}

impl Drop for HttpCallback {
    fn drop(&mut self) {
        (self.release)(self.context);
    }
}

/// # Safety
/// workspace 必须有效。context 从调用开始交给 Rust，由 release 释放一次。
/// dispatch 可从 Tokio 工作线程调用，字符串指针只在当次调用期间有效。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_workspace_start_http(
    workspace: *const Workspace,
    context: usize,
    dispatch: extern "C" fn(usize, u64, *const c_char, *const c_char) -> bool,
    release: extern "C" fn(usize),
) -> *mut c_char {
    let callback = HttpCallback {
        context,
        dispatch,
        release,
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if workspace.is_null() {
            return Err("工作空间无效".to_owned());
        }
        unsafe { &*workspace }.start_http_server(8787, move |id, method, body| {
            callback.send(id, method, body)
        })
    }))
    .unwrap_or_else(|_| Err("无法启动调试 HTTP 服务".to_owned()));
    let response = match result {
        Ok(_) => json!({"ok": true}),
        Err(error) => json!({"ok": false, "error": error}),
    };
    CString::new(response.to_string()).unwrap().into_raw()
}

/// # Safety
/// workspace 必须有效，body 必须为有效的 UTF-8 C 字符串。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_workspace_respond_http(
    workspace: *const Workspace,
    id: u64,
    status: u16,
    body: *const c_char,
) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if !workspace.is_null()
            && !body.is_null()
            && let Ok(body) = unsafe { CStr::from_ptr(body) }.to_str()
        {
            unsafe { &*workspace }.respond_http(id, status, body);
        }
    }));
}

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
