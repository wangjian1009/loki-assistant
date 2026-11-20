#include <napi/native_api.h>
#include <memory>
#include <new>
#include <string>

extern "C" void* loki_workspace_open(const char*, char**);
extern "C" char* loki_workspace_execute(const void*, const char*, const char*);
extern "C" void loki_workspace_free(void*);
extern "C" void loki_workspace_begin_stop(const void*);
extern "C" void loki_workspace_string_free(char*);

struct WorkspaceHandle {
    void* value = nullptr;
    ~WorkspaceHandle() { loki_workspace_free(value); }
};
struct WorkspaceWork {
    napi_async_work work = nullptr;
    napi_deferred deferred = nullptr;
    std::shared_ptr<WorkspaceHandle> workspace;
    std::string directory, operation, payload;
    char* result = nullptr;
    char* error = nullptr;
    bool opening = false;
};
static bool ReadString(napi_env env, napi_value value, std::string& output) {
    size_t length = 0;
    if (napi_get_value_string_utf8(env, value, nullptr, 0, &length) != napi_ok || length > 1048576) return false;
    output.resize(length + 1);
    if (napi_get_value_string_utf8(env, value, output.data(), output.size(), &length) != napi_ok) return false;
    output.resize(length);
    return output.find('\0') == std::string::npos;
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
        {"cancel", nullptr, Cancel, nullptr, nullptr, nullptr, napi_default, nullptr}
    };
    napi_define_properties(env, exports, 3, properties); return exports;
}
static napi_module module = {1, 0, nullptr, Init, "lokiworkspace", nullptr, {0}};
extern "C" __attribute__((constructor)) void RegisterLokiWorkspace() { napi_module_register(&module); }
