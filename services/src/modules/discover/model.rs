use serde::Serialize;

/// 业务模块提供的完整元数据。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ModuleMetadata {
    /// 模块标识，用于填充路径中的 `{module_id}`。
    pub id: String,
    /// 模块显示名称。
    pub name: String,
    /// 模块职责说明。
    pub description: String,
    /// 模块声明的版本。
    pub version: String,
    /// 当前请求方可访问的 HTTP API 列表。
    pub apis: Vec<ApiMetadata>,
}

impl ModuleMetadata {
    pub fn summary(&self) -> ModuleSummary {
        ModuleSummary {
            id: self.id.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            version: self.version.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ModuleSummary {
    /// 模块标识，用于查询模块详情。
    pub id: String,
    /// 模块显示名称。
    pub name: String,
    /// 模块职责说明。
    pub description: String,
    /// 模块声明的版本。
    pub version: String,
}

/// 模块对外提供的一个 HTTP API 描述。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ApiMetadata {
    /// 访问范围。`local` 限本机，`paired` 允许本机和已绑定设备调用。
    pub access: String,
    /// API 在所属模块中的标识。
    pub id: String,
    /// HTTP 请求方法，例如 `GET`。
    pub method: String,
    /// HTTP 路径模板，例如 `/meta/{module_id}`。
    pub path: String,
    /// API 功能的简短标题。
    pub summary: String,
    /// API 用途与行为说明。
    pub description: String,
    /// 该 API 声明的请求参数。
    pub parameters: Vec<ParameterMetadata>,
    /// 请求体的结构描述。未提供时序列化为 `null`。
    pub request: Option<DataMetadata>,
    /// 响应体的结构描述。未提供时序列化为 `null`。
    pub response: Option<DataMetadata>,
}

impl ApiMetadata {
    pub fn get(id: &str, path: &str, summary: &str, description: &str) -> Self {
        Self::new(id, "GET", path, summary, description)
    }

    pub fn new(id: &str, method: &str, path: &str, summary: &str, description: &str) -> Self {
        Self {
            access: "local".to_owned(),
            id: id.to_owned(),
            method: method.to_owned(),
            path: path.to_owned(),
            summary: summary.to_owned(),
            description: description.to_owned(),
            parameters: Vec::new(),
            request: None,
            response: None,
        }
    }

    pub fn with_response(mut self, response: DataMetadata) -> Self {
        self.response = Some(response);
        self
    }

    pub fn with_access(mut self, access: &str) -> Self {
        self.access = access.to_owned();
        self
    }

    pub fn with_parameters(mut self, parameters: Vec<ParameterMetadata>) -> Self {
        self.parameters = parameters;
        self
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ParameterMetadata {
    /// 参数名称。
    pub name: String,
    /// 参数在 HTTP 请求中的位置，例如 `path`。
    pub location: String,
    /// 参数值的类型名称，例如 `string`。
    pub type_name: String,
    /// 发起请求时是否必须提供此参数。
    pub required: bool,
    /// 参数用途说明。
    pub description: String,
}

impl ParameterMetadata {
    pub fn path(name: &str, type_name: &str, description: &str) -> Self {
        Self {
            name: name.to_owned(),
            location: "path".to_owned(),
            type_name: type_name.to_owned(),
            required: true,
            description: description.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DataMetadata {
    /// 请求体或响应体的结构名称。
    pub name: String,
    /// 结构中声明的字段。
    pub fields: Vec<FieldMetadata>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct FieldMetadata {
    /// 请求体或响应体中的字段名。
    pub name: String,
    /// 字段值的类型名称。
    pub type_name: String,
    /// 数据中是否必须包含此字段。
    pub required: bool,
    /// 字段用途说明。
    pub description: String,
}

impl FieldMetadata {
    pub fn required(name: &str, type_name: &str, description: &str) -> Self {
        Self {
            name: name.to_owned(),
            type_name: type_name.to_owned(),
            required: true,
            description: description.to_owned(),
        }
    }
}
