use std::str::FromStr;

use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorClass {
    TemplateResolve,
    TemplateInterpolate,
    TemplateRender,
    Attachment,
    Delivery,
    Routing,
}

impl ErrorClass {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TemplateResolve => "template_resolve",
            Self::TemplateInterpolate => "template_interpolate",
            Self::TemplateRender => "template_render",
            Self::Attachment => "attachment",
            Self::Delivery => "delivery",
            Self::Routing => "routing",
        }
    }
}

#[derive(Debug, Error)]
#[error("unknown error class {value:?}")]
pub struct UnknownErrorClass {
    pub value: String,
}

impl FromStr for ErrorClass {
    type Err = UnknownErrorClass;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "template_resolve" => Ok(Self::TemplateResolve),
            "template_interpolate" => Ok(Self::TemplateInterpolate),
            "template_render" => Ok(Self::TemplateRender),
            "attachment" => Ok(Self::Attachment),
            "delivery" => Ok(Self::Delivery),
            "routing" => Ok(Self::Routing),
            _ => Err(UnknownErrorClass {
                value: s.to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::ErrorClass;

    #[test]
    fn round_trip_template_resolve() {
        let ec = ErrorClass::TemplateResolve;
        assert_eq!(ErrorClass::from_str(ec.as_str()).unwrap(), ec);
    }

    #[test]
    fn round_trip_template_interpolate() {
        let ec = ErrorClass::TemplateInterpolate;
        assert_eq!(ErrorClass::from_str(ec.as_str()).unwrap(), ec);
    }

    #[test]
    fn round_trip_template_render() {
        let ec = ErrorClass::TemplateRender;
        assert_eq!(ErrorClass::from_str(ec.as_str()).unwrap(), ec);
    }

    #[test]
    fn round_trip_attachment() {
        let ec = ErrorClass::Attachment;
        assert_eq!(ErrorClass::from_str(ec.as_str()).unwrap(), ec);
    }

    #[test]
    fn round_trip_delivery() {
        let ec = ErrorClass::Delivery;
        assert_eq!(ErrorClass::from_str(ec.as_str()).unwrap(), ec);
    }

    #[test]
    fn round_trip_routing() {
        let ec = ErrorClass::Routing;
        assert_eq!(ErrorClass::from_str(ec.as_str()).unwrap(), ec);
    }

    #[test]
    fn unknown_string_returns_error() {
        let result = ErrorClass::from_str("bogus");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.value, "bogus");
    }
}
