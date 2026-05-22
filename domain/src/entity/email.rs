crate::genid!(EmailId);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RecipientKind {
    To,
    Cc,
    Bcc,
}
