use serde::Deserialize;
use std::{collections::HashMap, fmt};

#[derive(Deserialize, Eq, PartialEq, Hash)]
#[serde(transparent)]
pub struct ItemId(pub String);

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

#[derive(Deserialize)]
pub struct Document {
    pub format_version: i64,
    pub root: ItemId,
    pub index: HashMap<ItemId, Item>,
}

#[derive(Deserialize)]
pub struct Item {
    pub id: ItemId,
    pub name: Option<String>,
    pub docs: Option<String>,
    pub inner: ItemInner,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemInner {
    Module(Module),
    Impl(Impl),
    Function(Function),
    StructField(StructField),
    AssocType(AssocType),
    Variant(Variant),
    Enum(Enum),
    AssocConst(AssocConst),
    Struct(Struct),
    Constant(Constant),
}

#[derive(Deserialize)]
pub struct Module {
    pub items: Vec<ItemId>,
}

#[derive(Deserialize)]
pub struct Impl {}

#[derive(Deserialize)]
pub struct Function {}

#[derive(Deserialize)]
pub struct StructField {}

#[derive(Deserialize)]
pub struct AssocType {}

#[derive(Deserialize)]
pub struct Variant {}

#[derive(Deserialize)]
pub struct Enum {}

#[derive(Deserialize)]
pub struct AssocConst {}

#[derive(Deserialize)]
pub struct Struct {}

#[derive(Deserialize)]
pub struct Constant {}
