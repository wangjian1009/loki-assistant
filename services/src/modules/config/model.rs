use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 配置值支持的基础数据类型。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigDataType {
    String,
    Integer,
    Number,
    Boolean,
    Dictionary,
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
    pub required: bool,
    pub default: Option<Value>,
    pub dictionary: Option<DictionaryMetadata>,
    pub list: Option<ListMetadata>,
    pub checker: Option<CheckerMetadata>,
    pub file_path: Option<FilePathMetadata>,
    pub executable: Option<ExecutableMetadata>,
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

/// 配置模块向业务模块提供元数据的接口。
pub trait ConfigMetadataProvider {
    fn config_metadata() -> ConfigModuleMetadata;
}
