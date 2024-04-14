use catapulte_prelude::{EmbeddedTemplateDefinition, MetadataWithTemplate, TemplateDefinition};
use std::fs::{read_to_string, File};
use std::io::BufReader;
use std::path::PathBuf;

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Config {
    pub path: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            path: PathBuf::new().join("template"),
        }
    }
}

impl From<Config> for LocalLoader {
    fn from(value: Config) -> Self {
        Self::new(value.path)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Unable to open metadata file: {0:?}")]
    MetadataOpenFailed(std::io::Error),
    #[error("Unable to deserialize metadata file: {0:?}")]
    MetadataFormatInvalid(serde_json::Error),
    #[error("Unable to open template file: {0:?}")]
    TemplateOpenFailed(std::io::Error),
}

#[derive(Debug)]
pub struct LocalLoader {
    root: PathBuf,
}

impl LocalLoader {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl LocalLoader {
    pub(super) async fn find_by_name(
        &self,
        name: &str,
    ) -> Result<MetadataWithTemplate<EmbeddedTemplateDefinition>, Error> {
        tracing::debug!("loading template {}", name);
        let path = self.root.join(name).join("metadata.json");
        let metadata_file = File::open(path).map_err(|err| {
            metrics::counter!("template_provider_error", "reason" => "metadata_not_found")
                .increment(1);
            tracing::debug!("template provider error: metadata not found ({:?})", err);
            Error::MetadataOpenFailed(err)
        })?;
        let metadata_reader = BufReader::new(metadata_file);
        let metadata: MetadataWithTemplate<TemplateDefinition> =
            serde_json::from_reader(metadata_reader).map_err(|err| {
                metrics::counter!("template_provider_error", "reason" => "metadata_invalid")
                    .increment(1);
                tracing::debug!("template provider error: metadata invalid ({:?})", err);
                Error::MetadataFormatInvalid(err)
            })?;
        let template = match metadata.template {
            TemplateDefinition::Embedded(inner) => inner,
            TemplateDefinition::Local(inner) => {
                let template_path = self.root.join(inner.path);
                let content = read_to_string(template_path).map_err(|err| {
                    metrics::counter!("template_provider_error", "reason" => "template_not_found")
                        .increment(1);
                    tracing::debug!("template provider error: template not found ({:?})", err);
                    Error::TemplateOpenFailed(err)
                })?;
                EmbeddedTemplateDefinition { content }
            }
            TemplateDefinition::Remote(_inner) => unimplemented!(),
        };
        Ok(MetadataWithTemplate {
            inner: metadata.inner,
            template,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    #[tokio::test]
    async fn success_finds_template() {
        let loader = super::LocalLoader::new(PathBuf::new().join("..").join("..").join("template"));
        let meta = loader.find_by_name("user-login").await.unwrap();
        assert_eq!(meta.inner.name, "user-login");
        assert_eq!(
            meta.inner.description.as_deref(),
            Some("Template for login with magic link")
        );
        assert_eq!(meta.template.content, "<mjml>\n  <mj-head>\n    <mj-title>Hello {{name}}!</mj-title>\n    <mj-preview>Hello {{name}}!</mj-preview>\n  </mj-head>\n  <mj-body>\n    <mj-section>\n      <mj-column>\n        <mj-text>Hello {{name}}!</mj-text>\n        <mj-button href=\"http://example.com/login?token={{token}}\">Login</mj-button>\n      </mj-column>\n    </mj-section>\n  </mj-body>\n</mjml>\n");
    }

    #[tokio::test]
    async fn fails_metadata_not_found() {
        let loader = super::LocalLoader::new(PathBuf::new().join("..").join("..").join("template"));
        let err = loader.find_by_name("not-found").await.unwrap_err();
        assert!(matches!(err, super::Error::MetadataOpenFailed(_)));
    }

    #[tokio::test]
    async fn fails_invalid_metadata() {
        let loader = super::LocalLoader::new(PathBuf::new().join("..").join("..").join("template"));
        let err = loader.find_by_name("invalid-metadata").await.unwrap_err();
        assert!(matches!(err, super::Error::MetadataFormatInvalid(_)));
    }

    #[tokio::test]
    async fn success_embedded_template() {
        let loader = super::LocalLoader::new(PathBuf::new().join("..").join("..").join("template"));
        let meta = loader.find_by_name("embedded").await.unwrap();
        assert_eq!(meta.inner.name, "embedded");
        assert_eq!(
            meta.inner.description.as_deref(),
            Some("Template for login with magic link")
        );
        assert_eq!(meta.template.content, "<mjml></mjml>");
    }

    #[tokio::test]
    async fn fails_template_not_found() {
        let loader = super::LocalLoader::new(PathBuf::new().join("..").join("..").join("template"));
        let err = loader.find_by_name("template-not-found").await.unwrap_err();
        assert!(matches!(err, super::Error::TemplateOpenFailed(_)));
    }
}
