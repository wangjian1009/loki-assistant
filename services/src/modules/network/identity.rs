use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::PathBuf,
};

use iroh::SecretKey;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct IdentityStore {
    path: PathBuf,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredIdentity {
    version: u32,
    secret_key: SecretKey,
    endpoint_id: String,
}

impl IdentityStore {
    pub(super) fn peers_path(&self) -> PathBuf {
        self.path.with_file_name("peers.json")
    }

    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// 返回身份及本次是否创建；损坏的身份禁止覆盖，避免静默改变设备身份。
    pub async fn ensure(&self) -> Result<(SecretKey, bool), String> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.load_or_create())
            .await
            .map_err(|error| format!("身份文件处理任务失败：{error}"))?
    }

    fn read(&self) -> Result<SecretKey, String> {
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&self.path)
            .map_err(|error| format!("无法读取本地身份：{error}"))?;
        if !file
            .metadata()
            .map_err(|error| error.to_string())?
            .is_file()
        {
            return Err("本地身份必须是普通文件".to_owned());
        }
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("无法限制身份文件权限：{error}"))?;
        let mut data = Vec::new();
        Read::by_ref(&mut file)
            .take(4097)
            .read_to_end(&mut data)
            .map_err(|error| format!("无法读取本地身份：{error}"))?;
        if data.len() > 4096 {
            return Err("本地身份文件过大".to_owned());
        }
        let identity: StoredIdentity = serde_json::from_slice(&data)
            .map_err(|_| "本地身份文件格式无效，请检查文件，不会自动覆盖".to_owned())?;
        if identity.version != 1 || identity.secret_key.public().to_string() != identity.endpoint_id
        {
            return Err("本地身份版本或公私钥不匹配，不会自动覆盖".to_owned());
        }
        Ok(identity.secret_key)
    }

    fn load_or_create(&self) -> Result<(SecretKey, bool), String> {
        match fs::symlink_metadata(&self.path) {
            Ok(_) => return self.read().map(|key| (key, false)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("无法检查本地身份：{error}")),
        }
        let parent = self.path.parent().ok_or("身份文件路径无效")?;
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(|error| format!("无法创建身份目录：{error}"))?;
        let key = SecretKey::generate();
        let data = serde_json::to_vec_pretty(&StoredIdentity {
            version: 1,
            endpoint_id: key.public().to_string(),
            secret_key: key.clone(),
        })
        .map_err(|_| "无法生成身份文件".to_owned())?;
        let temporary = parent.join(format!(".identity-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
                .map_err(|error| format!("无法创建身份临时文件：{error}"))?;
            file.write_all(&data)
                .and_then(|_| file.sync_all())
                .map_err(|error| format!("无法写入身份文件：{error}"))?;
            // 原子发布且不覆盖：并发创建者只保留一个身份。
            match fs::hard_link(&temporary, &self.path) {
                Ok(()) => {
                    File::open(parent)
                        .and_then(|directory| directory.sync_all())
                        .map_err(|error| format!("无法同步身份目录：{error}"))?;
                    Ok((key, true))
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    self.read().map(|key| (key, false))
                }
                Err(error) => Err(format!("无法保存本地身份：{error}")),
            }
        })();
        let _ = fs::remove_file(temporary);
        result
    }
}
