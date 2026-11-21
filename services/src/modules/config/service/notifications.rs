use std::{collections::HashSet, sync::Mutex as SyncMutex, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode},
};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

use super::*;

const JOURNAL: &str = "_pending_config_notifications";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pending {
    id: Uuid,
    module: String,
    item: String,
    instance: Option<String>,
    key: String,
    value: Value,
    previous: Option<Value>,
    delete: bool,
}

type Resource = (String, String, Option<String>);
pub(super) type InFlight = HashSet<Resource>;

struct NotificationGuard {
    active: Arc<SyncMutex<InFlight>>,
    resource: Resource,
}

impl Drop for NotificationGuard {
    fn drop(&mut self) {
        self.active
            .lock()
            .expect("通知状态锁有效")
            .remove(&self.resource);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct NotificationRequest {
    pub id: Uuid,
    pub deleted: bool,
    pub retry: bool,
    completion: tokio::sync::watch::Receiver<bool>,
}

impl NotificationRequest {
    pub async fn confirmed(mut self) -> bool {
        self.completion
            .wait_for(|confirmed| *confirmed)
            .await
            .is_ok()
    }
}

#[derive(PartialEq, Eq)]
enum Outcome {
    Confirmed,
    Processing,
    Failed,
}

impl ConfigService {
    pub(crate) fn attach_notifications(&self, router: &Arc<Router>) {
        let _ = self.notification_router.set(Arc::downgrade(router));
    }

    pub(in crate::modules::config) fn notifications_attached(&self) -> bool {
        self.notification_router.get().is_some()
    }

    pub(in crate::modules::config) fn has_notification(&self, module: &str, key: &str) -> bool {
        item_options(module, key).notification_endpoint.is_some()
    }

    pub(in crate::modules::config) async fn pending_notifications(
        &self,
        module: &str,
        key: &str,
    ) -> Vec<Value> {
        self.module_pending_notifications(module)
            .await
            .remove(key)
            .unwrap_or_default()
    }

    pub(in crate::modules::config) async fn module_pending_notifications(
        &self,
        module: &str,
    ) -> BTreeMap<String, Vec<Value>> {
        let mut result = BTreeMap::<String, Vec<Value>>::new();
        for entry in pending(&*self.values.read().await)
            .into_iter()
            .filter(|entry| entry.module == module)
        {
            result.entry(entry.item).or_default().push(json!({
                "key":entry.key, "status":"pending",
                "operation":if entry.delete {"delete"} else {"write"},
                "value":entry.value,
            }));
        }
        result
    }

    pub(crate) async fn notification_password(
        &self,
        id: Uuid,
        module: &str,
        item: &str,
        key: &str,
        field: &str,
    ) -> Result<String, ConfigServiceError> {
        let entry = pending(&*self.values.read().await)
            .into_iter()
            .find(|entry| {
                entry.id == id
                    && entry.module == module
                    && entry.item == item
                    && entry.key == key
                    && !entry.delete
            })
            .ok_or(ConfigServiceError::ActionNotFound)?;
        let path = entry.value[field]
            .as_str()
            .ok_or(ConfigServiceError::ActionNotFound)?;
        if !item_options(module, item).matches_password_path(field, key, path) {
            return Err(ConfigServiceError::ActionNotFound);
        }
        self.passwords()?
            .fetch(path)
            .await
            .map_err(|error| ConfigServiceError::Storage(error.to_string()))
    }

    pub(crate) async fn notification_password_already_confirmed(
        &self,
        id: Uuid,
        module: &str,
        item: &str,
        key: &str,
        field: &str,
    ) -> bool {
        pending(&*self.values.read().await)
            .into_iter()
            .any(|entry| {
                entry.id == id
                    && entry.module == module
                    && entry.item == item
                    && entry.key == key
                    && !entry.delete
                    && entry
                        .previous
                        .as_ref()
                        .is_some_and(|previous| previous.get(field).is_some())
                    && (entry.value.get(field).is_none()
                        || entry.value.get(field)
                            == entry
                                .previous
                                .as_ref()
                                .and_then(|previous| previous.get(field)))
            })
    }

    fn passwords(&self) -> Result<&PasswordService, ConfigServiceError> {
        self.passwords
            .as_deref()
            .ok_or_else(|| ConfigServiceError::Storage("密码模块不可用。".into()))
    }

    fn reserve_notification(
        &self,
        module: &str,
        item: &str,
        instance: Option<&str>,
    ) -> Result<NotificationGuard, ConfigServiceError> {
        let resource = (
            module.to_owned(),
            item.to_owned(),
            instance.map(str::to_owned),
        );
        let active = Arc::clone(&self.notifications_in_flight);
        if !active
            .lock()
            .expect("通知状态锁有效")
            .insert(resource.clone())
        {
            return Err(ConfigServiceError::RetryInProgress);
        }
        Ok(NotificationGuard { active, resource })
    }

    pub(in crate::modules::config) async fn notify_write(
        &self,
        module: String,
        item: String,
        instance: Option<String>,
        input: Option<Value>,
        create_only: bool,
    ) -> Result<Option<(Value, bool)>, ConfigServiceError> {
        self.notify_change(module, item, instance, input, create_only, false)
            .await
    }

    pub(in crate::modules::config) async fn notify_patch(
        &self,
        module: String,
        item: String,
        instance: Option<String>,
        patch: Value,
    ) -> Result<Option<(Value, bool)>, ConfigServiceError> {
        self.notify_change(module, item, instance, Some(patch), false, true)
            .await
    }

    async fn notify_change(
        &self,
        module: String,
        item: String,
        instance: Option<String>,
        input: Option<Value>,
        create_only: bool,
        patch: bool,
    ) -> Result<Option<(Value, bool)>, ConfigServiceError> {
        let guard = self.reserve_notification(&module, &item, instance.as_deref())?;
        let service = self.clone();
        tokio::spawn(async move {
            let _guard = guard;
            let Some(entry) = service
                .stage_notification(module, item, instance, input, create_only, patch)
                .await?
            else {
                return Ok(None);
            };
            let processing = service.send_notification(&entry, false).await?;
            Ok(Some((entry.value, processing)))
        })
        .await
        .map_err(|error| ConfigServiceError::Storage(error.to_string()))?
    }

    pub(in crate::modules::config) async fn retry_notification(
        &self,
        module: &str,
        item: &str,
        instance: Option<&str>,
    ) -> Result<bool, ConfigServiceError> {
        let guard = self.reserve_notification(module, item, instance)?;
        let entry = pending(&*self.values.read().await)
            .into_iter()
            .find(|entry| {
                entry.module == module
                    && entry.item == item
                    && entry.instance.as_deref() == instance
            })
            .ok_or(ConfigServiceError::PendingNotFound)?;
        let service = self.clone();
        tokio::spawn(async move {
            let _guard = guard;
            service.send_notification(&entry, true).await
        })
        .await
        .map_err(|error| ConfigServiceError::Storage(error.to_string()))?
    }

    async fn send_notification(
        &self,
        entry: &Pending,
        retry: bool,
    ) -> Result<bool, ConfigServiceError> {
        match self.deliver(entry, retry).await? {
            Outcome::Confirmed => Ok(false),
            Outcome::Processing => Ok(true),
            Outcome::Failed => Err(ConfigServiceError::ChangeRejected {
                module_id: entry.module.clone(),
                key: entry.item.clone(),
                message: "业务模块拒绝了配置变更。".into(),
            }),
        }
    }

    async fn stage_notification(
        &self,
        module: String,
        item: String,
        instance: Option<String>,
        input: Option<Value>,
        create_only: bool,
        patch: bool,
    ) -> Result<Option<Pending>, ConfigServiceError> {
        let metadata = self.module_metadata(&module).await?;
        let item_metadata =
            metadata
                .item(&item)
                .ok_or_else(|| ConfigServiceError::ItemNotFound {
                    module_id: module.clone(),
                    key: item.clone(),
                })?;
        let options = item_options(&module, &item);
        let _commit = self.commits.lock().await;
        let mut values = self.values.read().await.clone();
        let existing = pending(&values).into_iter().find(|entry| {
            entry.module == module && entry.item == item && entry.instance == instance
        });
        if let Some(entry) = &existing {
            if create_only || (instance.is_none() && !patch) || entry.delete {
                return Err(ConfigServiceError::ChangePending);
            }
            values
                .get_mut(JOURNAL)
                .expect("待确认操作存在")
                .remove(&entry.id.to_string());
        }
        let effective = self
            .resolve_value(
                item_metadata,
                values.get(&module).and_then(|entries| entries.get(&item)),
            )
            .await?
            .map(|data| data.value);
        let id = Uuid::new_v4();
        let delete = input.is_none();
        let mut secrets = Vec::new();
        let (key, value, previous) = if let Some(instance) = &instance {
            let structure = options
                .structure
                .as_ref()
                .ok_or(ConfigServiceError::ActionNotFound)?;
            let key_field = options.notification_key_field()?;
            let mut records = effective
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default();
            let index = records
                .iter()
                .position(|record| record[key_field].as_str() == Some(instance));
            let previous = index.map(|index| records[index].clone());
            if create_only && previous.is_some() {
                return Err(ConfigServiceError::RecordExists);
            }
            let value = if let Some(input) = input {
                let changes = input.as_object().ok_or_else(|| {
                    ConfigServiceError::InvalidValue("配置记录必须为 JSON object。".into())
                })?;
                let mut record = existing
                    .as_ref()
                    .and_then(|entry| entry.value.as_object())
                    .cloned()
                    .or_else(|| previous.as_ref().and_then(Value::as_object).cloned())
                    .unwrap_or_default();
                if patch && previous.is_none() && existing.is_none() {
                    return Err(ConfigServiceError::ActionNotFound);
                }
                if patch && changes.contains_key(key_field) {
                    return Err(ConfigServiceError::InvalidValue(
                        "不能修改配置记录主键。".into(),
                    ));
                }
                for (key, value) in changes {
                    let field = structure
                        .fields
                        .iter()
                        .find(|field| &field.key == key)
                        .ok_or_else(|| {
                            ConfigServiceError::InvalidValue("配置记录包含未声明字段。".into())
                        })?;
                    if field.read_only {
                        return Err(ConfigServiceError::InvalidValue(
                            "配置记录包含只读字段。".into(),
                        ));
                    }
                    if field.data_type == ConfigDataType::Password {
                        let base = options.password_path(key, instance)?;
                        if patch && value.is_null() {
                            record.remove(key);
                            continue;
                        }
                        let mut secret = value.clone();
                        if patch && value.is_object() {
                            let mut current = Value::Null;
                            if let Some(path) = record.get(key).and_then(Value::as_str)
                                && options.matches_password_path(key, instance, path)
                            {
                                let previous =
                                    self.passwords()?.fetch(path).await.map_err(|error| {
                                        ConfigServiceError::Storage(error.to_string())
                                    })?;
                                current = serde_json::from_str(&previous).unwrap_or(Value::Null);
                            }
                            merge_patch(&mut current, value);
                            secret = current;
                        }
                        let secret = match secret {
                            Value::String(secret) => secret,
                            Value::Object(_) => secret.to_string(),
                            _ => {
                                return Err(ConfigServiceError::InvalidValue(
                                    "密码字段需要字符串或 JSON 对象。".into(),
                                ));
                            }
                        };
                        if secret.trim().is_empty() {
                            return Err(ConfigServiceError::InvalidValue(
                                "密码字段不能为空。".into(),
                            ));
                        }
                        let path = format!("{}-{id}.gpg", base.trim_end_matches(".gpg"));
                        record.insert(key.clone(), Value::String(path.clone()));
                        secrets.push((path, secret));
                    } else if patch && value.is_null() {
                        record.remove(key);
                    } else if patch {
                        merge_patch(record.entry(key.clone()).or_insert(Value::Null), value);
                    } else {
                        record.insert(key.clone(), value.clone());
                    }
                }
                if existing.is_none()
                    && previous.is_none()
                    && !options.create_fields.is_empty()
                    && changes
                        .keys()
                        .any(|field| !options.create_fields.contains(&field.as_str()))
                {
                    return Err(ConfigServiceError::InvalidValue(
                        "首次新增只能提交元数据声明的字段。".into(),
                    ));
                }
                if record
                    .get(key_field)
                    .is_some_and(|key| key.as_str() != Some(instance))
                {
                    return Err(ConfigServiceError::InvalidValue(
                        "配置记录标识不一致。".into(),
                    ));
                }
                record.insert(key_field.to_owned(), Value::String(instance.clone()));
                let value = Value::Object(record);
                if let Some(index) = index {
                    records[index] = value.clone();
                } else {
                    records.push(value.clone());
                }
                item_metadata.validate_with(&options, &Value::Array(records))?;
                value
            } else {
                if let Some(index) = index {
                    records.remove(index);
                    item_metadata.validate_with(&options, &Value::Array(records))?;
                }
                match existing
                    .as_ref()
                    .map(|entry| &entry.value)
                    .or(previous.as_ref())
                {
                    Some(record) => record.clone(),
                    None => return Ok(None),
                }
            };
            (instance.clone(), value, previous)
        } else {
            if matches!(
                item_metadata.data_type,
                ConfigDataType::List | ConfigDataType::Dictionary | ConfigDataType::Password
            ) {
                return Err(ConfigServiceError::InvalidValue(
                    "请使用带主键的配置记录接口。".into(),
                ));
            }
            let value = match input {
                Some(value) if patch => {
                    let current = existing
                        .as_ref()
                        .map(|entry| entry.value.clone())
                        .or_else(|| effective.clone());
                    let merged = patch_structure(item_metadata, &options, current, &value)?;
                    item_metadata.validate_with(&options, &merged)?;
                    merged
                }
                Some(value) => self.normalize_value(&module, item_metadata, &value).await?,
                None => match values
                    .get(&module)
                    .and_then(|entries| entries.get(&item))
                    .cloned()
                {
                    Some(value) => value,
                    None => return Ok(None),
                },
            };
            let key = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            (key, value, effective)
        };
        if key.is_empty() || key.chars().any(char::is_control) {
            return Err(ConfigServiceError::InvalidValue(
                "通知接入点的键无效。".into(),
            ));
        }
        let entry = Pending {
            id,
            module,
            item,
            instance,
            key,
            value,
            previous,
            delete,
        };
        let mut written: Vec<String> = Vec::new();
        for (path, secret) in secrets {
            let store = self.passwords()?;
            if let Err(error) = store.save(&path, &secret).await {
                for path in &written {
                    let _ = store.remove(path).await;
                }
                return Err(ConfigServiceError::Storage(error.to_string()));
            }
            written.push(path);
        }
        values
            .entry(JOURNAL.into())
            .or_default()
            .insert(id.to_string(), json!(entry));
        if let Err(error) = self.persist_notifications(values).await {
            if let Some(store) = &self.passwords {
                for path in written {
                    let _ = store.remove(&path).await;
                }
            }
            return Err(error);
        }
        if let Some(old) = existing {
            self.discard_notification_passwords(
                &entry,
                &old.value,
                &[Some(&entry.value), entry.previous.as_ref()],
            )
            .await;
        }
        Ok(Some(entry))
    }

    async fn persist_notifications(&self, values: StoredValues) -> Result<(), ConfigServiceError> {
        if let Some(storage) = &self.storage {
            storage.write(values.clone()).await?;
        }
        *self.values.write().await = values;
        Ok(())
    }

    async fn deliver(&self, entry: &Pending, retry: bool) -> Result<Outcome, ConfigServiceError> {
        let method = if entry.delete {
            Method::DELETE
        } else {
            Method::POST
        };
        let (completion, listener) = tokio::sync::watch::channel(false);
        let result = match self
            .dispatch_notification(entry, method, retry, listener)
            .await
        {
            Some(response) => match response.status() {
                StatusCode::OK | StatusCode::NO_CONTENT => Outcome::Confirmed,
                status if status.is_client_error() || status.is_server_error() => Outcome::Failed,
                _ => Outcome::Processing,
            },
            None => Outcome::Processing,
        };
        if result == Outcome::Processing {
            return Ok(result);
        }
        let _commit = self.commits.lock().await;
        let mut values = self.values.read().await.clone();
        let id = entry.id.to_string();
        if !values
            .get(JOURNAL)
            .is_some_and(|entries| entries.contains_key(&id))
        {
            return Err(ConfigServiceError::PendingNotFound);
        }
        values.get_mut(JOURNAL).expect("待确认操作存在").remove(&id);
        if result == Outcome::Confirmed {
            let entries = values.entry(entry.module.clone()).or_default();
            if let Some(instance) = &entry.instance {
                let options = item_options(&entry.module, &entry.item);
                let key_field = options.notification_key_field()?;
                let default = self
                    .module_metadata(&entry.module)
                    .await?
                    .item(&entry.item)
                    .and_then(|item| item.default.clone())
                    .unwrap_or_else(|| json!([]));
                let records = entries
                    .entry(entry.item.clone())
                    .or_insert(default)
                    .as_array_mut()
                    .ok_or(ConfigServiceError::ActionNotFound)?;
                if let Some(index) = records
                    .iter()
                    .position(|record| record[key_field].as_str() == Some(instance))
                {
                    if entry.delete {
                        records.remove(index);
                    } else {
                        records[index] = entry.value.clone();
                    }
                } else if !entry.delete {
                    records.push(entry.value.clone());
                }
            } else if entry.delete {
                entries.remove(&entry.item);
            } else {
                entries.insert(entry.item.clone(), entry.value.clone());
            }
        }
        self.persist_notifications(values).await?;
        self.clean_notification_passwords(entry, &result).await;
        drop(_commit);
        if result == Outcome::Confirmed {
            let _ = completion.send(true);
        }
        Ok(result)
    }

    async fn clean_notification_passwords(&self, entry: &Pending, result: &Outcome) {
        if *result == Outcome::Failed {
            self.discard_notification_passwords(entry, &entry.value, &[entry.previous.as_ref()])
                .await;
        } else if entry.delete {
            self.discard_notification_passwords(entry, &entry.value, &[])
                .await;
            if let Some(previous) = &entry.previous {
                self.discard_notification_passwords(entry, previous, &[Some(&entry.value)])
                    .await;
            }
        } else if let Some(previous) = &entry.previous {
            self.discard_notification_passwords(entry, previous, &[Some(&entry.value)])
                .await;
        }
    }

    async fn discard_notification_passwords(
        &self,
        entry: &Pending,
        discarded: &Value,
        retained: &[Option<&Value>],
    ) {
        let (Some(store), Some(instance)) = (&self.passwords, &entry.instance) else {
            return;
        };
        let options = item_options(&entry.module, &entry.item);
        for field in options
            .structure
            .iter()
            .flat_map(|structure| &structure.fields)
            .filter(|field| field.data_type == ConfigDataType::Password)
        {
            if let Some(path) = discarded[&field.key].as_str()
                && !retained
                    .iter()
                    .flatten()
                    .any(|value| value[&field.key].as_str() == Some(path))
                && options.matches_password_path(&field.key, instance, path)
                && let Err(error) = store.remove(path).await
            {
                eprintln!("清理配置凭据失败：{error}");
            }
        }
    }

    async fn dispatch_notification(
        &self,
        entry: &Pending,
        method: Method,
        retry: bool,
        completion: tokio::sync::watch::Receiver<bool>,
    ) -> Option<axum::response::Response> {
        let router = self.notification_router.get()?.upgrade()?;
        let options = item_options(&entry.module, &entry.item);
        let endpoint = options.notification_endpoint?;
        let uri = format!(
            "{endpoint}/{}",
            utf8_percent_encode(&entry.key, NON_ALPHANUMERIC)
        );
        let body = if method == Method::POST {
            let mut public = entry.value.clone();
            if let Some(record) = public.as_object_mut() {
                for field in options
                    .structure
                    .iter()
                    .flat_map(|structure| &structure.fields)
                    .filter(|field| field.data_type == ConfigDataType::Password)
                {
                    record.remove(&field.key);
                }
            }
            Body::from(public.to_string())
        } else {
            Body::empty()
        };
        let request = Request::builder()
            .uri(uri)
            .method(method)
            .header("content-type", "application/json")
            .extension(NotificationRequest {
                id: entry.id,
                deleted: entry.delete,
                retry,
                completion,
            })
            .body(body)
            .ok()?;
        tokio::time::timeout(Duration::from_secs(15), (*router).clone().oneshot(request))
            .await
            .ok()?
            .ok()
    }
}

fn pending(values: &StoredValues) -> Vec<Pending> {
    values
        .get(JOURNAL)
        .into_iter()
        .flat_map(|entries| entries.values())
        .filter_map(|entry| serde_json::from_value(entry.clone()).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    use crate::modules::config::registry::ConfigItemRegistration;
    use axum::{Extension, Json, extract::Path, routing::post};

    #[linkme::distributed_slice(crate::modules::config::registry::CONFIG_ITEMS)]
    static SCALAR: ConfigItemRegistration = ConfigItemRegistration {
        module_id: "notification_test",
        key: "scalar",
        options: || ConfigItemOptions {
            notification_endpoint: Some("/business/value"),
            ..ConfigItemOptions::default()
        },
    };

    #[tokio::test(start_paused = true)]
    async fn scalar_notifications_confirm_rollback_and_wait_for_delete() {
        let service = ConfigService::new(ConfigModuleMetadata {
            id: "notification_test".into(),
            name: "通知测试".into(),
            description: String::new(),
            version: "0.1.0".into(),
            items: vec![
                serde_json::from_value(json!({
                    "key":"scalar","name":"标量","description":"","data_type":"integer",
                    "required":false,"default":null
                }))
                .unwrap(),
            ],
        });
        let ready = Arc::new(AtomicBool::new(false));
        let router = Arc::new(Router::new().route(
            "/business/value/{key}",
            post(|Path(key): Path<String>, Extension(_event): Extension<NotificationRequest>, Json(value): Json<Value>| async move {
                assert_eq!(key, value.to_string());
                if key == "1" { StatusCode::OK }
                else if key == "2" { StatusCode::BAD_REQUEST }
                else { StatusCode::ACCEPTED }
            })
            .delete(|Path(key): Path<String>, Extension(event): Extension<NotificationRequest>,
                     Extension(ready): Extension<Arc<AtomicBool>>,
                     Extension(original): Extension<Arc<SyncMutex<Option<Uuid>>>>| async move {
                assert_eq!(key, "1");
                assert!(event.deleted);
                let mut original = original.lock().unwrap();
                assert_eq!(*original.get_or_insert(event.id), event.id);
                if ready.load(Ordering::Relaxed) { StatusCode::NO_CONTENT } else { StatusCode::ACCEPTED }
            })
        ).layer(Extension(ready.clone()))
         .layer(Extension(Arc::new(SyncMutex::new(None::<Uuid>)))));
        service.attach_notifications(&router);
        let module = "notification_test";
        let key = "scalar";
        assert_eq!(
            service
                .notify_write(module.into(), key.into(), None, Some(json!(1)), false)
                .await
                .unwrap(),
            Some((json!(1), false))
        );
        assert!(matches!(
            service
                .notify_write(module.into(), key.into(), None, Some(json!(2)), false)
                .await,
            Err(ConfigServiceError::ChangeRejected { .. })
        ));
        assert_eq!(service.value(module, key).await.unwrap().value, 1);
        assert_eq!(
            service
                .notify_write(module.into(), key.into(), None, None, false)
                .await
                .unwrap(),
            Some((json!(1), true))
        );
        assert_eq!(
            service.pending_notifications(module, key).await[0]["operation"],
            "delete"
        );
        assert_eq!(service.value(module, key).await.unwrap().value, 1);
        ready.store(true, Ordering::Relaxed);
        tokio::time::advance(Duration::from_secs(60)).await;
        assert_eq!(service.value(module, key).await.unwrap().value, 1);
        assert!(!service.retry_notification(module, key, None).await.unwrap());
        assert!(matches!(
            service.value(module, key).await,
            Err(ConfigServiceError::ValueNotSet { .. })
        ));
        assert!(service.pending_notifications(module, key).await.is_empty());
        assert!(matches!(
            service.retry_notification(module, key, None).await,
            Err(ConfigServiceError::PendingNotFound)
        ));
    }
}
