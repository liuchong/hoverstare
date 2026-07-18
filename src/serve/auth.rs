//! App 认证：JWT 签发 + 安装令牌交换与缓存（spec 10）

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dashmap::DashMap;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use secrecy::SecretString;
use serde::Serialize;

const TOKEN_TTL: Duration = Duration::from_secs(3600);
/// 提前 10 分钟刷新
const REFRESH_MARGIN: Duration = Duration::from_secs(600);

#[derive(Debug, thiserror::Error)]
pub enum ServeAuthError {
    #[error("private key 读取/解析失败: {0}")]
    Key(String),
    #[error("JWT 签发失败: {0}")]
    Jwt(#[from] jsonwebtoken::errors::Error),
    #[error("安装令牌请求失败: {0}")]
    Http(#[from] reqwest::Error),
    #[error("安装令牌响应异常 {status}: {body}")]
    Api { status: u16, body: String },
}

#[derive(Serialize)]
struct Claims {
    iss: String,
    iat: u64,
    exp: u64,
}

struct CacheEntry {
    token: SecretString,
    expires_at: Instant,
}

pub struct AppAuth {
    app_id: String,
    key: EncodingKey,
    api: String,
    http: reqwest::Client,
    cache: DashMap<u64, CacheEntry>,
}

impl AppAuth {
    pub fn new(app_id: String, pem: &str) -> Result<Arc<AppAuth>, ServeAuthError> {
        let key = EncodingKey::from_rsa_pem(pem.as_bytes())
            .map_err(|e| ServeAuthError::Key(e.to_string()))?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("hoverstare/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let api = std::env::var("GITHUB_API_URL")
            .unwrap_or_else(|_| "https://api.github.com".to_string());
        Ok(Arc::new(AppAuth {
            app_id,
            key,
            api,
            http,
            cache: DashMap::new(),
        }))
    }

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    /// App JWT（RS256，iss=app_id，iat-60s，exp+9min，spec 10）
    pub fn jwt(&self) -> Result<String, ServeAuthError> {
        let now = Self::now();
        let claims = Claims {
            iss: self.app_id.clone(),
            iat: now.saturating_sub(60),
            exp: now + 9 * 60,
        };
        Ok(jsonwebtoken::encode(
            &Header::new(Algorithm::RS256),
            &claims,
            &self.key,
        )?)
    }

    /// 取某 installation 的访问令牌（进程内缓存，提前 10 分钟刷新）
    pub async fn installation_token(
        &self,
        installation_id: u64,
    ) -> Result<SecretString, ServeAuthError> {
        if let Some(entry) = self.cache.get(&installation_id)
            && Instant::now() + REFRESH_MARGIN < entry.expires_at
        {
            return Ok(entry.token.clone());
        }

        let jwt = self.jwt()?;
        let url = format!(
            "{}/app/installations/{installation_id}/access_tokens",
            self.api
        );
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&jwt)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await?;
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await?;
        if !(200..300).contains(&status) {
            return Err(ServeAuthError::Api {
                status,
                body: body.to_string(),
            });
        }
        let token = body["token"].as_str().unwrap_or_default().to_string();
        if token.is_empty() {
            return Err(ServeAuthError::Api {
                status,
                body: "响应缺少 token 字段".into(),
            });
        }
        let secret = SecretString::from(token);
        self.cache.insert(
            installation_id,
            CacheEntry {
                token: secret.clone(),
                expires_at: Instant::now() + TOKEN_TTL,
            },
        );
        Ok(secret)
    }
}
