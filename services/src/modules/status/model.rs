use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct StatusModuleMetadata {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub items: Vec<StatusItemMetadata>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct StatusItemMetadata {
    pub key: String,
    pub name: String,
    pub description: String,
    pub remote_access: bool,
    pub schema: StatusSchema,
}

/// 独立描述状态值的结构，不依赖配置值类型或当前是否已经发布数据。
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "data_type", rename_all = "snake_case")]
pub enum StatusSchema {
    String {
        #[serde(skip_serializing_if = "Vec::is_empty")]
        choices: Vec<StatusChoiceMetadata>,
    },
    Integer,
    Number,
    Boolean,
    Null,
    Object {
        fields: Vec<StatusFieldMetadata>,
    },
    Array {
        items: Box<StatusSchema>,
    },
    Any,
}

impl StatusSchema {
    pub fn string() -> Self {
        Self::String {
            choices: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct StatusFieldMetadata {
    pub key: String,
    pub name: String,
    pub description: String,
    pub required: bool,
    pub nullable: bool,
    pub schema: StatusSchema,
}

impl StatusFieldMetadata {
    pub fn new(key: &str, name: &str, description: &str, schema: StatusSchema) -> Self {
        Self {
            key: key.to_owned(),
            name: name.to_owned(),
            description: description.to_owned(),
            required: true,
            nullable: false,
            schema,
        }
    }

    pub fn nullable(mut self) -> Self {
        self.nullable = true;
        self
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct StatusChoiceMetadata {
    pub value: Value,
    pub label: String,
    pub description: String,
}
