use std::borrow::Cow;

use bounded_static::ToStatic;

#[derive(Debug, Clone, ToStatic)]
pub struct Task<'a> {
    pub name: Cow<'a, str>,
}
impl<'a> Task<'a> {
    pub fn new(name: impl Into<Cow<'a, str>>) -> Self {
        Self { name: name.into() }
    }
}
