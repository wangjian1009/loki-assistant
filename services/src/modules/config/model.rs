use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 配置值支持的基础数据类型。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigDataType {
    String,
    Password,
    Integer,
    Number,
    Boolean,
    Dictionary,
    Structure,
    PasswordReference,
    List,
    FilePath,
    Executable,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigValueSource {
    User,
    Default,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ConfigValue {
    pub value: Value,
    pub source: ConfigValueSource,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ConfigModuleMetadata {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub items: Vec<ConfigItemMetadata>,
}

impl ConfigModuleMetadata {
    pub fn item(&self, key: &str) -> Option<&ConfigItemMetadata> {
        self.items.iter().find(|item| item.key == key)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ConfigItemMetadata {
    pub key: String,
    pub name: String,
    pub description: String,
    pub data_type: ConfigDataType,
    #[serde(default)]
    pub remote_access: ConfigRemoteAccess,
    pub required: bool,
    pub default: Option<Value>,
    pub dictionary: Option<DictionaryMetadata>,
    pub list: Option<ListMetadata>,
    pub checker: Option<CheckerMetadata>,
    pub file_path: Option<FilePathMetadata>,
    pub executable: Option<ExecutableMetadata>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ConfigItemOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structure: Option<StructureMetadata>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ConfigAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub save_timeout_seconds: Option<u64>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub requires_local_auth: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigRemoteAccess {
    #[default]
    Local,
    Read,
    ReadWrite,
}

impl ConfigRemoteAccess {
    pub fn allows_read(self) -> bool {
        self != Self::Local
    }

    pub fn allows_write(self) -> bool {
        self == Self::ReadWrite
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DictionaryMetadata {
    pub entries: Vec<DictionaryEntry>,
    pub allow_unknown: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DictionaryEntry {
    pub value: Value,
    pub label: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ListMetadata {
    pub item_type: ConfigDataType,
    pub min_items: Option<usize>,
    pub max_items: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CheckerMetadata {
    pub pattern: String,
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FilePathMetadata {
    pub default: Option<String>,
    pub must_exist: bool,
    pub directory: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ExecutableMetadata {
    pub default_program: String,
    /// PATH 之后依次搜索的目录；由所属模块声明，不执行 shell 展开。
    #[serde(default)]
    pub search_paths: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct StructureMetadata {
    pub id_field: Option<String>,
    pub label_field: Option<String>,
    pub add_label: Option<String>,
    #[serde(skip_serializing, default)]
    pub legacy_dictionary: Option<LegacyDictionary>,
    pub type_name: String,
    pub fields: Vec<StructureField>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct LegacyDictionary {
    pub key: String,
    pub previous_id_field: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct StructureField {
    pub key: String,
    pub label: String,
    pub data_type: ConfigDataType,
    pub required: bool,
    pub default_template: Option<String>,
    pub checker: Option<CheckerMetadata>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_path: Option<String>,
    pub options: Option<FieldOptions>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FieldOptions {
    pub path: String,
    pub items_field: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ConfigAction {
    pub id: String,
    pub label: String,
    #[serde(flatten)]
    pub interaction: ActionInteraction,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "interaction", rename_all = "snake_case")]
pub enum ActionInteraction {
    Request,
    Password { binding: PasswordBinding },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PasswordBinding {
    pub key: String,
    pub instance_parameter: String,
    pub value_parameter: String,
}

/// 配置模块向业务模块提供元数据的接口。
pub trait ConfigMetadataProvider {
    fn config_metadata() -> ConfigModuleMetadata;
}
