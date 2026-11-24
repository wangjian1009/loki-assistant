use std::{collections::HashSet, time::Duration};

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

pub(super) fn legacy_pending_password_id(base: &str, path: &str) -> Option<Uuid> {
    let suffix = path.strip_prefix(base.strip_suffix(".gpg")?)?;
    Uuid::parse_str(suffix.strip_prefix('-')?.strip_suffix(".gpg")?).ok()
}

fn pending_password_location(base: &str) -> (String, &str) {
    match base.rsplit_once('/') {
        Some((parent, name)) => (format!("{parent}/pending/"), name),
        None => ("pending/".to_owned(), base),
    }
}

pub(super) fn pending_password_id(base: &str, path: &str) -> Option<Uuid> {
    let (prefix, name) = pending_password_location(base);
    let (id, file) = path.strip_prefix(&prefix)?.split_once('/')?;
    if file != name {
        return None;
    }
    Uuid::parse_str(id).ok()
}

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    removed_password_entries: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    validation_error: Option<NotificationValidationError>,
}

struct PasswordChange {
    path: String,
    contents: Option<String>,
    previous: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotificationValidationError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clear_password_fields: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct NotificationConfirmedFields(pub serde_json::Map<String, Value>);

type Resource = (String, String, Option<String>);
pub(super) type InFlight = HashSet<Resource>;

struct NotificationGuard {
    active: crate::process::Process<InFlight>,
    resource: Resource,
}

impl Drop for NotificationGuard {
    fn drop(&mut self) {
        let resource = self.resource.clone();
        self.active.post(move |active| {
            active.remove(&resource);
        });
    }
}

#[derive(Debug, Clone)]
pub(crate) struct NotificationRequest {
    pub id: Uuid,
    pub deleted: bool,
    pub password_entry_removed: bool,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Confirmed,
    Processing,
    Failed,
}

impl ConfigService {
    pub(crate) fn attach_notifications(&self, router: &Arc<Router>) {
        let _ = self
            .notification_router
            .set(crate::process::Process::start(Arc::downgrade(router)));
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
        for entry in pending(&self.stored_values().await)
            .into_iter()
            .filter(|entry| entry.module == module)
        {
            let mut record = json!({
                "key":entry.key,
                "status":if entry.validation_error.is_some() {"invalid"} else {"pending"},
                "operation":if entry.delete {"delete"} else {"write"},
                "value":entry.value,
            });
            if let Some(error) = entry.validation_error {
                record["error"] =
                    json!({"source":"business","code":error.code,"message":error.message});
            }
            result.entry(entry.item).or_default().push(record);
        }
        result
    }

    /// 返回待确认密码快照。已有记录的密码字段均未变化时返回 None。
    pub(crate) async fn notification_passwords<const N: usize>(
        &self,
        id: Uuid,
        module: &str,
        item: &str,
        key: &str,
        fields: [&str; N],
    ) -> Result<Option<[Option<String>; N]>, ConfigServiceError> {
        let entry = pending(&self.stored_values().await)
            .into_iter()
            .find(|entry| {
                entry.id == id
                    && entry.module == module
                    && entry.item == item
                    && entry.key == key
                    && !entry.delete
            })
            .ok_or(ConfigServiceError::ActionNotFound)?;
        if entry.previous.as_ref().is_some_and(|previous| {
            fields.iter().any(|field| previous.get(*field).is_some())
                && fields
                    .iter()
                    .all(|field| previous.get(*field) == entry.value.get(*field))
        }) {
            return Ok(None);
        }
        self.load_record_passwords(&item_options(module, item), &entry.value, key, fields)
            .await
            .map(Some)
    }

    pub(super) fn passwords(&self) -> Result<&PasswordService, ConfigServiceError> {
        self.passwords
            .as_deref()
            .ok_or_else(|| ConfigServiceError::Storage("密码模块不可用。".into()))
    }

    async fn reserve_notification(
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
        let active = self.notifications_in_flight.clone();
        let sender = active.clone();
        active
            .call(move |active| {
                if active.insert(resource.clone()) {
                    Ok(NotificationGuard {
                        active: sender,
                        resource: resource.clone(),
                    })
                } else {
                    Err(ConfigServiceError::RetryInProgress)
                }
            })
            .await
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

    pub(crate) async fn notify_patch_if_passwords_match<const N: usize>(
        &self,
        module: &str,
        item: &str,
        instance: &str,
        patch: Value,
        fields: [&str; N],
        expected: [Option<String>; N],
    ) -> Result<bool, ConfigServiceError> {
        let guard = self
            .reserve_notification(module, item, Some(instance))
            .await?;
        if self
            .read_record_passwords(module, item, instance, fields)
            .await?
            != expected
        {
            return Ok(false);
        }
        let service = self.clone();
        let (module, item, instance) = (module.to_owned(), item.to_owned(), instance.to_owned());
        tokio::spawn(async move {
            let _guard = guard;
            let mut entry = service
                .stage_notification(module, item, Some(instance), Some(patch), false, true, true)
                .await?
                .ok_or(ConfigServiceError::ActionNotFound)?;
            Ok(!service.send_notification(&mut entry).await?)
        })
        .await
        .map_err(|error| ConfigServiceError::Storage(error.to_string()))?
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
        let guard = self
            .reserve_notification(&module, &item, instance.as_deref())
            .await?;
        let service = self.clone();
        tokio::spawn(async move {
            let _guard = guard;
            let Some(mut entry) = service
                .stage_notification(module, item, instance, input, create_only, patch, false)
                .await?
            else {
                return Ok(None);
            };
            let processing = service.send_notification(&mut entry).await?;
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
        let guard = self.reserve_notification(module, item, instance).await?;
        let mut entry = pending(&self.stored_values().await)
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
            service.send_notification(&mut entry).await
        })
        .await
        .map_err(|error| ConfigServiceError::Storage(error.to_string()))?
    }

    async fn send_notification(&self, entry: &mut Pending) -> Result<bool, ConfigServiceError> {
        match self.deliver(entry).await? {
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
        preserve_pending: bool,
    ) -> Result<Option<Pending>, ConfigServiceError> {
        let metadata = self.module_metadata(&module).await?;
        let options = item_options(&module, &item);
        let service = self.clone();
        self.commits
            .run(async move {
                let credentials = service.record_credentials.clone();
                credentials
                    .run(async move {
                        let item_metadata = metadata.item(&item).ok_or_else(|| {
                            ConfigServiceError::ItemNotFound {
                                module_id: module.clone(),
                                key: item.clone(),
                            }
                        })?;
                        let mut values = service.stored_values().await;
                        let existing = pending(&values).into_iter().find(|entry| {
                            entry.module == module
                                && entry.item == item
                                && entry.instance == instance
                        });
                        if let Some(entry) = &existing {
                            if create_only
                                || (instance.is_none() && !patch)
                                || entry.delete
                                || (preserve_pending && entry.validation_error.is_none())
                            {
                                return Err(ConfigServiceError::ChangePending);
                            }
                            values
                                .get_mut(JOURNAL)
                                .expect("待确认操作存在")
                                .remove(&entry.id.to_string());
                        }
                        let effective = service
                            .resolve_value(
                                item_metadata,
                                values.get(&module).and_then(|entries| entries.get(&item)),
                            )
                            .await?
                            .map(|data| data.value);
                        let id = Uuid::new_v4();
                        let delete = input.is_none();
                        let mut password_changes = Vec::new();
                        let mut removed_password_entries = Vec::new();
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
                                    ConfigServiceError::InvalidValue(
                                        "配置记录必须为 JSON object。".into(),
                                    )
                                })?;
                                let mut record = existing
                                    .as_ref()
                                    .and_then(|entry| entry.value.as_object())
                                    .cloned()
                                    .or_else(|| {
                                        previous.as_ref().and_then(Value::as_object).cloned()
                                    })
                                    .unwrap_or_default();
                                if patch && previous.is_none() && existing.is_none() {
                                    return Err(ConfigServiceError::ActionNotFound);
                                }
                                if patch && changes.contains_key(key_field) {
                                    return Err(ConfigServiceError::InvalidValue(
                                        "不能修改配置记录主键。".into(),
                                    ));
                                }
                                let mut line_changes =
                                    BTreeMap::<String, BTreeMap<&str, Option<&str>>>::new();
                                for (key, value) in changes {
                                    let field = structure
                                        .fields
                                        .iter()
                                        .find(|field| &field.key == key)
                                        .ok_or_else(|| {
                                            ConfigServiceError::InvalidValue(
                                                "配置记录包含未声明字段。".into(),
                                            )
                                        })?;
                                    if field.read_only {
                                        return Err(ConfigServiceError::InvalidValue(
                                            "配置记录包含只读字段。".into(),
                                        ));
                                    }
                                    if field.data_type.is_password() {
                                        let base = options.password_path(key, instance)?;
                                        let replacement = if patch && value.is_null() {
                                            None
                                        } else {
                                            if let Some(checker) = &field.checker {
                                                checker.validate(&field.label, value)?;
                                            }
                                            Some(
                                                value
                                                    .as_str()
                                                    .filter(|text| {
                                                        !text.trim().is_empty()
                                                            && !text.chars().any(char::is_control)
                                                    })
                                                    .ok_or_else(|| {
                                                        ConfigServiceError::InvalidValue(
                                                            "密码字段需要非空的单行字符串。".into(),
                                                        )
                                                    })?,
                                            )
                                        };
                                        line_changes
                                            .entry(base)
                                            .or_default()
                                            .insert(key, replacement);
                                    } else if patch && value.is_null() {
                                        record.remove(key);
                                    } else if patch {
                                        merge_patch(
                                            record.entry(key.clone()).or_insert(Value::Null),
                                            value,
                                        );
                                    } else {
                                        record.insert(key.clone(), value.clone());
                                    }
                                }
                                if existing.is_none()
                                    && previous.is_none()
                                    && !options.create_fields.is_empty()
                                    && changes.keys().any(|field| {
                                        !options.create_fields.contains(&field.as_str())
                                    })
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
                                record
                                    .insert(key_field.to_owned(), Value::String(instance.clone()));
                                for (base, changes) in line_changes {
                                    if let Some(first) = options
                                        .password_entry_fields(instance, &base)
                                        .find(|field| field.data_type.password_key().is_none())
                                        && changes
                                            .get(first.key.as_str())
                                            .is_some_and(Option::is_none)
                                    {
                                        for field in options.password_entry_fields(instance, &base)
                                        {
                                            record.remove(&field.key);
                                        }
                                        removed_password_entries.push(first.key.clone());
                                        continue;
                                    }
                                    let (path, backup) = service
                                        .pending_password_destination(
                                            &options,
                                            instance,
                                            &base,
                                            id,
                                            &record,
                                            previous.as_ref(),
                                        )
                                        .await?;
                                    if let Some(contents) = service
                                        .stage_password_lines(
                                            &options,
                                            instance,
                                            &base,
                                            &path,
                                            &changes,
                                            &mut record,
                                        )
                                        .await?
                                    {
                                        password_changes.push(PasswordChange {
                                            path,
                                            contents: Some(contents),
                                            previous: backup,
                                        });
                                    }
                                }
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
                                    item_metadata
                                        .validate_with(&options, &Value::Array(records))?;
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
                                ConfigDataType::List
                                    | ConfigDataType::Dictionary
                                    | ConfigDataType::Password { .. }
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
                                    let merged =
                                        patch_structure(item_metadata, &options, current, &value)?;
                                    item_metadata.validate_with(&options, &merged)?;
                                    merged
                                }
                                Some(value) => {
                                    service
                                        .normalize_value(&module, item_metadata, &value)
                                        .await?
                                }
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
                            removed_password_entries,
                            validation_error: None,
                        };
                        values
                            .entry(JOURNAL.into())
                            .or_default()
                            .insert(id.to_string(), json!(entry));
                        service
                            .persist_password_changes(values, &password_changes)
                            .await?;
                        if let Some(old) = existing {
                            service
                                .discard_notification_passwords(
                                    &entry,
                                    &old.value,
                                    &[Some(&entry.value), entry.previous.as_ref()],
                                )
                                .await;
                        }
                        Ok(Some(entry))
                    })
                    .await
            })
            .await
    }

    async fn persist_notifications(&self, values: StoredValues) -> Result<(), ConfigServiceError> {
        self.persist_values(values).await
    }

    pub async fn migrate_pending_password_paths(&self) -> Result<(), ConfigServiceError> {
        let service = self.clone();
        self.commits
            .run(async move {
                let credentials = service.record_credentials.clone();
                credentials
                    .run(async move {
                        let mut values = service.stored_values().await;
                        let mut migrated = Vec::new();
                        let mut password_changes = Vec::new();
                        for mut entry in pending(&values).into_iter().filter(|entry| !entry.delete)
                        {
                            let (Some(instance), Some(record)) =
                                (entry.instance.as_deref(), entry.value.as_object_mut())
                            else {
                                continue;
                            };
                            let options = item_options(&entry.module, &entry.item);
                            let mut bases = std::collections::BTreeSet::new();
                            for field in options
                                .structure
                                .iter()
                                .flat_map(|structure| &structure.fields)
                            {
                                if !field.data_type.is_password() {
                                    continue;
                                }
                                if let Some(path) = record.get(&field.key).and_then(Value::as_str) {
                                    let base = options.password_path(&field.key, instance)?;
                                    if legacy_pending_password_id(&base, path).is_some() {
                                        bases.insert(base);
                                    }
                                }
                            }
                            if bases.is_empty() {
                                continue;
                            }
                            let old_value = Value::Object(record.clone());
                            for base in bases {
                                let (path, previous) = service
                                    .pending_password_destination(
                                        &options,
                                        instance,
                                        &base,
                                        entry.id,
                                        record,
                                        entry.previous.as_ref(),
                                    )
                                    .await?;
                                if let Some(contents) = service
                                    .stage_password_lines(
                                        &options,
                                        instance,
                                        &base,
                                        &path,
                                        &BTreeMap::new(),
                                        record,
                                    )
                                    .await?
                                {
                                    password_changes.push(PasswordChange {
                                        path,
                                        contents: Some(contents),
                                        previous,
                                    });
                                }
                            }
                            values
                                .get_mut(JOURNAL)
                                .expect("待确认操作存在")
                                .insert(entry.id.to_string(), json!(entry));
                            migrated.push((entry, old_value));
                        }
                        if !migrated.is_empty() {
                            service
                                .persist_password_changes(values, &password_changes)
                                .await?;
                            for (entry, old_value) in migrated {
                                service
                                    .discard_notification_passwords(
                                        &entry,
                                        &old_value,
                                        &[Some(&entry.value), entry.previous.as_ref()],
                                    )
                                    .await;
                            }
                        }
                        Ok(())
                    })
                    .await
            })
            .await
    }

    async fn pending_password_destination(
        &self,
        options: &ConfigItemOptions,
        instance: &str,
        base: &str,
        id: Uuid,
        record: &serde_json::Map<String, Value>,
        previous: Option<&Value>,
    ) -> Result<(String, Option<String>), ConfigServiceError> {
        let confirmed = previous.is_some_and(|previous| {
            options
                .password_entry_fields(instance, base)
                .any(|field| previous[&field.key].as_str() == Some(base))
        });
        if !confirmed {
            let owned = options
                .password_entry_fields(instance, base)
                .any(|field| record.get(&field.key).and_then(Value::as_str) == Some(base));
            if owned {
                return Ok((base.to_owned(), self.load_password_entry(base).await?));
            }
            let missing = self
                .passwords()?
                .missing_entries(HashSet::from([base.to_owned()]))
                .await
                .map_err(|error| ConfigServiceError::Storage(error.to_string()))?;
            if missing.contains(base) {
                return Ok((base.to_owned(), None));
            }
        }
        let (prefix, name) = pending_password_location(base);
        let path = format!("{prefix}{id}/{name}");
        let backup = self.load_password_entry(&path).await?;
        Ok((path, backup))
    }

    async fn stage_password_lines(
        &self,
        options: &ConfigItemOptions,
        instance: &str,
        base: &str,
        path: &str,
        changes: &BTreeMap<&str, Option<&str>>,
        record: &mut serde_json::Map<String, Value>,
    ) -> Result<Option<String>, ConfigServiceError> {
        let mut entries = BTreeMap::<String, Option<String>>::new();
        let mut first = String::new();
        let mut named = Vec::new();
        let mut populated = false;
        for field in options.password_entry_fields(instance, base) {
            let secret = if let Some(replacement) = changes.get(field.key.as_str()) {
                replacement.map(str::to_owned)
            } else if let Some(previous) = record.get(&field.key).and_then(Value::as_str) {
                if !options.matches_password_path(&field.key, instance, previous) {
                    return Err(ConfigServiceError::ActionNotFound);
                }
                if !entries.contains_key(previous) {
                    entries.insert(
                        previous.to_owned(),
                        self.load_password_entry(previous).await?,
                    );
                }
                entries[previous]
                    .as_deref()
                    .map(|contents| options.read_password_line(&field.key, contents))
                    .transpose()?
            } else {
                None
            };
            if let Some(secret) = secret {
                populated = true;
                record.insert(field.key.clone(), Value::String(path.to_owned()));
                match field.data_type.password_key() {
                    None => first = secret,
                    Some(key) => named.push(format!("{key}: {secret}")),
                }
            } else {
                record.remove(&field.key);
            }
        }
        Ok(populated.then(|| {
            for line in named {
                first.push('\n');
                first.push_str(&line);
            }
            first
        }))
    }

    async fn prepare_password_changes(
        &self,
        entry: &mut Pending,
    ) -> Result<Vec<PasswordChange>, ConfigServiceError> {
        let Some(instance) = entry.instance.as_deref() else {
            return Ok(Vec::new());
        };
        let options = item_options(&entry.module, &entry.item);
        let mut changes = BTreeMap::<String, PasswordChange>::new();
        for key in &entry.removed_password_entries {
            let base = options.password_path(key, instance)?;
            if options
                .password_field(key)
                .is_none_or(|field| field.data_type.password_key().is_some())
                || options
                    .password_entry_fields(instance, &base)
                    .any(|field| entry.value.get(&field.key).is_some())
            {
                return Err(ConfigServiceError::ActionNotFound);
            }
            let previous = match self.passwords()?.fetch(&base).await {
                Ok(contents) => Some(contents),
                Err(PasswordError::EntryNotFound) => None,
                Err(error) => return Err(ConfigServiceError::Storage(error.to_string())),
            };
            changes.insert(
                base.clone(),
                PasswordChange {
                    path: base,
                    contents: None,
                    previous,
                },
            );
        }
        for field in options
            .structure
            .iter()
            .flat_map(|structure| &structure.fields)
        {
            if !field.data_type.is_password() {
                continue;
            }
            let Some(path) = entry.value[&field.key].as_str() else {
                continue;
            };
            let base = options.password_path(&field.key, instance)?;
            if path == base {
                continue;
            }
            if !options.matches_password_path(&field.key, instance, path) {
                return Err(ConfigServiceError::ActionNotFound);
            }
            if !changes.contains_key(&base) {
                let store = self.passwords()?;
                let contents = store
                    .fetch(path)
                    .await
                    .map_err(|error| ConfigServiceError::Storage(error.to_string()))?;
                let previous = match store.fetch(&base).await {
                    Ok(contents) => Some(contents),
                    Err(PasswordError::EntryNotFound) => None,
                    Err(error) => return Err(ConfigServiceError::Storage(error.to_string())),
                };
                changes.insert(
                    base.clone(),
                    PasswordChange {
                        path: base.clone(),
                        contents: Some(contents),
                        previous,
                    },
                );
            }
            entry.value[&field.key] = Value::String(base);
        }
        Ok(changes.into_values().collect())
    }

    async fn persist_password_changes(
        &self,
        values: StoredValues,
        changes: &[PasswordChange],
    ) -> Result<(), ConfigServiceError> {
        if changes.is_empty() {
            return self.persist_notifications(values).await;
        }
        let store = self.passwords()?;
        let mut attempted = 0;
        let result = async {
            for change in changes {
                attempted += 1;
                let result = match &change.contents {
                    Some(contents) => {
                        store
                            .save_multiline(
                                &change.path,
                                contents.strip_suffix('\n').unwrap_or(contents),
                            )
                            .await
                    }
                    None if change.previous.is_some() => store.remove(&change.path).await,
                    None => Ok(()),
                };
                result.map_err(|error| ConfigServiceError::Storage(error.to_string()))?;
            }
            self.persist_notifications(values).await
        }
        .await;
        if result.is_err() {
            for change in changes[..attempted].iter().rev() {
                let restored = match &change.previous {
                    Some(previous) => {
                        store
                            .save_multiline(
                                &change.path,
                                previous.strip_suffix('\n').unwrap_or(previous),
                            )
                            .await
                    }
                    None if change.contents.is_some() => store.remove(&change.path).await,
                    None => Ok(()),
                };
                if let Err(error) = restored {
                    log::warn!("恢复密码条目失败：{error}");
                }
            }
        }
        result
    }

    async fn deliver(&self, entry: &mut Pending) -> Result<Outcome, ConfigServiceError> {
        let method = if entry.delete {
            Method::DELETE
        } else {
            Method::POST
        };
        let (completion, listener) = tokio::sync::watch::channel(false);
        let response = self.dispatch_notification(entry, method, listener).await;
        if let Some(error) = response
            .as_ref()
            .filter(|response| response.status() == StatusCode::UNPROCESSABLE_ENTITY)
            .and_then(|response| response.extensions().get::<NotificationValidationError>())
        {
            let mut error = error.clone();
            let options = item_options(&entry.module, &entry.item);
            error
                .clear_password_fields
                .retain(|field| options.password_field(field).is_some());
            let mut rejected = entry.clone();
            if let Some(record) = rejected.value.as_object_mut() {
                for field in &error.clear_password_fields {
                    record.remove(field);
                }
            }
            rejected.validation_error = Some(error.clone());
            let service = self.clone();
            let pending = entry.clone();
            let rejection = error.clone();
            self.commits
                .run(async move {
                    let credentials = service.record_credentials.clone();
                    credentials
                        .run(async move {
                            let entry = &pending;
                            let mut values = service.stored_values().await;
                            let stored = values
                                .get_mut(JOURNAL)
                                .and_then(|entries| entries.get_mut(&entry.id.to_string()))
                                .ok_or(ConfigServiceError::PendingNotFound)?;
                            *stored = json!(rejected);
                            service.persist_notifications(values).await?;
                            if !rejection.clear_password_fields.is_empty() {
                                service
                                    .discard_notification_passwords(
                                        &entry,
                                        &entry.value,
                                        &[Some(&rejected.value), entry.previous.as_ref()],
                                    )
                                    .await;
                            }

                            Ok(())
                        })
                        .await
                })
                .await?;
            return Err(ConfigServiceError::PendingValidation(error));
        }
        let result = match response.as_ref().map(|response| response.status()) {
            Some(StatusCode::OK | StatusCode::NO_CONTENT) => Outcome::Confirmed,
            Some(status) if status.is_client_error() || status.is_server_error() => Outcome::Failed,
            _ => Outcome::Processing,
        };
        if result == Outcome::Processing {
            return Ok(result);
        }
        if let Some(fields) = response
            .as_ref()
            .filter(|_| result == Outcome::Confirmed)
            .and_then(|response| response.extensions().get::<NotificationConfirmedFields>())
        {
            self.apply_confirmed_fields(entry, &fields.0).await?;
        }
        let service = self.clone();
        let pending = entry.clone();
        *entry = self
            .commits
            .run(async move {
                let credentials = service.record_credentials.clone();
                credentials
                    .run(async move {
                        let mut pending = pending;
                        let entry = &mut pending;
                        let mut values = service.stored_values().await;
                        let id = entry.id.to_string();
                        if !values
                            .get(JOURNAL)
                            .is_some_and(|entries| entries.contains_key(&id))
                        {
                            return Err(ConfigServiceError::PendingNotFound);
                        }
                        let staged = entry.value.clone();
                        let password_changes = if result == Outcome::Confirmed && !entry.delete {
                            service.prepare_password_changes(entry).await?
                        } else {
                            Vec::new()
                        };
                        values.get_mut(JOURNAL).expect("待确认操作存在").remove(&id);
                        if result == Outcome::Confirmed {
                            let entries = values.entry(entry.module.clone()).or_default();
                            if let Some(instance) = &entry.instance {
                                let options = item_options(&entry.module, &entry.item);
                                let key_field = options.notification_key_field()?;
                                let default = service
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
                        service
                            .persist_password_changes(values, &password_changes)
                            .await?;
                        if let Some(previous) =
                            entry.previous.as_mut().and_then(Value::as_object_mut)
                        {
                            previous.retain(|_, value| {
                                !password_changes.iter().any(|change| {
                                    change.contents.is_none()
                                        && value.as_str() == Some(&change.path)
                                })
                            });
                        }
                        service.clean_notification_passwords(entry, &result).await;
                        if password_changes
                            .iter()
                            .any(|change| change.contents.is_some())
                        {
                            service
                                .discard_notification_passwords(
                                    entry,
                                    &staged,
                                    &[Some(&entry.value)],
                                )
                                .await;
                        }

                        Ok(pending)
                    })
                    .await
            })
            .await?;
        if result == Outcome::Confirmed {
            let _ = completion.send(true);
        }
        Ok(result)
    }

    async fn apply_confirmed_fields(
        &self,
        entry: &mut Pending,
        fields: &serde_json::Map<String, Value>,
    ) -> Result<(), ConfigServiceError> {
        let reject = || ConfigServiceError::ChangeRejected {
            module_id: entry.module.clone(),
            key: entry.item.clone(),
            message: "业务模块返回了无效的只读配置字段。".into(),
        };
        let options = item_options(&entry.module, &entry.item);
        let structure = options.structure.as_ref().ok_or_else(reject)?;
        let valid = entry.instance.is_some()
            && !entry.delete
            && fields.keys().all(|key| {
                structure.fields.iter().any(|field| {
                    field.key == *key
                        && field.read_only
                        && !field.data_type.is_password()
                        && structure.id_field.as_ref() != Some(key)
                })
            });
        if !valid {
            return Err(reject());
        }
        let mut record = entry.value.as_object().cloned().ok_or_else(reject)?;
        record.extend(fields.clone());
        let value = Value::Object(record);
        let metadata = self.module_metadata(&entry.module).await?;
        metadata
            .item(&entry.item)
            .ok_or_else(reject)?
            .validate_with(&options, &json!([value]))
            .map_err(|_| reject())?;
        entry.value = value;
        Ok(())
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
        let fields: Vec<_> = options
            .structure
            .iter()
            .flat_map(|structure| &structure.fields)
            .filter(|field| field.data_type.is_password())
            .collect();
        let kept_paths: HashSet<_> = retained
            .iter()
            .flatten()
            .flat_map(|record| {
                fields
                    .iter()
                    .filter_map(|field| record[&field.key].as_str())
            })
            .collect();
        let mut removed = HashSet::new();
        for field in fields {
            if let Some(path) = discarded[&field.key].as_str()
                && !kept_paths.contains(path)
                && options.matches_password_path(&field.key, instance, path)
                && removed.insert(path)
                && let Err(error) = store.remove(path).await
            {
                log::warn!("清理配置凭据失败：{error}");
            }
        }
    }

    async fn dispatch_notification(
        &self,
        entry: &Pending,
        method: Method,
        completion: tokio::sync::watch::Receiver<bool>,
    ) -> Option<axum::response::Response> {
        let dispatcher = self.notification_router.get()?;
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
                    .filter(|field| field.data_type.is_password())
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
                password_entry_removed: !entry.removed_password_entries.is_empty(),
                completion,
            })
            .body(body)
            .ok()?;
        let delivery = dispatcher
            .call(move |router| {
                let router = router.upgrade()?;
                Some(tokio::spawn(async move {
                    tokio::time::timeout(
                        Duration::from_secs(15),
                        (*router).clone().oneshot(request),
                    )
                    .await
                    .ok()?
                    .ok()
                }))
            })
            .await?;
        delivery.await.ok()?
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

pub(super) fn clear_retired_pending_password_references(values: &mut StoredValues) {
    let Some(journal) = values.get_mut(JOURNAL) else {
        return;
    };
    for value in journal.values_mut() {
        let Ok(mut entry) = serde_json::from_value::<Pending>(value.clone()) else {
            continue;
        };
        let options = item_options(&entry.module, &entry.item);
        let changed = options.clear_retired_password_references(&mut entry.value, true);
        if changed {
            entry.validation_error = None;
        }
        let previous_changed = entry
            .previous
            .as_mut()
            .is_some_and(|previous| options.clear_retired_password_references(previous, false));
        if changed || previous_changed {
            *value = json!(entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Mutex as SyncMutex,
        atomic::{AtomicBool, Ordering},
    };

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
