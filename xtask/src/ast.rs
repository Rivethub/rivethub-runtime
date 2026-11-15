use syn::parse::Parser as _;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned as _;
use syn::visit::{self, Visit};
use syn::{Expr, Meta, Token};

use crate::rustsrc::{Hit, Hits};

pub(crate) struct Scanner<'a> {
    rel: &'a str,
    hits: Hits,
    quotes: Vec<proc_macro2::Span>,
    brace: bool,
}

impl<'a> Scanner<'a> {
    pub(crate) fn new(rel: &'a str) -> Self {
        Self {
            rel,
            hits: Hits::new(rel),
            quotes: Vec::new(),
            brace: false,
        }
    }

    fn environment_import(&mut self, tree: &syn::UseTree) {
        let ident = match tree {
            syn::UseTree::Name(name) => &name.ident,
            syn::UseTree::Rename(rename) => &rename.ident,
            syn::UseTree::Group(group) => {
                for child in &group.items {
                    self.environment_import(child);
                }
                return;
            }
            syn::UseTree::Path(_) | syn::UseTree::Glob(_) => return,
        };
        if [
            "var",
            "var_os",
            "vars",
            "vars_os",
            "temp_dir",
            "home_dir",
            "current_dir",
        ]
        .iter()
        .any(|name| ident == name)
        {
            self.hits.push("ENV", ident.span(), format!("env::{ident}"));
        }
    }

    pub(crate) fn finish(mut self) -> Vec<(&'static str, Hit)> {
        if self.brace && !crate::rustsrc::in_wire(self.rel) {
            for span in self.quotes {
                self.hits.push("JSONBYTES", span, "b'\"'");
            }
        }
        self.hits.items
    }
}

impl<'ast> Visit<'ast> for Scanner<'_> {
    fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
        crate::ports::inspect(self.rel, item, &mut self.hits);
        visit::visit_item_trait(self, item);
    }

    fn visit_ident(&mut self, ident: &'ast syn::Ident) {
        let name = ident.to_string();
        let name = name.strip_prefix("r#").unwrap_or(&name);
        if self.rel != "crates/capture/src/event_id.rs" {
            if crate::jsnames::banned_name(name) {
                self.hits.push("JSMODEL", ident.span(), name);
            }
            let lower = name.to_ascii_lowercase();
            if !crate::rustsrc::in_wire(self.rel)
                && (lower.contains("utf16") || lower.ends_with("_units"))
            {
                self.hits.push("UTF16", ident.span(), name);
            }
        }
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        if !self.rel.starts_with("crates/config/") && self.rel != "crates/rivethub/src/main.rs" {
            let mut segments = path.segments.iter().rev();
            if let (Some(name), Some(parent)) = (segments.next(), segments.next())
                && parent.ident == "env"
                && matches!(
                    name.ident.to_string().as_str(),
                    "var" | "var_os" | "vars" | "vars_os" | "temp_dir" | "home_dir" | "current_dir"
                )
            {
                self.hits
                    .push("ENV", name.span(), format!("env::{}", name.ident));
            }
        }
        visit::visit_path(self, path);
    }

    fn visit_use_path(&mut self, path: &'ast syn::UsePath) {
        if path.ident == "env"
            && !self.rel.starts_with("crates/config/")
            && self.rel != "crates/rivethub/src/main.rs"
        {
            self.environment_import(&path.tree);
        }
        visit::visit_use_path(self, path);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if call.method == "starts_with" && path_string(&call.receiver) {
            self.hits.push(
                "PATHCONTAIN",
                call.method.span(),
                "starts_with on a path string",
            );
        }
        if call.method == "unwrap_or" {
            let mut variants = Privileged::default();
            for argument in &call.args {
                variants.visit_expr(argument);
            }
            if let Some(name) = variants.name {
                self.hits.push(
                    "PRIVILEGE",
                    call.method.span(),
                    format!("unwrap_or(..{name}..)"),
                );
            }
        }
        if call.method == "unwrap_or_default"
            && let Some(name) = privilege_receiver(&call.receiver)
        {
            self.hits.push(
                "PRIVILEGE",
                call.method.span(),
                format!("unwrap_or_default() on {name}"),
            );
        }
        visit::visit_expr_method_call(self, call);
    }

    fn visit_lit_byte(&mut self, byte: &'ast syn::LitByte) {
        match byte.value() {
            b'"' => self.quotes.push(byte.span()),
            b'{' | b'}' => self.brace = true,
            _ => {}
        }
    }

    fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
        let mut waivers = Vec::new();
        waiver_meta(&attr.meta, &mut waivers);
        if matches!(attr.style, syn::AttrStyle::Inner(_)) && !waivers.is_empty() {
            self.hits.push(
                "WAIVER",
                attr.span(),
                attr.path()
                    .segments
                    .last()
                    .map_or_else(String::new, |segment| segment.ident.to_string()),
            );
        }
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        visit::visit_path(self, &mac.path);
        if let Ok(file) = syn::parse2::<syn::File>(mac.tokens.clone()) {
            self.visit_file(&file);
        } else if let Ok(expressions) =
            Punctuated::<Expr, Token![,]>::parse_terminated.parse2(mac.tokens.clone())
        {
            for expression in &expressions {
                self.visit_expr(expression);
            }
        } else if let Ok(statements) = syn::Block::parse_within.parse2(mac.tokens.clone()) {
            for statement in &statements {
                self.visit_stmt(statement);
            }
        }
    }
}

#[expect(clippy::wildcard_enum_match_arm)]
fn path_string(expr: &Expr) -> bool {
    match expr {
        Expr::Paren(expr) => path_string(&expr.expr),
        Expr::Group(expr) => path_string(&expr.expr),
        Expr::Try(expr) => path_string(&expr.expr),
        Expr::MethodCall(call) => {
            ["to_string_lossy", "to_str", "display"]
                .iter()
                .any(|name| call.method == *name)
                || ([
                    "unwrap",
                    "expect",
                    "unwrap_or",
                    "unwrap_or_default",
                    "unwrap_or_else",
                    "to_owned",
                    "to_string",
                ]
                .iter()
                .any(|name| call.method == *name)
                    && path_string(&call.receiver))
        }
        _ => false,
    }
}

#[derive(Default)]
struct Privileged {
    name: Option<String>,
}

impl<'ast> Visit<'ast> for Privileged {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        if path.segments.len() > 1
            && let Some(segment) = path.segments.last()
            && matches!(
                segment.ident.to_string().as_str(),
                "System" | "Admin" | "Owner"
            )
        {
            self.name = Some(segment.ident.to_string());
        }
        visit::visit_path(self, path);
    }
}

#[expect(clippy::wildcard_enum_match_arm)]
fn privilege_receiver(expr: &Expr) -> Option<String> {
    let name = match expr {
        Expr::Field(field) => match &field.member {
            syn::Member::Named(name) => name.to_string(),
            syn::Member::Unnamed(_) => return None,
        },
        Expr::Path(path) => path.path.segments.last()?.ident.to_string(),
        Expr::Paren(expr) => return privilege_receiver(&expr.expr),
        Expr::MethodCall(call)
            if matches!(
                call.method.to_string().as_str(),
                "clone" | "to_owned" | "as_deref" | "as_ref" | "as_str" | "iter" | "as_slice"
            ) =>
        {
            return privilege_receiver(&call.receiver);
        }
        _ => return None,
    };
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.trim_end_matches('s'),
        "viewer" | "role" | "permission"
    )
    .then_some(name)
}

pub(crate) fn waiver_meta(meta: &Meta, out: &mut Vec<Meta>) {
    if meta.path().is_ident("expect") || meta.path().is_ident("allow") {
        out.push(meta.clone());
    } else if meta.path().is_ident("cfg_attr")
        && let Meta::List(list) = meta
        && let Ok(nested) = list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
    {
        for child in nested.iter().skip(1) {
            waiver_meta(child, out);
        }
    }
}
