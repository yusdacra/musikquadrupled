use rand::Rng;
use scc::HashSet;
use std::borrow::Cow;
use std::fmt::Write;
use std::path::Path;
use std::sync::Arc;

use crate::error::AppError;

fn hash_string(data: &[u8]) -> Result<String, argon2::Error> {
    argon2::hash_encoded(data, "11111111".as_bytes(), &argon2::Config::default())
}

#[derive(Debug, Clone)]
pub(crate) struct Tokens {
    hashed: Arc<HashSet<Cow<'static, str>>>,
    raw_contents: &'static str,
}

impl Tokens {
    pub async fn read(path: impl AsRef<Path>) -> Result<Self, AppError> {
        let this = Self {
            hashed: Arc::new(HashSet::new()),
            raw_contents: Box::leak(tokio::fs::read_to_string(path).await?.into_boxed_str()),
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
            .for_each_async(|hash| {
                writeln!(&mut contents, "{hash}").expect("if this fails then too bad")
            })
            .await;

        tokio::fs::write(path, contents).await.map_err(Into::into)
    }

    pub async fn generate(&self) -> Result<String, AppError> {
        let token = rand::thread_rng()
            .sample_iter(rand::distributions::Alphanumeric)
            .take(30)
            .map(|c| c as char)
            .collect::<String>();

        let token_hash = hash_string(token.as_bytes())?;

        self.hashed.insert_async(token_hash.into()).await?;
        Ok(token)
    }

    pub async fn verify(&self, token: impl AsRef<str>) -> Result<bool, AppError> {
        let token = token.as_ref();
        let token_hash = hash_string(token.as_bytes())?;
        Ok(self.hashed.contains_async(&Cow::Owned(token_hash)).await)
    }
}
