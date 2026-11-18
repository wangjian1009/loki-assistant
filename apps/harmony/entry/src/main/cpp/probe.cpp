#include <napi/native_api.h>
#include <hilog/log.h>
#include <new>

extern "C" char* loki_probe_run();
extern "C" void loki_probe_free(char*);

struct ProbeWork {
    napi_async_work work = nullptr;
    napi_deferred deferred = nullptr;
    char* result = nullptr;
};

static napi_value RunProbe(napi_env env, napi_callback_info) {
    auto* work = new (std::nothrow) ProbeWork;
    if (!work) {
        napi_throw_error(env, nullptr, "无法分配验证任务");
        return nullptr;
    }
    napi_value promise, name;
    if (napi_create_promise(env, &work->deferred, &promise) != napi_ok ||
        napi_create_string_utf8(env, "LokiIdentityProbe", NAPI_AUTO_LENGTH, &name) != napi_ok) {
        delete work;
        napi_throw_error(env, nullptr, "无法创建验证 Promise");
        return nullptr;
    }
    auto status = napi_create_async_work(env, nullptr, name,
        [](napi_env, void* data) {
            auto* work = static_cast<ProbeWork*>(data);
            work->result = loki_probe_run();
        },
        [](napi_env env, napi_status status, void* data) {
            auto* work = static_cast<ProbeWork*>(data);
            napi_value value;
            const char* result = work->result ? work->result : "{\"ok\":false,\"error\":\"原生任务未完成\"}";
            OH_LOG_Print(LOG_APP, LOG_INFO, 0xFF00, "LokiNetworkProbe", "%{public}s", result);
            napi_create_string_utf8(env, result, NAPI_AUTO_LENGTH, &value);
            if (status == napi_ok) napi_resolve_deferred(env, work->deferred, value);
            else napi_reject_deferred(env, work->deferred, value);
            loki_probe_free(work->result);
            napi_delete_async_work(env, work->work);
            delete work;
        }, work, &work->work);
    if (status == napi_ok) status = napi_queue_async_work(env, work->work);
    if (status != napi_ok) {
        if (work->work) napi_delete_async_work(env, work->work);
        napi_value error;
        napi_create_string_utf8(env, "无法启动验证任务", NAPI_AUTO_LENGTH, &error);
        napi_reject_deferred(env, work->deferred, error);
        delete work;
    }
    return promise;
}

static napi_value Init(napi_env env, napi_value exports) {
    OH_LOG_Print(LOG_APP, LOG_INFO, 0xFF00, "LokiProbeNative", "原生验证模块已加载");
    napi_property_descriptor properties[] = {
        {"runProbe", nullptr, RunProbe, nullptr, nullptr, nullptr, napi_default, nullptr}
    };
    napi_define_properties(env, exports, 1, properties);
    return exports;
}

static napi_module module = {1, 0, nullptr, Init, "lokiprobe", nullptr, {0}};
extern "C" __attribute__((constructor)) void RegisterLokiProbe() {
    napi_module_register(&module);
}
