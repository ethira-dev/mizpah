//! Format-v1 pack loader, detection, and parsing.

mod normalize;
mod registry;

pub use registry::{
    classify_pack_json, detect_pack_text, parse_pack_text, parse_with_format_hint,
};

#[cfg(test)]
pub use registry::{loaded_pack_ids, registry, FormatKind};

#[cfg(test)]
use normalize::mizpah_format_id;

/// Map upstream pack id → stable Mizpah `format_id` when applicable.
#[cfg(test)]
pub fn stable_format_id(pack_id: &str) -> String {
    mizpah_format_id(pack_id).to_string()
}

#[cfg(test)]
mod tests;
