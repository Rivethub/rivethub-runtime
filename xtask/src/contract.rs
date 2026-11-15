use std::collections::BTreeSet;
use std::path::Path;

use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{Attribute, Ident, Token, Visibility};

use crate::error::Result;
use crate::git;

const WIRE_SRC: &str = "crates/wire/src/";
const ROOTS_FILE: &str = "crates/wire/src/contract.rs";

pub fn check(root: &Path, findings: &mut Vec<String>) -> Result<usize> {
    let roots = match std::fs::read_to_string(root.join(ROOTS_FILE)) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.into()),
    };
    let modules = root_modules(&roots);
    let mut count = 0;
    for rel in git::tracked_files(root)? {
        let Some(module) = rel.strip_prefix(WIRE_SRC) else {
            continue;
        };
        if rel == ROOTS_FILE || !crate::rustsrc::is_scanned(&rel) {
            continue;
        }
        let file = syn::parse_file(&std::fs::read_to_string(root.join(&rel))?)?;
        let mut types = Types::default();
        types.visit_file(&file);
        let rooted = modules.contains(top_module(module));
        for (ident, schema) in types.serde {
            let line = ident.span().start().line;
            if !schema {
                findings.push(format!(
                    "CONTRACT {rel}:{line}: {ident} derives serde but not schemars::JsonSchema"
                ));
                count += 1;
            } else if rooted && !roots.contains(&format!("::{ident}>")) {
                findings.push(format!(
                    "CONTRACT {rel}:{line}: {ident} is missing from ROOTS in {ROOTS_FILE}"
                ));
                count += 1;
            }
        }
    }
    Ok(count)
}

fn root_modules(roots: &str) -> BTreeSet<&str> {
    roots
        .split("crate::")
        .skip(1)
        .filter_map(|rest| rest.split_once("::").map(|(module, _)| module))
        .collect()
}

fn top_module(module: &str) -> &str {
    let first = module.split_once('/').map_or(module, |(first, _)| first);
    first.strip_suffix(".rs").unwrap_or(first)
}

#[derive(Default)]
struct Types {
    serde: Vec<(Ident, bool)>,
}

impl Types {
    fn inspect(&mut self, vis: &Visibility, attrs: &[Attribute], ident: &Ident) {
        if !matches!(vis, Visibility::Public(_)) {
            return;
        }
        let derived = derives(attrs);
        if derived.contains("Serialize") || derived.contains("Deserialize") {
            self.serde
                .push((ident.clone(), derived.contains("JsonSchema")));
        }
    }
}

impl<'ast> Visit<'ast> for Types {
    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        self.inspect(&item.vis, &item.attrs, &item.ident);
        visit::visit_item_struct(self, item);
    }

    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
        self.inspect(&item.vis, &item.attrs, &item.ident);
        visit::visit_item_enum(self, item);
    }
}

fn derives(attrs: &[Attribute]) -> BTreeSet<String> {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("derive"))
        .filter_map(|attr| {
            attr.parse_args_with(Punctuated::<syn::Path, Token![,]>::parse_terminated)
                .ok()
        })
        .flatten()
        .filter_map(|path| {
            path.segments
                .last()
                .map(|segment| segment.ident.to_string())
        })
        .collect()
}
