use axum::body::Bytes;
use axum::extract::multipart::Field;
use serde_json::Value as JsonValue;
use serde_json::{from_slice, Error as JsonError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::string::FromUtf8Error;

#[derive(Debug)]
pub enum MultipartError {
    Parse(&'static str),
}

impl ToString for MultipartError {
    fn to_string(&self) -> String {
        match self {
            Self::Parse(inner) => inner.to_string(),
        }
    }
}

pub async fn field_to_bytes<'a>(field: Field<'a>) -> Bytes {
    // let _name = field.name().unwrap().to_string();
    field.bytes().await.unwrap()
}

pub async fn field_to_string<'a>(field: Field<'a>) -> Result<String, FromUtf8Error> {
    String::from_utf8(field_to_bytes(field).await.to_vec())
}

pub async fn field_to_json_value<'a>(field: Field<'a>) -> Result<JsonValue, JsonError> {
    let bytes = field_to_bytes(field).await;
    from_slice(&bytes)
}

#[derive(Debug, serde::Deserialize)]
pub struct MultipartFile {
    pub filename: String,
    pub filepath: PathBuf,
    pub content_type: Option<String>,
}

impl MultipartFile {
    fn from_field<'a>(root: &Path, field: &Field<'a>) -> Result<Self, MultipartError> {
        let filename = field
            .file_name()
            .map(ToString::to_string)
            .ok_or_else(|| MultipartError::Parse("unable to get filename"))?;
        let filepath = root.join(&filename);
        let content_type = field.content_type().map(|v| v.to_owned());
        Ok(Self {
            filename,
            filepath,
            content_type,
        })
    }
}

pub async fn field_to_file<'a>(
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
