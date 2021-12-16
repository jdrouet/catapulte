use actix_multipart::Field;
use actix_web::web;
use actix_web::web::{BufMut, Bytes, BytesMut};
use futures::TryStreamExt;
use mime::Mime;
use serde_json::Value as JsonValue;
use serde_json::{from_slice, Error as JsonError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::string::FromUtf8Error;

#[derive(Debug)]
pub enum MultipartError {
    Parse(String),
}

impl ToString for MultipartError {
    fn to_string(&self) -> String {
        match self {
            Self::Parse(inner) => inner.clone(),
        }
    }
}

pub async fn field_to_bytes(mut field: Field) -> Bytes {
    let mut bytes = BytesMut::new();
    while let Ok(Some(field)) = field.try_next().await {
        bytes.put(field);
    }
    Bytes::from(bytes)
}

pub async fn field_to_string(field: Field) -> Result<String, FromUtf8Error> {
    String::from_utf8(field_to_bytes(field).await.to_vec())
}

pub async fn field_to_json_value(field: Field) -> Result<JsonValue, JsonError> {
    let bytes = field_to_bytes(field).await;
    from_slice(&bytes)
}

fn get_filename(field: &Field) -> Option<String> {
    let content = field.content_disposition();
    if let Some(filename) = content.get_filename() {
        return Some(filename.to_string());
    }
    None
}

#[derive(Debug)]
pub struct MultipartFile {
    pub filename: String,
    pub filepath: PathBuf,
    pub content_type: Mime,
}

impl MultipartFile {
    fn from_field(root: &Path, field: &Field) -> Result<Self, MultipartError> {
        let filename = match get_filename(field) {
            Some(value) => value,
            None => return Err(MultipartError::Parse("unable to get filename".into())),
        };
        let filepath = root.join(&filename);
        let content_type = field.content_type().clone();
        Ok(Self {
            filename,
            filepath,
            content_type,
        })
    }
}

pub async fn field_to_file(root: &Path, mut field: Field) -> Result<MultipartFile, MultipartError> {
    let multipart_file = MultipartFile::from_field(root, &field)?;
    let filepath = multipart_file.filepath.clone();
    // TODO find a better way than unwraping twice
    let mut file = web::block(|| std::fs::File::create(filepath))
        .await
        .unwrap()
        .unwrap();
    while let Ok(Some(chunk)) = field.try_next().await {
        file = web::block(move || file.write_all(&chunk).map(|_| file))
            .await
            .unwrap()
            .unwrap();
    }
    Ok(multipart_file)
}
