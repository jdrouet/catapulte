pub(crate) use mrml::prelude::render::Options as RenderOptions;

#[derive(Clone, Debug, Default, serde::Deserialize)]
pub(crate) struct Configuration {
    pub disable_comments: bool,
    pub social_icon_origin: Option<String>,
}

impl Configuration {
    pub(crate) fn build(&self) -> RenderOptions {
        tracing::debug!("building render options");
        RenderOptions {
            disable_comments: self.disable_comments,
            social_icon_origin: self.social_icon_origin.clone(),
        }
    }
}
