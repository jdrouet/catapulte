#[macro_export]
macro_rules! genid {
    ($struct_name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $struct_name(uuid::Uuid);

        impl Default for $struct_name {
            fn default() -> Self {
                Self(uuid::Uuid::now_v7())
            }
        }

        impl From<uuid::Uuid> for $struct_name {
            fn from(value: uuid::Uuid) -> Self {
                Self(value)
            }
        }

        impl $struct_name {
            pub const fn as_uuid(&self) -> uuid::Uuid {
                self.0
            }
        }
    };
}
