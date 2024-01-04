pub(crate) use mrml::prelude::render::RenderOptions;

#[derive(Clone, Debug, Default, serde::Deserialize)]
pub(crate) struct Configuration {
    pub disable_comments: bool,
    pub social_icon_origin: Option<String>,
}

impl Configuration {
    pub(crate) fn build(&self) -> RenderOptions {
        tracing::debug!("building render options");
        let mut opts = RenderOptions {
            disable_comments: self.disable_comments,
            ..Default::default()
        };
        if let Some(ref url) = self.social_icon_origin {
            opts.social_icon_origin = Some(url.to_string().into());
        }
        opts
    }
}
