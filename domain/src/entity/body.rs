use thiserror::Error;

#[derive(Debug, Clone)]
pub struct Plain {
    text: Option<String>,
    html: Option<String>,
}

#[derive(Debug, Error)]
pub enum InvalidPlainBody {
    #[error("at least one of text or html must be provided")]
    Empty,
}

impl Plain {
    /// # Errors
    ///
    /// Returns `InvalidPlainBody::Empty` when both `text` and `html` are `None`.
    pub fn try_new(text: Option<String>, html: Option<String>) -> Result<Self, InvalidPlainBody> {
        if text.is_none() && html.is_none() {
            return Err(InvalidPlainBody::Empty);
        }
        Ok(Self { text, html })
    }

    #[must_use]
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    #[must_use]
    pub fn html(&self) -> Option<&str> {
        self.html.as_deref()
    }

    #[must_use]
    pub fn into_parts(self) -> (Option<String>, Option<String>) {
        (self.text, self.html)
    }
}

#[derive(Debug, Clone)]
pub enum MjmlSource {
    Inline(String),
    Named(String),
    Remote(url::Url),
}

#[derive(Debug, Clone)]
pub enum BodySource {
    Plain(Plain),
    Mjml(MjmlSource),
}

#[derive(Debug, Clone)]
pub enum ResolvedBody {
    Plain(Plain),
    Mjml(String),
}

#[derive(Debug, Clone)]
pub enum InterpolatedBody {
    Plain(Plain),
    Mjml(String),
}

#[derive(Debug)]
pub struct RenderedBody(Plain);

impl RenderedBody {
    #[must_use]
    pub const fn new(plain: Plain) -> Self {
        Self(plain)
    }

    #[must_use]
    pub fn into_plain(self) -> Plain {
        self.0
    }

    #[must_use]
    pub fn text(&self) -> Option<&str> {
        self.0.text()
    }

    #[must_use]
    pub fn html(&self) -> Option<&str> {
        self.0.html()
    }
}

#[cfg(test)]
mod tests {
    use super::{InvalidPlainBody, Plain};

    #[test]
    fn plain_try_new_empty_returns_error() {
        let err = Plain::try_new(None, None).unwrap_err();
        assert!(matches!(err, InvalidPlainBody::Empty));
    }
}
