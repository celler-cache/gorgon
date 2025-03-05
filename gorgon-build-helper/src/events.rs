use std::{borrow::Cow, collections::BTreeMap};

use log::kv::ToKey;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event<'a> {
    #[serde(borrow)]
    Log(LogEvent<'a>),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LogEvent<'a> {
    #[serde(rename = "lvl")]
    pub level: log::Level,
    #[serde(borrow, rename = "tgt")]
    pub target: Cow<'a, str>,
    #[serde(borrow, rename = "msg")]
    pub msg: Cow<'a, str>,
    #[serde(
        borrow,
        rename = "kv",
        skip_serializing_if = "BTreeMap::is_empty",
        default
    )]
    pub key_values: BTreeMap<Cow<'a, str>, serde_json::Value>,
    #[serde(
        borrow,
        rename = "mod",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub module_path: Option<Cow<'a, str>>,
    #[serde(
        borrow,
        rename = "file",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub file: Option<Cow<'a, str>>,
    #[serde(rename = "line", skip_serializing_if = "Option::is_none", default)]
    pub line: Option<u32>,
}

impl<'a> TryFrom<&'a log::Record<'a>> for LogEvent<'a> {
    type Error = log::kv::Error;
    fn try_from(rec: &'a log::Record<'a>) -> Result<Self, Self::Error> {
        #[derive(Default)]
        struct Visitor<'a>(BTreeMap<Cow<'a, str>, serde_json::Value>);
        impl<'a> log::kv::VisitSource<'a> for Visitor<'a> {
            fn visit_pair(
                &mut self,
                key: log::kv::Key<'a>,
                value: log::kv::Value<'a>,
            ) -> Result<(), log::kv::Error> {
                self.0.insert(
                    key.to_borrowed_str()
                        .map(Cow::Borrowed)
                        .unwrap_or_else(|| Cow::Owned(key.as_str().into())),
                    serde_json::to_value(value).map_err(log::kv::Error::boxed)?,
                );
                Ok(())
            }
        }
        let key_values = {
            let mut visitor = Visitor::default();
            rec.key_values().visit(&mut visitor)?;
            visitor.0
        };

        let args = rec.args();
        Ok(Self {
            level: rec.level(),
            target: rec.target().into(),
            msg: args
                .as_str()
                .map(Cow::Borrowed)
                .unwrap_or_else(|| Cow::Owned(format!("{args}"))),
            key_values,
            module_path: rec.module_path().map(|s| s.into()),
            file: rec.file().map(|s| s.into()),
            line: rec.line(),
        })
    }
}

impl log::kv::Source for LogEvent<'_> {
    fn visit<'kvs>(
        &'kvs self,
        visitor: &mut dyn log::kv::VisitSource<'kvs>,
    ) -> Result<(), log::kv::Error> {
        for (k, v) in self.key_values.iter() {
            visitor.visit_pair(k.to_key(), log::kv::Value::from_serde(v))?;
        }
        Ok(())
    }
}
