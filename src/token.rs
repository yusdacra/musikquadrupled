use rand::Rng;
use scc::{HashMap, HashSet};
use std::borrow::Cow;
use std::fmt::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use crate::error::AppError;

fn get_current_time() -> u64 {
    UNIX_EPOCH.elapsed().unwrap().as_secs()
}

fn hash_string(data: &[u8]) -> Result<String, argon2::Error> {
    argon2::hash_encoded(data, "11111111".as_bytes(), &argon2::Config::default())
}

fn generate_random_string(len: usize) -> String {
    rand::thread_rng()
        .sample_iter(rand::distributions::Alphanumeric)
        .take(len)
        .map(|c| c as char)
        .collect::<String>()
}

#[derive(Debug, Clone)]
pub(crate) struct Tokens {
    hashed: Arc<HashSet<Cow<'static, str>>>,
    raw_contents: &'static str,
}

impl Tokens {
    pub async fn read(path: impl AsRef<Path>) -> Result<Self, AppError> {
        let tokens = tokio::fs::read_to_string(path).await?;
        let this = Self {
            hashed: Arc::new(HashSet::new()),
            // this is okay since we only call this once and it will be
            // used for all of it's lifetime
            raw_contents: Box::leak(tokens.into_boxed_str()),
        };

        for token in this.raw_contents.lines() {
            let token = token.trim();
            if !token.is_empty() {
                this.hashed
                    .insert_async(Cow::Borrowed(token))
                    .await
                    .expect("the set will be empty");
            }
        }

        Ok(this)
    }

    pub async fn write(&self, path: impl AsRef<Path>) -> Result<(), AppError> {
        let mut contents = String::new();
        self.hashed
            .scan_async(|hash| {
                writeln!(&mut contents, "{hash}").expect("if this fails then too bad")
            })
            .await;

        tokio::fs::write(path, contents).await.map_err(Into::into)
    }

    pub async fn generate(&self) -> Result<String, AppError> {
        let token = generate_random_string(30);

        let token_hash = hash_string(token.as_bytes())?;

        self.hashed.insert_async(token_hash.into()).await?;
        Ok(token)
    }

    pub async fn verify(&self, token: impl AsRef<str>) -> Result<bool, AppError> {
        let token = token.as_ref();
        let token_hash = hash_string(token.as_bytes())?;
        Ok(self.hashed.contains_async(&Cow::Owned(token_hash)).await)
    }

    pub async fn revoke_all(&self) {
        self.hashed.clear_async().await;
    }
}

#[derive(Clone)]
pub(crate) struct MusicScopedTokens {
    map: Arc<HashMap<String, MusicScopedToken>>,
    expiry_time: u64,
}

impl MusicScopedTokens {
    pub fn new(expiry_time: u64) -> Self {
        Self {
            map: Arc::new(HashMap::new()),
            expiry_time,
        }
    }

    pub async fn generate_for_id(&self, music_id: String) -> String {
        let data = MusicScopedToken {
            creation: get_current_time(),
            music_id,
        };
        let token = generate_random_string(12);

        let _ = self.map.insert_async(token.clone(), data).await;
        return token;
    }

    pub async fn verify(&self, token: impl AsRef<str>) -> Option<String> {
        let token = token.as_ref();
        let data = self.map.read_async(token, |_, v| v.clone()).await?;
        if get_current_time() - data.creation > self.expiry_time {
            self.map.remove_async(token).await;
            return None;
        }
        Some(data.music_id)
    }
}

#[derive(Clone)]
struct MusicScopedToken {
    creation: u64,
    music_id: String,
}
