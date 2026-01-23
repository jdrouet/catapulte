pub mod json;
pub mod multipart;

use catapulte_domain::model::Recipient as DomainRecipient;
use utoipa::openapi::Type;

use crate::error::RequestError;

/// Email address with optional display name
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(untagged)]
pub enum Mailbox {
    /// Simple email string: "user@example.com" or "Name <user@example.com>"
    Simple(String),
    /// Structured format with separate name and email fields
    Structured { name: Option<String>, email: String },
}

impl Mailbox {
    pub fn into_domain(self) -> Result<DomainRecipient, RequestError> {
        match self {
            Mailbox::Simple(s) => parse_mailbox_string(&s),
            Mailbox::Structured { name, email } => {
                // Basic email validation
                if !email.contains('@') {
                    return Err(RequestError::InvalidEmail(email));
                }
                Ok(DomainRecipient { name, email })
            }
        }
    }
}

fn parse_mailbox_string(s: &str) -> Result<DomainRecipient, RequestError> {
    // Try to parse "Name <email@example.com>" format
    if let Some(start) = s.find('<')
        && let Some(end) = s.find('>')
    {
        let name = s[..start].trim();
        let email = s[start + 1..end].trim();
        if !email.contains('@') {
            return Err(RequestError::InvalidEmail(email.to_string()));
        }
        return Ok(DomainRecipient {
            name: if name.is_empty() {
                None
            } else {
                Some(name.to_string())
            },
            email: email.to_string(),
        });
    }

    // Plain email address
    if !s.contains('@') {
        return Err(RequestError::InvalidEmail(s.to_string()));
    }
    Ok(DomainRecipient {
        name: None,
        email: s.to_string(),
    })
}

impl utoipa::ToSchema for Mailbox {
    fn name() -> std::borrow::Cow<'static, str> {
        "Mailbox".into()
    }
}

impl utoipa::PartialSchema for Mailbox {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::OneOfBuilder::new()
            .item(
                utoipa::openapi::ObjectBuilder::new()
                    .examples(["Bob Sponge <bob.sponge@sea.earth>"])
                    .schema_type(utoipa::openapi::schema::SchemaType::Type(Type::String)),
            )
            .item(
                utoipa::openapi::ObjectBuilder::new()
                    .property(
                        "name",
                        utoipa::openapi::ObjectBuilder::new()
                            .examples(["Bob Sponge"])
                            .schema_type(utoipa::openapi::schema::SchemaType::Type(Type::String)),
                    )
                    .property(
                        "email",
                        utoipa::openapi::ObjectBuilder::new()
                            .examples(["bob.sponge@sea.earth"])
                            .schema_type(utoipa::openapi::schema::SchemaType::Type(Type::String)),
                    )
                    .required("email"),
            )
            .into()
    }
}

/// One or more recipients
#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
pub enum Recipient {
    One(Mailbox),
    Many(Vec<Mailbox>),
}

impl Default for Recipient {
    fn default() -> Self {
        Self::Many(Vec::new())
    }
}

impl Recipient {
    pub fn into_domain_vec(self) -> Result<Vec<DomainRecipient>, RequestError> {
        match self {
            Recipient::One(mailbox) => Ok(vec![mailbox.into_domain()?]),
            Recipient::Many(list) => list.into_iter().map(|m| m.into_domain()).collect(),
        }
    }
}

impl utoipa::ToSchema for Recipient {
    fn name() -> std::borrow::Cow<'static, str> {
        "Recipient".into()
    }
}

impl utoipa::PartialSchema for Recipient {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::OneOfBuilder::new()
            .item(
                utoipa::openapi::ObjectBuilder::new()
                    .examples(["Bob Sponge <bob.sponge@sea.earth>"])
                    .schema_type(utoipa::openapi::schema::SchemaType::Type(Type::String)),
            )
            .item(
                utoipa::openapi::ArrayBuilder::new()
                    .examples([["Bob Sponge <bob.sponge@sea.earth>"]])
                    .items(utoipa::openapi::Object::with_type(
                        utoipa::openapi::schema::SchemaType::Type(Type::String),
                    )),
            )
            .into()
    }
}
