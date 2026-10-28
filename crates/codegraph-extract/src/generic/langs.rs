//! The specs: which node types each grammar uses, and its hooks.

use super::Spec;
use codegraph_core::Language;

mod cfml;
mod erlang;
mod nix;
mod objc;
mod pascal;
mod solidity;
mod terraform;

/// The spec for a language the generic walker extracts.
pub fn spec_for(lang: Language) -> Option<&'static Spec> {
    match lang {
        Language::Solidity => Some(&solidity::SPEC),
        Language::Pascal => Some(&pascal::SPEC),
        Language::Terraform => Some(&terraform::SPEC),
        Language::Nix => Some(&nix::SPEC),
        Language::Erlang => Some(&erlang::SPEC),
        Language::ObjC => Some(&objc::SPEC),
        Language::Cfml => Some(&cfml::SPEC),
        Language::CfScript => Some(&cfml::SCRIPT),
        _ => None,
    }
}
