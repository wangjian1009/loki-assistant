#include <napi/native_api.h>
#include <cstdint>
#include <memory>
#include <new>
#include <string>
#include <utility>

extern "C" void* loki_workspace_open(const char*, char**);
extern "C" char* loki_workspace_execute(const void*, const char*, const char*);
extern "C" void loki_workspace_free(void*);
extern "C" void loki_workspace_begin_stop(const void*);
extern "C" void loki_workspace_string_free(char*);
extern "C" char* loki_workspace_start_http(const void*, uintptr_t,
    bool (*)(uintptr_t, uint64_t, const char*, const char*), void (*)(uintptr_t));
extern "C" void loki_workspace_respond_http(const void*, uint64_t, uint16_t, const char*);

struct WorkspaceHandle {
    void* value = nullptr;
    ~WorkspaceHandle() { loki_workspace_free(value); }
};
struct HttpMonitor {
    napi_threadsafe_function function = nullptr;
    std::weak_ptr<WorkspaceHandle> workspace;
};
struct HttpCall {
    uint64_t id;
    std::string method, body;
};
struct HttpReply {
    uint64_t id;
    std::weak_ptr<WorkspaceHandle> workspace;
};
static void ReleaseHttp(uintptr_t context) {
    auto* monitor = reinterpret_cast<HttpMonitor*>(context);
    napi_release_threadsafe_function(monitor->function, napi_tsfn_abort);
}
static bool DispatchHttp(uintptr_t context, uint64_t id, const char* method, const char* body) {
    auto* monitor = reinterpret_cast<HttpMonitor*>(context);
    auto* call = new HttpCall{id, method, body};
    if (napi_call_threadsafe_function(monitor->function, call, napi_tsfn_nonblocking) == napi_ok) return true;
    delete call;
    return false;
}
struct WorkspaceWork {
    napi_async_work work = nullptr;
    napi_deferred deferred = nullptr;
    std::shared_ptr<WorkspaceHandle> workspace;
    std::string directory, operation, payload;
    char* result = nullptr;
    char* error = nullptr;
    HttpMonitor* http = nullptr;
    bool opening = false;
    ~WorkspaceWork() { if (http) ReleaseHttp(reinterpret_cast<uintptr_t>(http)); }
};
static bool ReadString(napi_env env, napi_value value, std::string& output) {
    size_t length = 0;
    if (napi_get_value_string_utf8(env, value, nullptr, 0, &length) != napi_ok || length > 1048576) return false;
    output.resize(length + 1);
    if (napi_get_value_string_utf8(env, value, output.data(), output.size(), &length) != napi_ok) return false;
    output.resize(length);
    return output.find('\0') == std::string::npos;
}
static napi_value RespondHttp(napi_env env, napi_callback_info info) {
    size_t count = 2;
    napi_value args[2];
    void* data = nullptr;
    uint32_t status = 0;
    std::string body;
    if (napi_get_cb_info(env, info, &count, args, nullptr, &data) != napi_ok || count != 2 || !data ||
        napi_get_value_uint32(env, args[0], &status) != napi_ok || status < 200 || status >= 600 ||
        !ReadString(env, args[1], body)) {
        napi_throw_type_error(env, nullptr, "调试 HTTP 响应无效");
        return nullptr;
    }
    auto* reply = static_cast<HttpReply*>(data);
    if (auto workspace = reply->workspace.lock()) {
        loki_workspace_respond_http(workspace->value, reply->id, static_cast<uint16_t>(status), body.c_str());
    }
    napi_value result;
    napi_get_undefined(env, &result);
    return result;
}
static void CallHttp(napi_env env, napi_value callback, void* context, void* data) {
    std::unique_ptr<HttpCall> call(static_cast<HttpCall*>(data));
    if (!env || !callback) return;
    auto* monitor = static_cast<HttpMonitor*>(context);
    auto reply = std::make_unique<HttpReply>(HttpReply{call->id, monitor->workspace});
    napi_value args[3], receiver;
    if (napi_create_string_utf8(env, call->method.c_str(), call->method.size(), &args[0]) != napi_ok ||
        napi_create_string_utf8(env, call->body.c_str(), call->body.size(), &args[1]) != napi_ok ||
        napi_create_function(env, "respond", NAPI_AUTO_LENGTH, RespondHttp, reply.get(), &args[2]) != napi_ok ||
        napi_add_finalizer(env, args[2], reply.get(), [](napi_env, void* value, void*) {
            delete static_cast<HttpReply*>(value);
        }, nullptr, nullptr) != napi_ok) return;
    reply.release();
    napi_get_undefined(env, &receiver);
    napi_call_function(env, receiver, callback, 3, args, nullptr);
}
static napi_value Queue(napi_env env, WorkspaceWork* work) {
    napi_value promise, name;
    if (napi_create_promise(env, &work->deferred, &promise) != napi_ok ||
        napi_create_string_utf8(env, "LokiWorkspace", NAPI_AUTO_LENGTH, &name) != napi_ok) {
        delete work; napi_throw_error(env, nullptr, "无法创建工作空间任务"); return nullptr;
    }
    auto status = napi_create_async_work(env, nullptr, name,
        [](napi_env, void* data) {
            auto* work = static_cast<WorkspaceWork*>(data);
            if (work->opening) {
                work->workspace->value = loki_workspace_open(work->directory.c_str(), &work->error);
            } else if (work->http) {
                auto* monitor = std::exchange(work->http, nullptr);
                work->result = loki_workspace_start_http(work->workspace->value,
                    reinterpret_cast<uintptr_t>(monitor), DispatchHttp, ReleaseHttp);
            } else {
                work->result = loki_workspace_execute(work->workspace->value, work->operation.c_str(), work->payload.c_str());
            }
        },
        [](napi_env env, napi_status status, void* data) {
            auto* work = static_cast<WorkspaceWork*>(data);
            napi_value value;
            if (status != napi_ok || work->error || (work->opening && !work->workspace->value)) {
                napi_value message;
                napi_create_string_utf8(env, work->error ? work->error : "工作空间任务未完成", NAPI_AUTO_LENGTH, &message);
                napi_create_error(env, nullptr, message, &value);
                napi_reject_deferred(env, work->deferred, value);
            } else if (work->opening) {
                auto* reference = new std::shared_ptr<WorkspaceHandle>(work->workspace);
                napi_create_external(env, reference, [](napi_env, void* data, void*) {
                    delete static_cast<std::shared_ptr<WorkspaceHandle>*>(data);
                }, nullptr, &value);
                napi_resolve_deferred(env, work->deferred, value);
            } else {
                napi_create_string_utf8(env, work->result ? work->result : "{\"ok\":false,\"error\":\"工作空间任务未完成\"}", NAPI_AUTO_LENGTH, &value);
                napi_resolve_deferred(env, work->deferred, value);
            }
            loki_workspace_string_free(work->result); loki_workspace_string_free(work->error);
            napi_delete_async_work(env, work->work); delete work;
        }, work, &work->work);
    if (status == napi_ok) status = napi_queue_async_work(env, work->work);
    if (status != napi_ok) {
        if (work->work) napi_delete_async_work(env, work->work);
        napi_value message, error;
        napi_create_string_utf8(env, "无法启动工作空间任务", NAPI_AUTO_LENGTH, &message);
        napi_create_error(env, nullptr, message, &error);
        napi_reject_deferred(env, work->deferred, error); delete work;
    }
    return promise;
}
static napi_value Open(napi_env env, napi_callback_info info) {
    size_t count = 1; napi_value args[1];
    auto* work = new (std::nothrow) WorkspaceWork;
    if (!work) { napi_throw_error(env, nullptr, "无法分配任务"); return nullptr; }
    if (napi_get_cb_info(env, info, &count, args, nullptr, nullptr) != napi_ok || count != 1 || !ReadString(env, args[0], work->directory)) {
        delete work; napi_throw_type_error(env, nullptr, "工作空间目录无效"); return nullptr;
    }
    work->opening = true; work->workspace = std::make_shared<WorkspaceHandle>();
    return Queue(env, work);
}
static napi_value Execute(napi_env env, napi_callback_info info) {
    size_t count = 3; napi_value args[3]; void* reference = nullptr;
    auto* work = new (std::nothrow) WorkspaceWork;
    if (!work) { napi_throw_error(env, nullptr, "无法分配任务"); return nullptr; }
    if (napi_get_cb_info(env, info, &count, args, nullptr, nullptr) != napi_ok || count != 3 ||
        napi_get_value_external(env, args[0], &reference) != napi_ok || !reference ||
        !ReadString(env, args[1], work->operation) || !ReadString(env, args[2], work->payload)) {
        delete work; napi_throw_type_error(env, nullptr, "工作空间参数无效"); return nullptr;
    }
    work->workspace = *static_cast<std::shared_ptr<WorkspaceHandle>*>(reference);
    if (work->operation == "stop") loki_workspace_begin_stop(work->workspace->value);
    return Queue(env, work);
}
static napi_value StartHttp(napi_env env, napi_callback_info info) {
    size_t count = 2;
    napi_value args[2], name;
    void* reference = nullptr;
    napi_valuetype type;
    if (napi_get_cb_info(env, info, &count, args, nullptr, nullptr) != napi_ok || count != 2 ||
        napi_get_value_external(env, args[0], &reference) != napi_ok || !reference ||
        napi_typeof(env, args[1], &type) != napi_ok || type != napi_function) {
        napi_throw_type_error(env, nullptr, "调试 HTTP 回调无效");
        return nullptr;
    }
    auto work = std::make_unique<WorkspaceWork>();
    work->workspace = *static_cast<std::shared_ptr<WorkspaceHandle>*>(reference);
    auto monitor = std::make_unique<HttpMonitor>();
    monitor->workspace = work->workspace;
    if (napi_create_string_utf8(env, "LokiHttpUi", NAPI_AUTO_LENGTH, &name) != napi_ok ||
        napi_create_threadsafe_function(env, args[1], nullptr, name, 32, 1, monitor.get(),
            [](napi_env, void* value, void*) { delete static_cast<HttpMonitor*>(value); },
            monitor.get(), CallHttp, &monitor->function) != napi_ok) {
        napi_throw_error(env, nullptr, "无法创建调试 HTTP 回调");
        return nullptr;
    }
    napi_unref_threadsafe_function(env, monitor->function);
    work->http = monitor.release();
    return Queue(env, work.release());
}
// 取消只写入信号，不占用异步工作线程，也不等待网络。
static napi_value Cancel(napi_env env, napi_callback_info info) {
    size_t count = 2; napi_value args[2]; void* reference = nullptr; std::string operation;
    if (napi_get_cb_info(env, info, &count, args, nullptr, nullptr) != napi_ok || count != 2 ||
        napi_get_value_external(env, args[0], &reference) != napi_ok || !reference || !ReadString(env, args[1], operation)) {
        napi_throw_type_error(env, nullptr, "取消请求参数无效"); return nullptr;
    }
    auto workspace = *static_cast<std::shared_ptr<WorkspaceHandle>*>(reference);
    char* result = loki_workspace_execute(workspace->value, "cancel", operation.c_str());
    loki_workspace_string_free(result);
    napi_value value; napi_get_undefined(env, &value); return value;
}
static napi_value Init(napi_env env, napi_value exports) {
    napi_property_descriptor properties[] = {
        {"open", nullptr, Open, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"execute", nullptr, Execute, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"cancel", nullptr, Cancel, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"startHttpServer", nullptr, StartHttp, nullptr, nullptr, nullptr, napi_default, nullptr}
    };
    napi_define_properties(env, exports, sizeof(properties) / sizeof(properties[0]), properties); return exports;
}
static napi_module module = {1, 0, nullptr, Init, "lokiworkspace", nullptr, {0}};
extern "C" __attribute__((constructor)) void RegisterLokiWorkspace() { napi_module_register(&module); }
