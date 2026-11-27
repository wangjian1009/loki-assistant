use std::{
    collections::HashMap,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use base64::{Engine as _, engine::general_purpose};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::{agent_shell::AgentShell, service_proxy::StatusSubscription};

const MAX_IMAGE_BYTES: usize = 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct ImageMapping {
    source: String,
    file: String,
}

#[derive(Default)]
pub(crate) struct ImageCache {
    state: Mutex<CacheState>,
}

#[derive(Default)]
struct CacheState {
    sessions: HashMap<PathBuf, watch::Sender<()>>,
    observer: Option<StatusSubscription>,
}

fn key(text: &str) -> String {
    general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(text.as_bytes()))
}

fn remove_directory(path: &Path) {
    if let Err(error) = fs::remove_dir_all(path)
        && error.kind() != ErrorKind::NotFound
    {
        eprintln!("无法清理已关闭会话的图片缓存：{error}");
    }
}

impl ImageCache {
    fn state(&self) -> MutexGuard<'_, CacheState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn reconcile(&self, directory: &Path, endpoint: &str, ids: &[String]) {
        let device = directory.join(key(endpoint));
        let active = ids.iter().map(|id| key(id)).collect::<Vec<_>>();
        let mut state = self.state();
        state.sessions.retain(|path, _| {
            path.parent() != Some(device.as_path())
                || path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|id| active.iter().any(|key| key == id))
        });
        if let Ok(entries) = fs::read_dir(&device) {
            for entry in entries.flatten() {
                if !active.iter().any(|id| entry.file_name() == id.as_str()) {
                    remove_directory(&entry.path());
                }
            }
        }
    }

    pub(crate) fn close(&self, cache: &Path, endpoint: &str, session: &str) {
        let path = cache
            .join("agent-shell-v2")
            .join(key(endpoint))
            .join(key(session));
        let mut state = self.state();
        state.sessions.remove(&path);
        remove_directory(&path);
    }

    pub(crate) async fn read(
        self: Arc<Self>,
        shell: &AgentShell,
        cache: PathBuf,
        session: String,
        source: String,
    ) -> Result<String, String> {
        let (endpoint, generation) = shell.image_target();
        let endpoint = endpoint.ok_or("没有当前设备")?;
        let root = cache.join("agent-shell-v2");
        let directory = root.join(key(&endpoint)).join(key(&session));
        let name = key(&source);
        let mapping_path = directory.join(format!("{name}.json"));
        let mut closed = {
            let mut state = self.state();
            if state.observer.is_none() {
                let cache = Arc::downgrade(&self);
                state.observer = Some(shell.watch_image_sessions(move |update| {
                    let Some(cache) = cache.upgrade() else {
                        return false;
                    };
                    if let Some((endpoint, ids)) = update {
                        cache.reconcile(&root, &endpoint, &ids);
                    }
                    true
                }));
            }
            if !shell.has_session(generation, &session) {
                return Err("会话已关闭".into());
            }
            if let Ok(json) = fs::read(&mapping_path)
                && let Ok(mapping) = serde_json::from_slice::<ImageMapping>(&json)
                && mapping.source == source
                && ["png", "jpg", "gif", "webp"]
                    .iter()
                    .any(|ext| mapping.file == format!("{name}.{ext}"))
            {
                let image = directory.join(mapping.file);
                if image.is_file() {
                    return Ok(image.to_string_lossy().into_owned());
                }
            }
            let scope = state
                .sessions
                .entry(directory.clone())
                .or_insert_with(|| watch::channel(()).0);
            // Receiver 不持有 Sender，移除会话会终止网络请求。
            scope.subscribe()
        };
        let image = tokio::select! {
            biased;
            _ = closed.changed() => return Err("会话已关闭".into()),
            result = shell.read_image(generation, &session, source.clone()) =>
                result.map_err(|_| "无法从电脑读取会话图片")?,
        };
        let extension = match image.mime.as_str() {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            _ => return Err("图片格式不受支持".into()),
        };
        let data = general_purpose::STANDARD
            .decode(image.data)
            .map_err(|_| "图片数据无效")?;
        if data.is_empty() || data.len() > MAX_IMAGE_BYTES {
            return Err("图片大小无效".into());
        }
        let _state = self.state();
        if closed.has_changed().is_err() || !shell.has_session(generation, &session) {
            return Err("会话已关闭".into());
        }
        fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        let file = format!("{name}.{extension}");
        let mapping = serde_json::to_vec(&ImageMapping {
            source,
            file: file.clone(),
        })
        .map_err(|e| e.to_string())?;
        let image = directory.join(file);
        atomic_write(&image, &data).map_err(|e| e.to_string())?;
        if let Err(error) = atomic_write(&mapping_path, &mapping) {
            let _ = fs::remove_file(image);
            return Err(error.to_string());
        }
        Ok(image.to_string_lossy().into_owned())
    }
}

fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension(uuid::Uuid::new_v4().to_string());
    let result = fs::write(&temporary, data).and_then(|_| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
