// 在 Node 中执行真实 ArkTS 应用与适配代码，以原生边界替身验证持久化和回调行为。
const assert = require('node:assert/strict');
const { test } = require('node:test');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require(path.join(process.argv[2], 'tools/hvigor/hvigor-ohos-plugin/node_modules/typescript'));
const etsRoot = path.resolve(__dirname, '../entry/src/main/ets');
const clone = value => JSON.parse(JSON.stringify(value));
const id = number => number.toString(16).padStart(64, '0');
const credential = { token: 'b'.repeat(64), bound_at: 123 };
const error = (kind, reason, extra = {}) => ({ kind, reason, message: reason, request_id: 'request', outcome: 'not_sent', ...extra });
const invitation = number => ({ version: 1, pairing_id: '12345678-1234-1234-1234-123456789abc', endpoint_id: id(number), relay_url: 'https://relay.example/', relay_token: 'relay-secret', alpn: 'loki-assistant/pair/1', pairing_token: 'a'.repeat(64), expires_at: Math.floor(Date.now() / 1000) + 300 });

function fixture(initial = {}) {
  const control = { saved: clone(initial), calls: [], bindingError: undefined, holdBinding: false, bindingStarts: [], finishBinding: undefined, requestError: undefined, writes: 0, failSave: false };
  function publishStatus(handle) {
    const service = handle.service;
    let failure;
    if (service?.binding.status === 'bound' && control.requestError) {
      failure = control.requestError;
      if (failure.kind === 'not_bound') service.binding = { status: 'failed', error: clone(failure) };
    }
    const status = service?.binding.status === 'bound' && !failure
      ? control.statusSnapshot ?? { service: { name: '工作电脑', device_type: 'computer' } }
      : null;
    const snapshot = { generation: handle.generation, endpoint_id: service?.endpoint_id ?? null,
      checked_at: status || failure ? 123 : 0, error: failure ?? null, status,
      service: status?.service ?? null };
    for (const watch of handle.watchers) {
      if (watch.scope === 'device') watch.send(clone(snapshot));
      if (watch.scope === 'mail') watch.send(clone({ generation: snapshot.generation,
        endpoint_id: snapshot.endpoint_id, status: status?.mail ?? null, activity: handle.mailActivity ?? null }));
      if (watch.scope === 'slack') watch.send(clone({ generation: snapshot.generation, status: status?.slack ?? null }));
    }
  }
  function watchStatus(handle, scope, send) {
    const watch = { scope, send };
    handle.watchers.add(watch);
    publishStatus(handle);
    return { cancel() { handle.watchers.delete(watch); } };
  }
  function invoke(handle, method, value) {
    const result = JSON.parse(native.execute(handle, method, value === undefined ? '' :
      typeof value === 'string' ? value : JSON.stringify(value)));
    if (!result.ok) throw result.respons_error ?? new Error(result.error);
    return result;
  }
  const native = {
    Workspace: {
      async open() {
        const handle = { service: null, stopped: false, cancelled: new Set(), generation: 0, watchers: new Set() };
        let sequence = 0;
        const network = {
          request(request, options, success, failure) {
            const operationId = String(++sequence);
            queueMicrotask(() => {
              try { success(invoke(handle, 'request', { ...request, ...options, operation_id: operationId })); }
              catch (error) { failure(error); }
            });
            return { cancel() { handle.cancelled.add(operationId); } };
          }
        };
        return {
          setService(service) { invoke(handle, 'set_service', service); },
          clearService() { invoke(handle, 'clear_service'); },
          service() { return invoke(handle, 'service').service; },
          parseInvitation(payload) { return invoke(handle, 'parse_invitation', payload).invitation; },
          network() { return network; },
          loadState() { return invoke(handle, 'load_state'); },
          async saveState(state) { invoke(handle, 'save_state', state); },
          watchStatus(send) { return watchStatus(handle, 'device', send); },
          openMail() {
            return {
              watchStatus(send) { return watchStatus(handle, 'mail', send); },
              startSync(endpoint) { invoke(handle, 'start_mail_sync', endpoint); }
            };
          },
          openSlack() { return { watchStatus(send) { return watchStatus(handle, 'slack', send); } }; },
          async shutdown() { invoke(handle, 'stop'); }
        };
      }
    },
    execute(handle, operation, payload) {
      control.calls.push({ operation, payload });
      const ok = data => JSON.stringify({ ok: true, ...data });
      const fail = respons_error => JSON.stringify({ ok: false, respons_error });
      if (operation === 'stop') { handle.stopped = true; return ok({}); }
      if (handle.stopped) { return fail(error('cancelled', 'network_closed')); }
      if (operation === 'load_state') { return ok({ state: clone(control.saved), endpoint_id: id(999) }); }
      if (operation === 'save_state') {
        if (control.failSave) { return JSON.stringify({ ok: false, error: '磁盘写入失败' }); }
        control.saved = JSON.parse(payload); control.writes++; return ok({});
      }
      if (operation === 'set_service') {
        const service = JSON.parse(payload);
        handle.generation++; service.generation = handle.generation; handle.service = service;
        handle.mailActivity = undefined;
        if (['unbound', 'binding'].includes(service.binding.status)) {
          service.binding = { status: 'binding' };
          const options = service.auto_binding;
          control.bindingStarts.push(options);
          const finish = () => {
            if (handle.service !== service || handle.stopped) return;
            const failure = control.bindingError || (!options ? error('invalid_request', 'invalid_invitation') : undefined);
            service.binding = failure ? { status: 'failed', error: clone(failure) } : { status: 'bound', credential: clone(credential) };
            delete service.auto_binding;
            publishStatus(handle);
          };
          control.finishBinding = finish;
          if (!control.holdBinding) queueMicrotask(finish);
        }
        publishStatus(handle);
        return ok({});
      }
      if (operation === 'clear_service') { handle.service = null; handle.generation++; publishStatus(handle); return ok({}); }
      if (operation === 'refresh_status') { publishStatus(handle); return ok({}); }
      if (operation === 'start_mail_sync') {
        handle.mailActivity = { receiving: true, error: null, sync: {
          id: '12345678-1234-1234-1234-123456789abc', phase: 'indexing', messages: ['更新索引'], error: null
        } };
        publishStatus(handle);
        return ok({});
      }
      if (operation === 'service') { return ok({ service: clone(handle.service) }); }
      if (operation === 'parse_invitation') { return ok({ invitation: JSON.parse(payload) }); }
      if (operation === 'cancel') { handle.cancelled.add(payload); return ok({}); }
      if (operation === 'request') {
        const request = JSON.parse(payload);
        if (handle.cancelled.has(request.operation_id)) { return fail(error('cancelled', 'user_requested')); }
        if (!handle.service) { return fail(error('not_configured', 'service_not_set')); }
        if (handle.service.binding.status !== 'bound') { return fail(error('not_bound', 'binding_required')); }
        if (control.requestError) {
          if (control.requestError.kind === 'not_bound') { handle.service.binding = { status: 'failed', error: clone(control.requestError) }; }
          return fail(control.requestError);
        }
        if (request.method === 'POST' && request.path === '/mail/sync') {
          return ok({ request_id: 'request', status: 202, generation: 1, body: { api_version: '1', id: '12345678-1234-1234-1234-123456789abc' } });
        }
        assert.equal(request.method, 'GET');
        assert.equal(request.path, '/status');
        return ok({ request_id: 'request', status: 200, generation: 1, body: {
          api_version: '1', status: control.statusSnapshot ?? { service: { name: '工作电脑', device_type: 'computer' },
            slack: { unread_previews: [] }, mail: { unread_count: 1, unread_subjects: ['测试邮件'], sync_runs: [] } }
        } });
      }
      assert.fail(`未知原生操作 ${operation}`);
    }
  };
  const cache = new Map();
  function load(filename) {
    if (cache.has(filename)) return cache.get(filename).exports;
    const module = { exports: {} }; cache.set(filename, module);
    const code = ts.transpileModule(fs.readFileSync(filename, 'utf8'), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020, esModuleInterop: true } }).outputText;
    const localRequire = name => name === 'liblokiworkspace.so' ? native : load(path.resolve(path.dirname(filename), `${name}.ets`));
    vm.runInThisContext(`(function(require,module,exports){${code}\n})`, { filename })(localRequire, module, module.exports);
    return module.exports;
  }
  const { Workspace } = load(path.join(etsRoot, 'workspace/Workspace.ets'));
  const { Application } = load(path.join(etsRoot, 'app/Application.ets'));
  return { control, load, create() { const workspace = new Workspace('/private'); return { workspace, app: new Application(workspace) }; } };
}
function request(network) {
  return new Promise(resolve => network.request({ method: 'GET', path: '/network/session', query: [] }, { timeout_ms: 10000 }, () => assert.fail('请求不得成功'), resolve));
}

test('会话展示保留已收到的问答，按消息 ID 合并正文续片', () => {
  const { load } = fixture();
  const { conversationRows } = load(path.join(__dirname, '../../agent-viewer/src/main/ets/ConversationLayout.ets'));
  const block = (id, kind, text = id, parentId = '') => ({ id, kind, title: '', text, parentId, collapsed: null });
  const question = block('问题', 'user');
  const continuation = block('续片', 'continuation', '的续片', question.id);
  const reply = block('回复', 'assistant');
  const followUp = block('追问', 'user');
  const rows = conversationRows([question, continuation, reply, followUp], new Map());
  assert.deepEqual(rows.map(row => row.id), [question.id, reply.id, followUp.id]);
  assert.equal(rows[0].text, '问题的续片');
});

async function observedDevices(app) {
  let subscription;
  try {
    return await new Promise((resolve, reject) => {
      app.watchDevices(resolve, message => reject(new Error(message)))
        .then(value => { subscription = value; }, reject);
    });
  } finally {
    // 监听首帧可能在桥接返回句柄之前投递。
    if (subscription) subscription.cancel();
    else await new Promise(resolve => setImmediate(() => { subscription?.cancel(); resolve(); }));
  }
}

test('冷启动只加载末级页面，多级返回按需加载父页并复用原生导航栈', async () => {
  const { load } = fixture();
  const { NavigatorManager, NavigationResult, PageName, launchPath, parseNavigationPath } =
    load(path.join(etsRoot, 'app/NavigatorManager.ets'));
  const navigator = new NavigatorManager();
  const operations = [];
  const router = {
    pushUrl: async ({ url }) => { operations.push(['push', url]); },
    replaceUrl: async ({ url }) => { operations.push(['replace', url]); },
    back: () => { operations.push(['back']); }
  };
  const stage = {
    loadContent: async url => { operations.push(['load', url]); },
    getMainWindowSync: () => ({ getUIContext: () => ({ getRouter: () => router }) })
  };
  assert.equal(launchPath({ parameters: { page: 'Index,Unknown' } }, false), undefined);
  assert.equal(launchPath({ parameters: { page: 'Index,' } }, false), undefined);
  const debugWant = { parameters: { httpApiEnabled: true, httpApiPage: 'Index,Settings' } };
  const settingsPath = { pages: [{ url: PageName.Index }, { url: PageName.Settings }] };
  assert.equal(launchPath(debugWant, false), undefined);
  assert.deepEqual(launchPath(debugWant, true), settingsPath);
  navigator.openFromWant(launchPath({ parameters: { page: 'Index,Settings,Index' } }, false));
  await navigator.attach(stage);
  assert.deepEqual(operations, [['load', 'pages/Index']]);
  assert.deepEqual(navigator.snapshot(), { pages: [...settingsPath.pages, { url: PageName.Index }] });

  const home = parseNavigationPath('{"pages":[{"url":"Index"}]}');
  assert.equal(parseNavigationPath('{"pages":[{"url":"pages/Index"}]}'), undefined);
  assert.equal(navigator.canGoBack(), true);
  assert.equal(await navigator.back(), NavigationResult.Ok);
  assert.deepEqual(navigator.snapshot(), settingsPath);
  const stopWatching = navigator.watchLeaveGuard(() => false);
  assert.equal(await navigator.setPath(home), NavigationResult.Busy);
  stopWatching();
  assert.equal(await navigator.back(), NavigationResult.Ok);
  assert.equal(navigator.canGoBack(), false);
  assert.equal(await navigator.openPage(PageName.Settings), NavigationResult.Ok);
  assert.equal(await navigator.back(), NavigationResult.Ok);
  assert.deepEqual(operations, [
    ['load', 'pages/Index'], ['replace', 'pages/Settings'], ['replace', 'pages/Index'],
    ['push', 'pages/Settings'], ['back']
  ]);
  assert.deepEqual(navigator.snapshot(), home);

  const replacement = parseNavigationPath('{"pages":[{"url":"Settings"},{"url":"Index"},{"url":"Settings"}]}');
  assert.equal(await navigator.setPath(replacement), NavigationResult.Ok);
  assert.equal(await navigator.setPath(settingsPath), NavigationResult.Ok);
  assert.deepEqual(navigator.snapshot(), settingsPath);
  assert.deepEqual(operations.slice(5), [['replace', 'pages/Settings']]);

  const devicesPath = { pages: [{ url: PageName.Index }, { url: PageName.Devices }] };
  assert.deepEqual(launchPath({ parameters: { page: 'Index,Devices' } }, false), devicesPath);
  assert.deepEqual(parseNavigationPath('{"pages":[{"url":"Index"},{"url":"Devices"}]}'), devicesPath);
  assert.equal(await navigator.openSection(PageName.Devices), NavigationResult.Ok);
  assert.deepEqual(navigator.snapshot(), devicesPath);
  assert.equal(await navigator.back(), NavigationResult.Ok);
  assert.deepEqual(navigator.snapshot(), home);
  assert.deepEqual(operations.slice(6), [['replace', 'pages/Devices'], ['replace', 'pages/Index']]);
});

test('面板和应用布局按注册清单迁移并保留隐藏项目的位置', () => {
  const { load } = fixture();
  const { moveVisibleItem, readItemLayout, readLegacyLauncherLayout, readLegacyPanelLayout } =
    load(path.join(etsRoot, 'app/CurrentDeviceLayout.ets'));
  const panels = ['launcher', 'device', 'mail', 'slack', 'new-panel'];
  const apps = ['health', 'coffee', 'agent-viewer', 'new-app'];
  const legacy = JSON.stringify({
    panelOrder: ['slack', 'launcher', 'device', 'mail'],
    hiddenPanels: ['device', 'device', 'obsolete'],
    appOrder: ['coffee', 'agent-viewer', 'health'],
    hiddenApps: ['agent-viewer', 'obsolete']
  });
  const oldPanelOrder = readLegacyPanelLayout(panels, '', 'slack,refresh,mail');
  assert.deepEqual(oldPanelOrder.order, ['launcher', 'device', 'slack', 'mail', 'new-panel']);
  const panelLayout = readLegacyPanelLayout(panels, legacy, '');
  assert.deepEqual(panelLayout.hidden, ['device']);
  assert.deepEqual(panelLayout.order, ['slack', 'launcher', 'device', 'mail', 'new-panel']);
  const appLayout = readLegacyLauncherLayout(apps, legacy);
  assert.deepEqual(appLayout.hidden, ['agent-viewer']);
  assert.deepEqual(appLayout.order, ['coffee', 'agent-viewer', 'health', 'new-app']);
  const moved = moveVisibleItem(appLayout.order, appLayout.hidden, 'health', 'coffee');
  assert.deepEqual(moved, ['health', 'agent-viewer', 'coffee', 'new-app']);
  assert.deepEqual(readItemLayout(apps, JSON.stringify({ order: moved, hidden: [] }), appLayout),
    { order: moved, hidden: [] });
});

test('应用保存多台电脑，Network 只恢复选中的一个目标', async () => {
  const fixtureState = fixture(); let { app, workspace } = fixtureState.create();
  await app.addService(JSON.stringify(invitation(1))); await observedDevices(app);
  await app.addService(JSON.stringify(invitation(2))); await observedDevices(app);
  let views = await app.list(); assert.equal(views.length, 2); assert.equal(views[0].active, false); assert.equal(views[1].active, true);
  assert.equal((await (await workspace.open()).service()).endpoint_id, id(2));
  assert.equal(fixtureState.control.saved.computers[1].binding.credential.token, credential.token);
  assert.equal(fixtureState.control.saved.computers[1].name, '工作电脑');
  assert.equal(fixtureState.control.saved.computers[1].device_type, 'computer');
  assert.ok(!JSON.stringify(views).includes('relay-secret')); assert.ok(!JSON.stringify(views).includes(credential.token));
  await app.stop(); ({ app, workspace } = fixtureState.create());
  assert.equal(await (await workspace.open()).service(), null);
  views = await app.list(); assert.equal(views[1].status, 'offline');
  assert.equal((await (await workspace.open()).service()).binding.credential.token, credential.token);
  assert.equal((await observedDevices(app))[1].status, 'online');
  await app.select(id(1)); assert.equal((await (await workspace.open()).service()).endpoint_id, id(1));
});

test('一次获取全部开放状态，手动同步复用托盘路径', async () => {
  const { control, create } = fixture();
  const { app, workspace } = create();
  const taskId = '12345678-1234-1234-1234-123456789abc';
  control.statusSnapshot = {
    service: { name: '工作电脑', device_type: 'computer', start_at: 1795744238 },
    network: { my: { status: 'connected' } },
    mail: { unread_count: 0, unread_subjects: [], sync_runs: [{ id: taskId, phase: 'completed' }] },
    slack: { unread_previews: [{ id: 'T1.C1', title: '讨论', team_name: '团队', unread_count: 2, preview: '测试消息' }] },
    example: { available: false, nested: { values: [0, null, '状态'] } }
  };
  await app.addService(JSON.stringify(invitation(1)));
  const [computer] = await observedDevices(app);
  assert.deepEqual(computer.dashboard.status, control.statusSnapshot);
  const progress = [];
  const slackUpdates = [];
  const nativeWorkspace = await workspace.open();
  const mail = nativeWorkspace.openMail();
  const subscription = mail.watchStatus(update => {
    if (update.activity?.sync) progress.push(update.activity.sync);
  });
  const slackSubscription = nativeWorkspace.openSlack().watchStatus(update => slackUpdates.push(update.status));
  mail.startSync(id(1));
  subscription.cancel();
  slackSubscription.cancel();
  assert.equal(slackUpdates[0].unread_previews[0].id, 'T1.C1');
  assert.equal(progress.length, 1);
  assert.equal(progress[0].id, taskId);
  assert.equal(progress[0].phase, 'indexing');
  assert.deepEqual(progress[0].messages, ['更新索引']);
  assert.ok(control.calls.some(call => call.operation === 'start_mail_sync'));
  assert.ok(!control.calls.some(call => call.operation === 'request'));
});

test('未绑定配置自动开始绑定，界面只读取 Service 状态并保存凭据', async () => {
  const fixtureState = fixture(); const { app, workspace } = fixtureState.create();
  fixtureState.control.holdBinding = true;
  assert.equal(typeof (await workspace.network()).bind, 'undefined');
  assert.equal(typeof app.bind, 'undefined');
  await app.addService(JSON.stringify(invitation(1)));
  let views = await observedDevices(app); assert.equal(views[0].status, 'binding');
  assert.ok(!fixtureState.control.calls.some(call => call.operation === 'request'));
  fixtureState.control.finishBinding();
  views = await observedDevices(app); assert.equal(views[0].status, 'online');
  assert.equal(fixtureState.control.saved.computers[0].binding.credential.token, credential.token);
  assert.equal(fixtureState.control.saved.computers[0].auto_binding, undefined);
  assert.equal(fixtureState.control.bindingStarts.length, 1);
});

test('绑定失败记录在 Service，提示删除后重新扫码，不保留待确认或自动重试', async () => {
  const fixtureState = fixture(); const { app, workspace } = fixtureState.create();
  fixtureState.control.bindingError = error('services', 'rejected', { status: 410, source: 'business', code: 'pairing-expired', message: '二维码已过期' });
  await app.addService(JSON.stringify(invitation(1)));
  let views = await observedDevices(app); assert.equal(views[0].status, 'binding_failed');
  assert.match(views[0].detail, /二维码已过期.*删除.*重新扫码绑定/);
  assert.equal(views[0].binding_error.code, 'pairing-expired');
  assert.equal(fixtureState.control.saved.computers[0].binding.status, 'failed');
  assert.equal(fixtureState.control.saved.computers[0].auto_binding, undefined);
  assert.equal('pending' in fixtureState.control.saved.computers[0], false);
  assert.ok(!JSON.stringify(views).includes(credential.token));
  await observedDevices(app); assert.equal(fixtureState.control.bindingStarts.length, 1);
  await assert.rejects(app.addService(JSON.stringify(invitation(1))), /删除后重新扫码添加/);
  await app.remove(id(1)); assert.deepEqual(await app.list(), []);
  assert.equal(await (await workspace.open()).service(), null);
});

test('未选择的旧多设备记录不自动连接，损坏凭据不覆盖', async () => {
  const record = number => ({ endpoint_id: id(number), relay_url: 'https://relay.example/', relay_token: 'secret', bound_at: 1, pending: null });
  let fixtureState = fixture({ computers: [record(1), record(2)] }); let { app, workspace } = fixtureState.create();
  assert.ok((await app.list()).every(view => view.status === 'offline'));
  assert.equal(await (await workspace.open()).service(), null); assert.equal(fixtureState.control.writes, 0);
  fixtureState = fixture({ computers: [record(1)], selected_endpoint_id: null });
  ({ app, workspace } = fixtureState.create());
  assert.equal((await app.list())[0].active, false);
  assert.equal(await (await workspace.open()).service(), null);
  fixtureState = fixture({ computers: [record(1)] });
  ({ app } = fixtureState.create());
  assert.equal((await app.list())[0].active, true);
  const invalid = record(1); invalid.binding = { status: 'bound' };
  fixtureState = fixture({ computers: [invalid] }); ({ app } = fixtureState.create());
  await assert.rejects(app.list(), /绑定凭据已损坏/); assert.equal(fixtureState.control.writes, 0);
});

test('名称只在手机修改，后续绑定上报本机名称', async () => {
  const fixtureState = fixture(); const { app } = fixtureState.create();
  await app.setDeviceName('我的移动端');
  assert.equal(await app.deviceName(), '我的移动端');
  assert.ok(!fixtureState.control.calls.some(call => ['request', 'bind'].includes(call.operation)));
  await app.addService(JSON.stringify(invitation(1)));
  assert.equal(fixtureState.control.bindingStarts[0].device_name, '我的移动端');
});

test('request 异步交付 Union，不包装成 Error，并支持取消', async () => {
  const fixtureState = fixture(); const { workspace } = fixtureState.create(); let asynchronous = false;
  const nativeWorkspace = await workspace.open();
  const network = nativeWorkspace.network();
  const pending = new Promise(resolve => network.request({ method: 'GET', path: '/', query: [] }, { timeout_ms: 10000 }, () => assert.fail(), error => { assert.equal(asynchronous, true); resolve(error); }));
  asynchronous = true; assert.equal((await pending).kind, 'not_configured');
  const service = { endpoint_id: id(1), relay_url: 'https://relay.example/', relay_token: 'secret', binding: { status: 'unbound' } };
  nativeWorkspace.setService(service); assert.equal((await request(await workspace.network())).kind, 'not_bound');
  service.binding = { status: 'bound', credential }; nativeWorkspace.setService(service);
  for (const failure of [error('network', 'receive_failed'), error('services', 'rejected', { status: 503, source: 'services', code: 'busy' }), error('cancelled', 'service_changed')]) {
    fixtureState.control.requestError = failure;
    const actual = await request(await workspace.network()); assert.deepEqual(actual, failure); assert.equal(actual instanceof Error, false);
  }
  fixtureState.control.requestError = undefined;
  const cancelled = await new Promise(resolve => {
    const handle = network.request({ method: 'GET', path: '/', query: [] }, { timeout_ms: 10000 }, () => assert.fail(), resolve); handle.cancel();
  });
  assert.equal(cancelled.kind, 'cancelled'); assert.equal(cancelled.reason, 'user_requested');
  fixtureState.control.requestError = error('not_bound', 'binding_revoked');
  assert.equal((await request(await workspace.network())).reason, 'binding_revoked');
  assert.equal((await (await workspace.open()).service()).binding.status, 'failed');
});

test('磁盘提交失败时，应用保留旧选择，不向另一个电脑发送请求', async () => {
  const fixtureState = fixture(); const { app, workspace } = fixtureState.create();
  await app.addService(JSON.stringify(invitation(1))); await app.addService(JSON.stringify(invitation(2)));
  fixtureState.control.failSave = true;
  await assert.rejects(app.select(id(1)), /磁盘写入失败/);
  assert.equal((await (await workspace.open()).service()).endpoint_id, id(2));
  assert.equal(fixtureState.control.saved.selected_endpoint_id, id(2));
});

test('解除绑定提示删除重加，删除当前设备清除目标并持久化', async () => {
  const fixtureState = fixture(); const { app, workspace } = fixtureState.create();
  await app.addService(JSON.stringify(invitation(1)));
  await assert.rejects(app.remove(id(1)), /先解除绑定/);
  fixtureState.control.requestError = error('not_bound', 'binding_revoked');
  const views = await observedDevices(app);
  assert.equal(views[0].status, 'binding_failed');
  assert.match(views[0].detail, /删除.*重新扫码绑定/);
  assert.match((await app.list())[0].detail, /删除.*重新扫码绑定/);
  await app.remove(id(1));
  assert.deepEqual(await app.list(), []);
  assert.equal(await (await workspace.open()).service(), null);
  assert.deepEqual(fixtureState.control.saved.computers, []);
  assert.equal(fixtureState.control.saved.selected_endpoint_id, null);
  assert.deepEqual(await fixtureState.create().app.list(), []);
});

test('删除其他已解绑设备保留当前目标，磁盘失败保留原记录', async () => {
  const record = number => ({ endpoint_id: id(number), relay_url: 'https://relay.example/', relay_token: 'secret',
    binding: number === 1 ? { status: 'failed', error: error('not_bound','binding_revoked') } : { status: 'bound', credential }, pending: null });
  const fixtureState = fixture({ computers: [record(1), record(2)], selected_endpoint_id: id(2) });
  const { app, workspace } = fixtureState.create();
  await app.list(); fixtureState.control.failSave = true;
  await assert.rejects(app.remove(id(1)), /磁盘写入失败/);
  assert.equal((await app.list()).length, 2);
  assert.equal((await (await workspace.open()).service()).endpoint_id, id(2));
  fixtureState.control.failSave = false;
  await app.remove(id(1));
  assert.equal((await app.list()).length, 1);
  assert.equal((await (await workspace.open()).service()).endpoint_id, id(2));
  assert.ok(!fixtureState.control.calls.some(call => ['bind', 'request', 'clear_service'].includes(call.operation)));
});
