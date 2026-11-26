use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Storage {
    Library,
    Daily,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Model {
    Studio,
    Original,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum CupType {
    #[serde(rename = "xpod")]
    XPod,
    Omni,
    Other,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Pattern {
    Center,
    Spiral,
    Circle,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Brewing {
    cup_type: Option<CupType>,
    dose_g: Option<f64>,
    water_ratio: Option<f64>,
    water_ml: Option<f64>,
    grinder_enabled: Option<bool>,
    grind_size: Option<f64>,
    rpm: Option<f64>,
    bypass_enabled: Option<bool>,
    bypass_ml: Option<f64>,
    bypass_temp_c: Option<f64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pour {
    volume_ml: Option<f64>,
    temperature_c: Option<f64>,
    flow_ml_s: Option<f64>,
    pattern: Option<Pattern>,
    pause_s: Option<f64>,
    vibrate_before: Option<bool>,
    vibrate_after: Option<bool>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RecipeData {
    title: String,
    url: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    model: Option<Model>,
    coffee: Option<String>,
    roaster: Option<String>,
    origin: Option<String>,
    process: Option<String>,
    flavor_notes: Option<String>,
    #[serde(default)]
    brewing: Brewing,
    #[serde(default)]
    pours: Vec<Pour>,
}

#[derive(Deserialize, Serialize)]
pub(super) struct Recipe {
    pub id: String,
    storage: Storage,
    table_id: Option<String>,
    region: Option<String>,
    source: Option<String>,
    saved_on: Option<String>,
    based_on_id: Option<String>,
    data: RecipeData,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Create {
    pub recipe: RecipeData,
    pub based_on_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Update {
    pub recipe: RecipeData,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Query {
    pub q: Option<String>,
}

#[derive(Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub(super) enum Request {
    List { q: Option<String> },
    Get { id: String },
    Create { info: Create },
    Update { id: String, recipe: RecipeData },
}

#[derive(Deserialize, Serialize)]
pub(super) struct List {
    recipes: Vec<Recipe>,
}

#[derive(Deserialize, Serialize)]
pub(super) struct Saved {
    pub recipe: Recipe,
    indexed: bool,
}

#[derive(Serialize)]
pub(super) struct Detail {
    pub recipe: Recipe,
}

#[derive(Serialize)]
pub(super) struct Response<T> {
    pub api_version: &'static str,
    #[serde(flatten)]
    pub result: T,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum Reply<T> {
    Success { result: T },
    Failure { error: Failure },
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Failure {
    InvalidRequest,
    RecipeNotFound,
    RecipeConflict,
    RecipeReadOnly,
    XbloomUnavailable,
    RecipeReadFailed,
    DailyTemplateUnavailable,
    DailyConflict,
    RecipeSaveFailed,
}
