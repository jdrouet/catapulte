use utoipa::openapi::Type;

pub(crate) mod json;
pub(crate) mod multipart;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(transparent)]
pub(crate) struct Mailbox(lettre::message::Mailbox);

impl Mailbox {
    fn inner(self) -> lettre::message::Mailbox {
        self.0
    }
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

impl Recipient {
    fn into_vec(self) -> Vec<lettre::message::Mailbox> {
        match self {
            Recipient::One(item) => vec![item.inner()],
            Recipient::Many(list) => list.into_iter().map(|item| item.inner()).collect(),
        }
    }
}
