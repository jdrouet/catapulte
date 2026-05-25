#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlobRef {
    pub backend: String,
    pub key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentRef {
    pub filename: String,
    pub content_type: String,
    pub size_bytes: u64,
    pub blob: BlobRef,
}

#[derive(Debug)]
pub struct ResolvedAttachment {
    pub filename: String,
    pub content_type: String,
    pub bytes: bytes::Bytes,
}
