use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::{
    ConfigService, ConfigServiceError, StoredValues,
    notifications::{JOURNAL, NotificationValidationError, Pending, pending},
    system,
};
use crate::modules::config::{registry::item_options, storage::FileStorage};

const FORMAT_VERSION: u32 = 2;
const REVISIONS: &str = "_accepted_config_revisions";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::modules::config) struct Snapshot {
    format_version: u32,
    modules: StoredValues,
    #[serde(default)]
    monitor_states: std::collections::BTreeMap<String, Vec<MonitorState>>,
    #[serde(rename = "_system", default)]
    system: Map<String, Value>,
}

#[derive(Clone, Serialize, Deserialize)]
struct MonitorState {
    monitor: String,
    item: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    related_items: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    unset_items: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    removed_items: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    instance: Option<String>,
    revision: Uuid,
    status: MonitorStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    record_operation: Option<RecordOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<NotificationValidationError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error_revision: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    removed_password_entries: Vec<String>,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MonitorStatus {
    Pending,
    Processing,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RecordOperation {
    Create,
    Delete,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionedField {
    accepted: Option<VersionedValue>,
    editing: Option<VersionedValue>,
}

#[derive(Serialize, Deserialize)]
struct VersionedValue {
    revision: Uuid,
    #[serde(flatten)]
    change: FieldChange,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum FieldChange {
    Set { value: Value },
    Remove { remove: bool },
}

impl VersionedValue {
    fn new(revision: Uuid, value: Option<&Value>) -> Self {
        let change = match value {
            Some(value) => FieldChange::Set {
                value: value.clone(),
            },
            None => FieldChange::Remove { remove: true },
        };
        Self { revision, change }
    }

    fn value(self) -> Option<Value> {
        match self.change {
            FieldChange::Set { value } => Some(value),
            FieldChange::Remove { .. } => None,
        }
    }
}

impl MonitorState {
    fn from_entry(entry: &Pending, monitor: &str, is_new_record: bool) -> Self {
        let record_operation = if entry.delete {
            Some(RecordOperation::Delete)
        } else if is_new_record {
            Some(RecordOperation::Create)
        } else {
            None
        };
        Self {
            monitor: monitor.into(),
            item: entry.item.clone(),
            unset_items: Vec::new(),
            related_items: entry
                .related
                .iter()
                .map(|entry| entry.item.clone())
                .collect(),
            removed_items: std::iter::once(entry)
                .chain(entry.related.iter())
                .filter(|entry| entry.instance.is_none() && entry.delete)
                .map(|entry| entry.item.clone())
                .collect(),
            instance: entry.instance.clone(),
            revision: entry.id,
            status: if entry.error_revision == Some(entry.id) {
                MonitorStatus::Failed
            } else {
                MonitorStatus::Pending
            },
            record_operation,
            error: entry.validation_error.clone(),
            error_revision: entry.error_revision,
            removed_password_entries: entry.removed_password_entries.clone(),
        }
    }

    fn to_entry(&self, module: &str, item: &str, value: Value, previous: Option<Value>) -> Pending {
        let key = self.instance.clone().unwrap_or_else(|| {
            value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string())
        });
        Pending {
            id: self.revision,
            module: module.into(),
            item: item.into(),
            instance: self.instance.clone(),
            key,
            value,
            previous,
            delete: if self.instance.is_some() {
                self.record_operation == Some(RecordOperation::Delete)
            } else {
                self.removed_items.iter().any(|removed| removed == item)
            },
            validation_error: self.error.clone(),
            error_revision: self.error_revision,
            removed_password_entries: self.removed_password_entries.clone(),
            related: Vec::new(),
        }
    }
}

impl Snapshot {
    pub(super) fn encode(mut values: StoredValues) -> Self {
        let system = values.remove(system::SECTION).unwrap_or_default();
        let groups = pending(&values);
        let edits: Vec<_> = groups
            .iter()
            .flat_map(|entry| std::iter::once(entry).chain(entry.related.iter()))
            .collect();
        values.remove(JOURNAL);
        let revisions = values.remove(REVISIONS).unwrap_or_default();
        let mut monitor_states = std::collections::BTreeMap::<String, Vec<MonitorState>>::new();

        for entry in &groups {
            let options = item_options(&entry.module, &entry.item);
            if let Some(monitor) = options.monitor {
                let mut state = MonitorState::from_entry(
                    entry,
                    monitor.id,
                    entry.instance.is_some() && entry.previous.is_none(),
                );
                state.unset_items = std::iter::once(entry)
                    .chain(entry.related.iter())
                    .filter(|edit| {
                        !values
                            .get(&edit.module)
                            .is_some_and(|items| items.contains_key(&edit.item))
                    })
                    .map(|edit| edit.item.clone())
                    .collect();
                monitor_states
                    .entry(entry.module.clone())
                    .or_default()
                    .push(state);
                values.entry(entry.module.clone()).or_default();
            }
        }

        for (module, items) in &mut values {
            let current: std::collections::HashSet<_> = items.keys().cloned().collect();
            for edit in edits.iter().filter(|edit| &edit.module == module) {
                items.entry(edit.item.clone()).or_insert_with(|| {
                    if edit.instance.is_some() {
                        json!([])
                    } else {
                        Value::Null
                    }
                });
            }
            for (item, value) in items {
                let options = item_options(module, item);
                let Some(monitor) = options.monitor else {
                    continue;
                };
                let item_edits: Vec<_> = edits
                    .iter()
                    .filter(|edit| &edit.module == module && &edit.item == item)
                    .copied()
                    .collect();
                if let Some(structure) = &options.structure
                    && let Some(key) = &structure.id_field
                    && let Some(records) = value.as_array_mut()
                {
                    let known: std::collections::HashSet<_> = records
                        .iter()
                        .filter_map(|record| record[key].as_str().map(str::to_owned))
                        .collect();
                    for record in records.iter_mut() {
                        let Some(instance) = record[key].as_str().map(str::to_owned) else {
                            continue;
                        };
                        let edit = item_edits
                            .iter()
                            .find(|edit| edit.instance.as_deref() == Some(&instance))
                            .copied();
                        *record = encode_fields(
                            module,
                            item,
                            Some(&instance),
                            record.as_object(),
                            edit,
                            &revisions,
                        );
                    }
                    for edit in item_edits.iter().filter(|edit| {
                        !edit.delete
                            && edit
                                .instance
                                .as_ref()
                                .is_some_and(|instance| !known.contains(instance))
                    }) {
                        records.push(encode_fields(
                            module,
                            item,
                            edit.instance.as_deref(),
                            None,
                            Some(edit),
                            &revisions,
                        ));
                    }
                } else if options.structure.is_some() && value.is_object() {
                    *value = encode_fields(
                        module,
                        item,
                        None,
                        value.as_object(),
                        item_edits.first().copied(),
                        &revisions,
                    );
                } else {
                    let edit = item_edits.first().copied();
                    let accepted = current.contains(item).then_some(&*value);
                    let revision = accepted_revision(&revisions, module, item, None, None);
                    *value = encode_field(
                        accepted,
                        revision,
                        edit.map(|edit| (edit.id, (!edit.delete).then_some(&edit.value))),
                    );
                }
                debug_assert!(!monitor.id.is_empty());
            }
        }

        Self {
            format_version: FORMAT_VERSION,
            modules: values,
            monitor_states,
            system,
        }
    }

    pub(in crate::modules::config) fn decode(self) -> Result<StoredValues, ConfigServiceError> {
        if self.format_version != FORMAT_VERSION {
            return Err(ConfigServiceError::Storage("不支持的配置文件版本。".into()));
        }
        let mut values = self.modules;
        let mut journal = Map::new();
        let mut candidates = std::collections::BTreeMap::<Uuid, Vec<Pending>>::new();
        let mut revisions = Map::new();
        for (module, items) in &mut values {
            let states = self
                .monitor_states
                .get(module)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let mut removed = Vec::new();
            for (item, value) in &mut *items {
                let options = item_options(module, item);
                let Some(monitor) = options.monitor else {
                    continue;
                };
                let item_states: Vec<_> = states
                    .iter()
                    .filter(|state| {
                        (state.item == *item || state.related_items.contains(item))
                            && state.monitor == monitor.id
                    })
                    .collect();
                if let Some(structure) = &options.structure
                    && let Some(key) = &structure.id_field
                    && let Some(records) = value.as_array_mut()
                {
                    let mut accepted_records = Vec::with_capacity(records.len());
                    for record in records.drain(..) {
                        let instance = record[key].as_str().ok_or_else(invalid_snapshot)?;
                        let state = item_states
                            .iter()
                            .find(|state| state.instance.as_deref() == Some(instance))
                            .copied();
                        let (accepted, candidate) = decode_fields(
                            module,
                            item,
                            Some(instance),
                            &record,
                            state,
                            &mut revisions,
                        )?;
                        let is_new = state.is_some_and(|state| {
                            state.record_operation == Some(RecordOperation::Create)
                        });
                        if let Some(state) = state {
                            let previous = (!is_new).then(|| accepted.clone());
                            let entry = state.to_entry(module, item, candidate, previous);
                            candidates.entry(state.revision).or_default().push(entry);
                        }
                        if !is_new {
                            accepted_records.push(accepted);
                        }
                    }
                    if accepted_records.is_empty()
                        && item_states
                            .iter()
                            .any(|state| state.unset_items.contains(item))
                    {
                        removed.push(item.clone());
                    } else {
                        *records = accepted_records;
                    }
                } else if options.structure.is_some()
                    && value.is_object()
                    && value.get("accepted").is_none()
                {
                    let state = item_states.first().copied();
                    let (accepted, candidate) =
                        decode_fields(module, item, None, value, state, &mut revisions)?;
                    if let Some(state) = state {
                        let previous = (accepted
                            .as_object()
                            .is_some_and(|fields| !fields.is_empty()))
                        .then(|| accepted.clone());
                        let entry = state.to_entry(module, item, candidate, previous);
                        candidates.entry(state.revision).or_default().push(entry);
                    }
                    *value = accepted;
                } else {
                    let slot = read_field(value, item_states.first().copied())?;
                    let accepted =
                        decode_accepted(slot.accepted, &mut revisions, module, item, None, None)?;
                    if let Some(state) = item_states.first() {
                        let candidate = slot
                            .editing
                            .and_then(VersionedValue::value)
                            .or_else(|| accepted.clone())
                            .ok_or_else(invalid_snapshot)?;
                        let entry = state.to_entry(module, item, candidate, accepted.clone());
                        candidates.entry(state.revision).or_default().push(entry);
                    }
                    if let Some(accepted) = accepted {
                        *value = accepted;
                    } else {
                        removed.push(item.clone());
                    }
                }
            }
            for item in removed {
                items.remove(&item);
            }
        }
        for (module, states) in &self.monitor_states {
            for state in states {
                if item_options(module, &state.item).monitor.is_none() {
                    continue;
                }
                let mut members = candidates
                    .remove(&state.revision)
                    .ok_or_else(invalid_snapshot)?;
                if members.len() != 1 + state.related_items.len() {
                    return Err(invalid_snapshot());
                }
                let index = members
                    .iter()
                    .position(|entry| entry.item == state.item)
                    .ok_or_else(invalid_snapshot)?;
                let mut primary = members.remove(index);
                primary.related = members;
                journal.insert(primary.id.to_string(), json!(primary));
            }
        }
        if !candidates.is_empty() {
            return Err(invalid_snapshot());
        }
        values.insert(JOURNAL.into(), journal);
        values.insert(REVISIONS.into(), revisions);
        values.insert(system::SECTION.into(), self.system);
        Ok(values)
    }
}

fn encode_fields(
    module: &str,
    item: &str,
    instance: Option<&str>,
    accepted: Option<&Map<String, Value>>,
    edit: Option<&Pending>,
    revisions: &Map<String, Value>,
) -> Value {
    let candidate = edit.and_then(|edit| edit.value.as_object());
    let mut result = accepted.or(candidate).cloned().unwrap_or_default();
    let options = item_options(module, item);
    let structure = options.structure.expect("字段级监控需要结构元数据");
    for field in structure
        .fields
        .iter()
        .filter(|field| field.monitor.is_some())
    {
        let before = accepted.and_then(|fields| fields.get(&field.key));
        let after = candidate.and_then(|fields| fields.get(&field.key));
        let revision = accepted_revision(revisions, module, item, instance, Some(&field.key));
        let editing = edit
            .filter(|edit| !edit.delete && before != after)
            .map(|edit| (edit.id, after));
        if before.is_some() || editing.is_some() {
            result.insert(field.key.clone(), encode_field(before, revision, editing));
        }
    }
    Value::Object(result)
}

fn decode_fields(
    module: &str,
    item: &str,
    instance: Option<&str>,
    value: &Value,
    state: Option<&MonitorState>,
    revisions: &mut Map<String, Value>,
) -> Result<(Value, Value), ConfigServiceError> {
    let mut accepted = value.as_object().ok_or_else(invalid_snapshot)?.clone();
    let mut candidate = accepted.clone();
    let options = item_options(module, item);
    let structure = options.structure.expect("字段级监控需要结构元数据");
    for field in structure
        .fields
        .iter()
        .filter(|field| field.monitor.is_some())
    {
        let Some(value) = accepted.remove(&field.key) else {
            continue;
        };
        candidate.remove(&field.key);
        let slot = read_field(&value, state)?;
        let current = decode_accepted(
            slot.accepted,
            revisions,
            module,
            item,
            instance,
            Some(&field.key),
        )?;
        let next = match slot.editing {
            Some(edit) => edit.value(),
            None => current.clone(),
        };
        if let Some(value) = current {
            accepted.insert(field.key.clone(), value);
        }
        if let Some(value) = next {
            candidate.insert(field.key.clone(), value);
        }
    }
    Ok((Value::Object(accepted), Value::Object(candidate)))
}

fn encode_field(
    accepted: Option<&Value>,
    revision: Uuid,
    editing: Option<(Uuid, Option<&Value>)>,
) -> Value {
    json!(VersionedField {
        accepted: accepted.map(|value| VersionedValue::new(revision, Some(value))),
        editing: editing.map(|(revision, value)| VersionedValue::new(revision, value)),
    })
}

fn read_field(
    value: &Value,
    state: Option<&MonitorState>,
) -> Result<VersionedField, ConfigServiceError> {
    let slot: VersionedField =
        serde_json::from_value(value.clone()).map_err(|_| invalid_snapshot())?;
    if slot.editing.as_ref().is_some_and(|edit| {
        state.is_none_or(|state| state.revision != edit.revision)
            || matches!(edit.change, FieldChange::Remove { remove: false })
    }) {
        return Err(invalid_snapshot());
    }
    Ok(slot)
}

fn decode_accepted(
    value: Option<VersionedValue>,
    revisions: &mut Map<String, Value>,
    module: &str,
    item: &str,
    instance: Option<&str>,
    field: Option<&str>,
) -> Result<Option<Value>, ConfigServiceError> {
    let Some(value) = value else { return Ok(None) };
    let FieldChange::Set { value: current } = value.change else {
        return Err(invalid_snapshot());
    };
    revisions.insert(
        revision_key(module, item, instance, field),
        json!(value.revision),
    );
    Ok(Some(current))
}

fn revision_key(module: &str, item: &str, instance: Option<&str>, field: Option<&str>) -> String {
    json!([module, item, instance, field]).to_string()
}

fn accepted_revision(
    revisions: &Map<String, Value>,
    module: &str,
    item: &str,
    instance: Option<&str>,
    field: Option<&str>,
) -> Uuid {
    revisions
        .get(&revision_key(module, item, instance, field))
        .and_then(Value::as_str)
        .and_then(|revision| Uuid::parse_str(revision).ok())
        .unwrap_or_else(Uuid::nil)
}

fn invalid_snapshot() -> ConfigServiceError {
    ConfigServiceError::Storage("配置文件中的编辑状态无效。".into())
}

pub(super) fn record_acceptance(values: &mut StoredValues, entry: &Pending) {
    let revisions = values.entry(REVISIONS.into()).or_default();
    let options = item_options(&entry.module, &entry.item);
    let fields = options
        .structure
        .iter()
        .flat_map(|structure| &structure.fields)
        .filter(|field| field.monitor.is_some());
    if options.structure.is_some() {
        for field in fields {
            if entry
                .previous
                .as_ref()
                .and_then(|record| record.get(&field.key))
                != entry.value.get(&field.key)
            {
                revisions.insert(
                    revision_key(
                        &entry.module,
                        &entry.item,
                        entry.instance.as_deref(),
                        Some(&field.key),
                    ),
                    json!(entry.id),
                );
            }
        }
    } else {
        revisions.insert(
            revision_key(&entry.module, &entry.item, None, None),
            json!(entry.id),
        );
    }
}

impl ConfigService {
    pub(crate) fn update_storage(
        path: impl AsRef<std::path::Path>,
        update: impl FnOnce(&mut Value) -> Result<bool, ConfigServiceError>,
    ) -> Result<bool, ConfigServiceError> {
        let storage = FileStorage::new(path)?;
        let Some(snapshot) = storage.read()? else {
            return Ok(false);
        };
        let mut document = json!(snapshot);
        if !update(&mut document)? {
            return Ok(false);
        }
        let snapshot: Snapshot =
            serde_json::from_value(document).map_err(|_| invalid_snapshot())?;
        snapshot.clone().decode()?;
        storage.back_up()?;
        storage.write_atomic(snapshot)?;
        Ok(true)
    }

    pub fn migrate_storage(path: impl AsRef<std::path::Path>) -> Result<bool, ConfigServiceError> {
        let storage = FileStorage::new(path)?;
        let Some(legacy) = storage.read_legacy()? else {
            return Ok(false);
        };
        let snapshot = Snapshot::encode(legacy.clone());
        let mut restored = snapshot.decode()?;
        restored.remove(REVISIONS);
        let mut original = legacy;
        for section in [system::SECTION, JOURNAL] {
            if restored.get(section).is_some_and(Map::is_empty) && !original.contains_key(section) {
                restored.remove(section);
            }
            if original.get(section).is_some_and(Map::is_empty) && !restored.contains_key(section) {
                original.remove(section);
            }
        }
        if restored != original {
            return Err(ConfigServiceError::Storage(
                "转换后的配置与原文件不一致。".into(),
            ));
        }
        storage.back_up()?;
        storage.write_atomic(Snapshot::encode(original))?;
        Ok(true)
    }

    pub(crate) async fn editing_snapshot(&self, module: &str) -> Result<Value, ConfigServiceError> {
        let metadata = self.module_metadata(module).await?;
        let mut snapshot = Snapshot::encode(self.stored_values().await);
        let mut data = snapshot.modules.remove(module).unwrap_or_default();
        data.retain(|key, _| metadata.item(key).is_some());
        for (key, value) in &mut data {
            let options = item_options(module, key);
            if let Some(structure) = &options.structure {
                let redact = |record: &mut Value| {
                    for field in structure
                        .fields
                        .iter()
                        .filter(|field| field.data_type.is_password())
                    {
                        if let Some(slot) = record.get_mut(&field.key) {
                            if field.monitor.is_some() {
                                for state in ["accepted", "editing"] {
                                    if let Some(value) =
                                        slot.get_mut(state).and_then(|slot| slot.get_mut("value"))
                                    {
                                        *value = json!({"configured": true});
                                    }
                                }
                            } else {
                                *slot = json!({"configured": true});
                            }
                        }
                    }
                };
                if let Some(records) = value.as_array_mut() {
                    records.iter_mut().for_each(redact);
                } else {
                    redact(value);
                }
            } else if metadata
                .item(key)
                .is_some_and(|item| item.data_type.is_password())
            {
                *value = json!({"configured": true});
            }
        }
        let mut monitor_states = snapshot.monitor_states.remove(module).unwrap_or_default();
        for state in &mut monitor_states {
            state.removed_password_entries.clear();
            state.unset_items.clear();
        }
        Ok(json!({
            "module_id": module,
            "data": data,
            "monitor_states": monitor_states,
        }))
    }
}
