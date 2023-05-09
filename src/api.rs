use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct WsApiMessage {
    pub(crate) name: String,
    #[serde(rename = "type")]
    pub(crate) kind: WsApiMessageType,
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) device_id: Option<String>,
    pub(crate) options: Map<String, Value>,
}

impl WsApiMessage {
    pub(crate) fn new(name: impl Into<String>, kind: WsApiMessageType) -> Self {
        Self {
            name: name.into(),
            kind,
            id: String::new(),
            device_id: None,
            options: Map::new(),
        }
    }

    pub(crate) fn request(name: impl Into<String>) -> Self {
        Self::new(name, WsApiMessageType::Request)
    }

    pub(crate) fn authenticate(password: impl Into<String>) -> Self {
        Self::new("authenticate", WsApiMessageType::Request).option("password", password.into())
    }

    pub(crate) fn id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    pub(crate) fn device_id(mut self, device_id: impl Into<String>) -> Self {
        self.device_id = Some(device_id.into());
        self
    }

    pub(crate) fn option(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.options.insert(key.into(), value.into());
        self
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum WsApiMessageType {
    #[serde(rename = "request")]
    Request,
    #[serde(rename = "response")]
    Response,
    #[serde(rename = "broadcast")]
    Broadcast,
}

impl ToString for WsApiMessage {
    fn to_string(&self) -> String {
        serde_json::to_string(self).unwrap()
    }
}
