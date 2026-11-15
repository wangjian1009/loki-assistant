use serde::Serialize;

/// 业务模块提供的完整元数据。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ModuleMetadata {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
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
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
}

/// 模块对外提供的一个 HTTP API 描述。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ApiMetadata {
    pub id: String,
    pub method: String,
    pub path: String,
    pub summary: String,
    pub description: String,
    pub parameters: Vec<ParameterMetadata>,
    pub request: Option<DataMetadata>,
    pub response: Option<DataMetadata>,
}

impl ApiMetadata {
    pub fn get(id: &str, path: &str, summary: &str, description: &str) -> Self {
        Self::new(id, "GET", path, summary, description)
    }

    pub fn new(id: &str, method: &str, path: &str, summary: &str, description: &str) -> Self {
        Self {
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

    pub fn with_parameters(mut self, parameters: Vec<ParameterMetadata>) -> Self {
        self.parameters = parameters;
        self
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ParameterMetadata {
    pub name: String,
    pub location: String,
    pub type_name: String,
    pub required: bool,
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
    pub name: String,
    pub fields: Vec<FieldMetadata>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct FieldMetadata {
    pub name: String,
    pub type_name: String,
    pub required: bool,
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
