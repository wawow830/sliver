use crate::lua_worker::LuaSource;

const DEFAULT_BYTES: &[u8] = include_bytes!("default.lua");

pub(crate) fn source() -> LuaSource {
    LuaSource::embedded(DEFAULT_BYTES.to_vec())
}

#[cfg(test)]
pub(crate) fn bytes() -> &'static [u8] {
    DEFAULT_BYTES
}
