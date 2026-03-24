use std::ffi::c_int;

pub mod entity_kind_serde {
    use super::*;
    use clang::EntityKind;

    pub fn serialize<S: serde::Serializer>(kind: &EntityKind, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i32(*kind as c_int)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<EntityKind, D::Error> {
        use serde::Deserialize;
        let v = c_int::deserialize(d)?;
        match v {
            1..=50 | 70..=73 | 100..=149 | 200..=280 | 300 | 400..=441 | 500..=503 | 600..=603
            | 700 => Ok(unsafe { std::mem::transmute(v) }),
            _ => Err(serde::de::Error::custom(format!(
                "invalid EntityKind discriminant: {v}"
            ))),
        }
    }
}

pub mod type_kind_serde {
    use super::*;
    use clang::TypeKind;

    pub fn serialize<S: serde::Serializer>(tk: &TypeKind, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i32(*tk as c_int)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<TypeKind, D::Error> {
        use serde::Deserialize;
        let v = c_int::deserialize(d)?;
        match v {
            1..=38 | 101..=175 => Ok(unsafe { std::mem::transmute(v) }),
            _ => Err(serde::de::Error::custom(format!(
                "invalid TypeKind discriminant: {v}"
            ))),
        }
    }
}

pub mod option_storage_class_serde {
    use super::*;
    use clang::StorageClass;

    pub fn serialize<S: serde::Serializer>(
        sc: &Option<StorageClass>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match sc {
            Some(sc) => s.serialize_i32(*sc as c_int),
            None => s.serialize_i32(0),
        }
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Option<StorageClass>, D::Error> {
        use serde::Deserialize;
        let v = c_int::deserialize(d)?;
        match v {
            0 => Ok(None),
            1..=7 => Ok(Some(unsafe { std::mem::transmute(v) })),
            _ => Err(serde::de::Error::custom(format!(
                "invalid StorageClass discriminant: {v}"
            ))),
        }
    }
}
