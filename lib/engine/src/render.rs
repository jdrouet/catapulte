use std::collections::HashMap;

#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct Config {
    #[serde(default)]
    pub disable_comments: bool,
    #[serde(default)]
    pub social_icon_origin: Option<String>,
    #[serde(default)]
    pub fonts: Option<HashMap<String, String>>,
}

impl From<Config> for mrml::prelude::render::RenderOptions {
    fn from(value: Config) -> Self {
        let mut result: Self = mrml::prelude::render::RenderOptions {
            disable_comments: value.disable_comments,
            ..Default::default()
        };
        if let Some(origin) = value.social_icon_origin {
            result.social_icon_origin = Some(origin.into());
        }
        // `RenderOptions.fonts` has a default list of `fonts`. We want to be able
        // to override this list but to keep it if `fonts` is `None`.
        if let Some(fonts) = value.fonts {
            result.fonts = fonts
                .into_iter()
                .map(|(key, value)| (key, value.into()))
                .collect();
        }
        result
    }
}
