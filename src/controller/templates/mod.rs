use lettre::message::Mailbox;

pub(super) mod json;
pub(super) mod multipart;

#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
pub(crate) enum Recipient {
    One(Mailbox),
    Many(Vec<Mailbox>),
}

impl Default for Recipient {
    fn default() -> Self {
        Self::Many(Vec::default())
    }
}

impl<'s> utoipa::ToSchema<'s> for Recipient {
    fn schema() -> (
        &'s str,
        utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
    ) {
        (
            "Recipient",
            utoipa::openapi::OneOfBuilder::new()
                .item(
                    utoipa::openapi::ObjectBuilder::new()
                        .example(Some(serde_json::Value::String(String::from(
                            "Bob Sponge <bob.sponge@sea.earth>",
                        ))))
                        .schema_type(utoipa::openapi::SchemaType::String),
                )
                .item(
                    utoipa::openapi::ArrayBuilder::new()
                        .items(utoipa::openapi::Object::with_type(
                            utoipa::openapi::SchemaType::String,
                        ))
                        .example(Some(serde_json::Value::Array(vec![
                            serde_json::Value::String(String::from(
                                "Bob Sponge <bob.sponge@sea.earth>",
                            )),
                        ]))),
                )
                .into(),
        )
    }
}

impl Recipient {
    fn into_vec(self) -> Vec<Mailbox> {
        match self {
            Recipient::One(item) => vec![item],
            Recipient::Many(list) => list,
        }
    }
}
