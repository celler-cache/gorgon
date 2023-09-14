// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Build::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Build::Name).string().not_null())
                    .col(ColumnDef::new(Build::MachineName).string().not_null())
                    .col(ColumnDef::new(Build::Start).date_time().not_null())
                    .col(
                        ColumnDef::new(Build::StartPhase)
                            .json()
                            .not_null()
                            .default("[]"),
                    )
                    .col(ColumnDef::new(Build::End).date_time().null())
                    .primary_key(
                        Index::create()
                            .col(Build::Name)
                            .col(Build::MachineName)
                            .col(Build::Start),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Build::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Build {
    #[sea_orm(iden = "builds")]
    Table,
    Name,
    MachineName,
    Start,
    StartPhase,
    End,
}
