use serde::{Deserialize, Serialize};

pub const API_VERSION: &str = "1";

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListRequest {
    #[serde(default)]
    pub query: String,
}

#[derive(Serialize)]
pub struct EntriesResponse {
    pub api_version: String,
    pub files: Vec<String>,
}

#[derive(Serialize)]
pub struct EntryResponse {
    pub api_version: String,
    pub file: String,
    pub contents: String,
}
