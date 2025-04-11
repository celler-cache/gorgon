// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::borrow::Cow;

use gorgond_client::Uuid;

#[derive(Debug, Default, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Task {
    pub sid: Uuid,
    pub parent_sid: Option<Uuid>,
    pub name: Cow<'static, str>,
    pub level: Option<log::Level>,
    pub phase: Option<String>,
}
impl Task {
    pub fn gen_sid() -> Uuid {
        let mut node_id = [0u8; 6];
        node_id[2..6].copy_from_slice(&std::process::id().to_be_bytes());
        Uuid::now_v6(&node_id)
    }
}
