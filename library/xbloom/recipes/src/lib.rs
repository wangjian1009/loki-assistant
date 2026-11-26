use std::collections::HashSet;
use std::future::Future;

use base64::{Engine, engine::general_purpose::STANDARD};
use rsa::{Pkcs1v15Encrypt, RsaPublicKey, pkcs8::DecodePublicKey, traits::PublicKeyParts};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use thiserror::Error;
use url::Url;

const PAGE_SIZE: usize = 100;
const PUBLIC_KEY: &str = include_str!("../cloud-public-key.pem");
const CREATED_ENDPOINT: &str = "tuMyTeaRecipeCreated.tuhtml";
const SHARED_ENDPOINT: &str = "tuMyRecipeShared.tuhtml";
const OFFICIAL_ENDPOINT: &str = "tuMyRecipeProduct.tuhtml";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecipeSource {
    Personal,
    Official,
    All,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Region {
    #[default]
    International,
    China,
}

impl Region {
    fn origin(self) -> &'static str {
        match self {
            Self::International => "https://client-api.xbloom.com",
            Self::China => "https://clientcn-api.xbloomcoffee.cn",
        }
    }

    fn language(self) -> u8 {
        match self {
            Self::International => 0,
            Self::China => 3,
        }
    }
}

/// 调用方使用系统 HTTP 客户端发送请求。实现需以错误返回非 2xx 响应。
pub trait HttpTransport {
    fn post(
        &self,
        url: &str,
        body: Vec<u8>,
    ) -> impl Future<Output = Result<Vec<u8>, String>> + Send;
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("输出配方失败：{0}")]
    Output(#[from] std::io::Error),
    #[error("网络请求失败：{0}")]
    Transport(String),
    #[error("xBloom {0} 返回了无效数据")]
    Response(&'static str),
    #[error("xBloom {operation} 失败（{code}）")]
    Api {
        operation: &'static str,
        code: String,
    },
    #[error("登录已过期，请重新登录")]
    SessionExpired,
    #[error("账号会话的地区与请求地区不一致")]
    RegionMismatch,
    #[error("配方列表分页没有推进（{list}，机型 {model}，第 {page} 页）")]
    Pagination {
        list: &'static str,
        model: u8,
        page: u32,
    },
    #[error("需要 xBloom H5 分享链接")]
    ShareUrl,
    #[error("xBloom 请求加密失败")]
    Encryption,
}

pub struct Session {
    member_id: u64,
    token: String,
    region: Region,
}

impl Session {
    pub fn member_id(&self) -> u64 {
        self.member_id
    }
}

/// 配方持有原始 JSON 字段。导出时保留服务端返回的段落、扩展字段和编号。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Recipe(pub Map<String, Value>);

impl Recipe {
    pub fn id(&self) -> Option<u64> {
        self.0.get("tableId").and_then(as_id)
    }

    pub fn name(&self) -> Option<&str> {
        self.0.get("theName").and_then(Value::as_str)
    }

    pub fn model(&self) -> Option<u64> {
        self.0.get("adaptedModel").and_then(as_id)
    }
}

pub struct Client<T> {
    transport: T,
    region: Region,
}

impl<T: HttpTransport> Client<T> {
    pub fn new(transport: T, region: Region) -> Self {
        Self { transport, region }
    }

    pub async fn login(&self, email: &str, password: &str) -> Result<Session, Error> {
        let mut body = self.envelope();
        body["email"] = json!(email);
        body["password"] = json!(password);
        let response = self
            .post("tMemberLogin.thtml", &body, self.region == Region::China)
            .await?;
        let token = response["token"]
            .as_str()
            .filter(|token| !token.is_empty())
            .ok_or(Error::Response("登录"))?;
        let member_id = as_id(&response["member"]["tableId"]).ok_or(Error::Response("登录"))?;
        Ok(Session {
            member_id,
            token: token.to_owned(),
            region: self.region,
        })
    }

    /// 下载账号自建和已保存的分享配方，遍历 Studio 和 Original 分区。
    pub async fn all_recipes(&self, session: &Session) -> Result<Vec<Recipe>, Error> {
        self.recipes(session, RecipeSource::Personal).await
    }

    /// 根据来源下载配方，遍历 Studio 和 Original 分区。
    pub async fn recipes(
        &self,
        session: &Session,
        source: RecipeSource,
    ) -> Result<Vec<Recipe>, Error> {
        let mut recipes = Vec::new();
        self.visit_recipes(session, source, |recipe| {
            recipes.push(recipe);
            Ok(())
        })
        .await?;
        Ok(recipes)
    }

    /// 每收到一个去重后的配方便调用 VISIT。
    pub async fn visit_recipes(
        &self,
        session: &Session,
        source: RecipeSource,
        mut visit: impl FnMut(Recipe) -> Result<(), std::io::Error>,
    ) -> Result<(), Error> {
        if session.region != self.region {
            return Err(Error::RegionMismatch);
        }
        let endpoints: &[&str] = match source {
            RecipeSource::Personal => &[CREATED_ENDPOINT, SHARED_ENDPOINT],
            RecipeSource::Official => &[OFFICIAL_ENDPOINT],
            RecipeSource::All => &[CREATED_ENDPOINT, SHARED_ENDPOINT, OFFICIAL_ENDPOINT],
        };
        let mut all_seen = HashSet::new();
        for &endpoint in endpoints {
            for model in [1, 2] {
                let mut seen = HashSet::new();
                for page in 1..=1000 {
                    let mut body = self.envelope();
                    body["memberId"] = json!(session.member_id);
                    body["token"] = json!(session.token);
                    body["adaptedModel"] = json!(model);
                    body["pageNumber"] = json!(page);
                    body["countPerPage"] = json!(PAGE_SIZE);
                    let response = self.post(endpoint, &body, true).await?;
                    let rows = match response.get("list") {
                        Some(Value::Array(rows)) => rows,
                        None | Some(Value::Null) if page == 1 => break,
                        _ => return Err(Error::Response(endpoint)),
                    };
                    let mut advanced = false;
                    for row in rows {
                        let fields = row.as_object().ok_or(Error::Response(endpoint))?;
                        let recipe = Recipe(fields.clone());
                        let id = recipe.id().ok_or(Error::Response(endpoint))?;
                        advanced |= seen.insert(id);
                        if all_seen.insert(id) {
                            visit(recipe)?;
                        }
                    }
                    if !rows.is_empty() && !advanced {
                        return Err(Error::Pagination {
                            list: endpoint,
                            model,
                            page,
                        });
                    }
                    if rows.len() < PAGE_SIZE {
                        break;
                    }
                    if page == 1000 {
                        return Err(Error::Pagination {
                            list: endpoint,
                            model,
                            page,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn shared_recipe(&self, link: &str) -> Result<Recipe, Error> {
        let link = Url::parse(link).map_err(|_| Error::ShareUrl)?;
        let share_region = match link.host_str() {
            Some("share-h5.xbloom.com") if link.scheme() == "https" => Region::International,
            Some("share-h5.xbloomcoffee.cn") if link.scheme() == "https" => Region::China,
            _ => return Err(Error::ShareUrl),
        };
        let id = link
            .query_pairs()
            .find_map(|(key, value)| (key == "id" && !value.is_empty()).then(|| value.into_owned()))
            .ok_or(Error::ShareUrl)?;
        let body = json!({"tableIdOfRSA": id, "interfaceVersion": 19700101, "skey": "testskey"});
        let response = self
            .post_at(share_region, "RecipeDetail.html", &body, false)
            .await?;
        let fields = response["recipeVo"]
            .as_object()
            .ok_or(Error::Response("配方详情"))?;
        Ok(Recipe(fields.clone()))
    }

    fn envelope(&self) -> Value {
        json!({
            "interfaceVersion": 20240918,
            "skey": "testskey",
            "languageType": self.region.language(),
            "clientType": 2,
            "phoneType": "Android"
        })
    }

    async fn post(
        &self,
        operation: &'static str,
        body: &Value,
        encrypted: bool,
    ) -> Result<Value, Error> {
        self.post_at(self.region, operation, body, encrypted).await
    }

    async fn post_at(
        &self,
        region: Region,
        operation: &'static str,
        body: &Value,
        encrypted: bool,
    ) -> Result<Value, Error> {
        let body = request_body(body, encrypted)?;
        let url = format!("{}/{}", region.origin(), operation);
        let reply = self
            .transport
            .post(&url, body)
            .await
            .map_err(Error::Transport)?;
        let reply: Value =
            serde_json::from_slice(&reply).map_err(|_| Error::Response(operation))?;
        if reply["result"] != "success" {
            let code = as_id(&reply["code"]).or_else(|| as_id(&reply["resultCode"]));
            if matches!(code, Some(10001 | 20003)) {
                return Err(Error::SessionExpired);
            }
            return Err(Error::Api {
                operation,
                code: code.map_or_else(|| "业务错误".to_owned(), |code| code.to_string()),
            });
        }
        Ok(reply)
    }
}

fn request_body(body: &Value, encrypted: bool) -> Result<Vec<u8>, Error> {
    let body = serde_json::to_vec(body).map_err(|_| Error::Encryption)?;
    if !encrypted {
        return Ok(body);
    }
    let key = RsaPublicKey::from_public_key_pem(PUBLIC_KEY).map_err(|_| Error::Encryption)?;
    let mut result = Vec::new();
    for chunk in body.chunks(key.size() - 11) {
        let encrypted = key
            .encrypt(&mut rand::rngs::OsRng, Pkcs1v15Encrypt, chunk)
            .map_err(|_| Error::Encryption)?;
        result.extend(encrypted);
    }
    serde_json::to_vec(&STANDARD.encode(result)).map_err(|_| Error::Encryption)
}

fn as_id(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}
