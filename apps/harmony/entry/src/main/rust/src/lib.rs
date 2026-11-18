use std::ffi::{CString, c_char};

#[allow(dead_code)]
mod probe {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../../../../services/src/modules/network_probe/probe.rs"
    ));
}

// 由 NAPI 的工作线程调用；返回值只能由 loki_probe_free 释放。
#[unsafe(no_mangle)]
pub extern "C" fn loki_probe_run() -> *mut c_char {
    let result = std::panic::catch_unwind(|| {
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(probe::run_mobile())
    });
    let output = match result {
        Ok(Ok(report)) => report,
        Ok(Err(error)) => serde_json::json!({"ok":false,"error":format!("{error:#}")}).to_string(),
        Err(_) => serde_json::json!({"ok":false,"error":"原生验证发生 panic"}).to_string(),
    };
    CString::new(output).expect("JSON 不含零字节").into_raw()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_probe_free(value: *mut c_char) {
    if !value.is_null() {
        drop(unsafe { CString::from_raw(value) });
    }
}
