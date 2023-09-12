use chrono::prelude::*;
use chrono::DateTime;
use sea_orm::{entity::prelude::*, FromJsonQueryResult};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "builds")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub name: String,
    pub machine_name: String,

    pub start: DateTime<Utc>,
    pub start_phase: PhaseList,
    pub end: Option<DateTime<Utc>>,
}

#[derive(Debug, Copy, Clone, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, FromJsonQueryResult)]
pub struct Phase(pub String, pub DateTime<Utc>);

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize, FromJsonQueryResult)]
pub struct PhaseList(pub Vec<Phase>);
