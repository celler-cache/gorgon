use std::borrow::Cow;

use gorgond_client::Uuid;

#[derive(Debug, Clone)]
pub struct Task {
    pub sub_id: Uuid,
    pub name: Cow<'static, str>,
}
impl Task {
    pub fn gen_sub_id() -> Uuid {
        let mut node_id = [0u8; 6];
        node_id[2..6].copy_from_slice(&std::process::id().to_be_bytes());
        Uuid::now_v6(&node_id)
    }
}
