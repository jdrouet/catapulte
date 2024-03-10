use axum::body::Bytes;
use axum::extract::multipart::Field;
use serde_json::Value as JsonValue;
use serde_json::{from_slice, Error as JsonError};
use std::fmt::Display;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::string::FromUtf8Error;

#[derive(Debug)]
pub(crate) enum MultipartError {
    Parse(&'static str),
}

impl Display for MultipartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(inner) => inner.fmt(f),
        }
    }
}

pub(crate) async fn field_to_bytes(field: Field<'_>) -> Bytes {
    field.bytes().await.unwrap()
}

pub(crate) async fn field_to_string(field: Field<'_>) -> Result<String, FromUtf8Error> {
    String::from_utf8(field_to_bytes(field).await.to_vec())
}

pub(crate) async fn field_to_json_value(field: Field<'_>) -> Result<JsonValue, JsonError> {
    let bytes = field_to_bytes(field).await;
    from_slice(&bytes)
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct MultipartFile {
    pub filename: String,
    pub filepath: PathBuf,
    pub content_type: Option<String>,
}

impl MultipartFile {
    fn from_field(root: &Path, field: &Field<'_>) -> Result<Self, MultipartError> {
        let filename = field
            .file_name()
            .map(ToString::to_string)
            .ok_or(MultipartError::Parse("unable to get filename"))?;
        let filepath = root.join(&filename);
        let content_type = field.content_type().map(|v| v.to_owned());
        Ok(Self {
            filename,
            filepath,
            content_type,
        })
    }
}

pub(crate) async fn field_to_file<'a>(
    root: &Path,
    mut field: Field<'a>,
) -> Result<MultipartFile, MultipartError> {
    let multipart_file = MultipartFile::from_field(root, &field)?;
    let filepath = multipart_file.filepath.clone();
    // TODO find a better way to load file content
    let mut file = std::fs::File::create(filepath).unwrap();
    while let Ok(Some(chunk)) = field.chunk().await {
        file.write_all(&chunk).unwrap();
    }
    Ok(multipart_file)
}
