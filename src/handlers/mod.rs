use ::http::HeaderValue;
use serde::Deserialize;

pub(crate) mod data;
pub(crate) mod internal;
pub(crate) mod metadata;
pub(crate) mod share;

pub(crate) const AUDIO_CACHE_HEADER: HeaderValue =
    HeaderValue::from_static("private, max-age=604800");

#[derive(Deserialize)]
pub(crate) struct Auth {
    #[serde(default)]
    token: Option<String>,
}

pub(crate) use self::data::*;
pub(crate) use internal::*;
pub(crate) use metadata::*;
pub(crate) use share::*;
