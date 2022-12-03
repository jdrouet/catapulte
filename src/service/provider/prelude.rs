use crate::error::ServerError;
use serde_json::json;
use std::borrow::Cow;

#[derive(Clone, Debug)]
pub enum Error {
    Loading { origin: Cow<'static, str> },
}

impl From<Error> for ServerError {
    fn from(err: Error) -> Self {
        match err {
            Error::Loading { origin } => ServerError::not_found()
                .message("unable to find template")
                .details(json!({ "origin": origin })),
        }
    }
}
