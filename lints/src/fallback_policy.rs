use std::collections::BTreeSet;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned as _;
use syn::visit::{self, Visit};
use syn::{Attribute, Expr, Meta, Stmt, Token};

pub fn violations(source: &str) -> Result<Vec<usize>, syn::Error> {
    let file = syn::parse_file(source)?;
    let mut visitor = Policy::default();
    visitor.visit_file(&file);
    Ok(visitor.invalid)
}

#[derive(Default)]
struct Policy {
    statements: BTreeSet<(usize, usize)>,
    invalid: Vec<usize>,
}

impl<'ast> Visit<'ast> for Policy {
    fn visit_stmt(&mut self, stmt: &'ast Stmt) {
        let attrs = match stmt {
            Stmt::Local(local) => &local.attrs[..],
            Stmt::Macro(mac) => &mac.attrs[..],
            Stmt::Expr(expr, _) => expression_attrs(expr),
            Stmt::Item(_) => &[],
        };
        for attr in attrs {
            let start: proc_macro2::LineColumn = attr.span().start();
            self.statements.insert((start.line, start.column));
        }
        visit::visit_stmt(self, stmt);
    }

    fn visit_attribute(&mut self, attr: &'ast Attribute) {
        let start: proc_macro2::LineColumn = attr.span().start();
        let statement = matches!(attr.style, syn::AttrStyle::Outer)
            && self.statements.contains(&(start.line, start.column));
        if invalid_meta(&attr.meta, statement) {
            self.invalid.push(start.line);
        }
    }
}

fn expression_attrs(expr: &Expr) -> &[Attribute] {
    match expr {
        Expr::MethodCall(expr) => &expr.attrs,
        Expr::Call(expr) => &expr.attrs,
        Expr::Match(expr) => &expr.attrs,
        Expr::If(expr) => &expr.attrs,
        Expr::Block(expr) => &expr.attrs,
        Expr::Return(expr) => &expr.attrs,
        Expr::Await(expr) => &expr.attrs,
        Expr::Try(expr) => &expr.attrs,
        Expr::Path(expr) => &expr.attrs,
        Expr::Lit(expr) => &expr.attrs,
        Expr::Assign(expr) => &expr.attrs,
        Expr::Break(expr) => &expr.attrs,
        Expr::Continue(expr) => &expr.attrs,
        Expr::Loop(expr) => &expr.attrs,
        Expr::While(expr) => &expr.attrs,
        Expr::ForLoop(expr) => &expr.attrs,
        Expr::Macro(expr) => &expr.attrs,
        _ => &[],
    }
}

fn invalid_meta(meta: &Meta, statement: bool) -> bool {
    let Meta::List(list) = meta else { return false };
    let Ok(parts) = list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated) else {
        return false;
    };
    if list.path.is_ident("cfg_attr") {
        return parts
            .iter()
            .skip(1)
            .any(|part| invalid_meta(part, statement));
    }
    if !list.path.is_ident("expect") && !list.path.is_ident("allow") {
        return false;
    }
    let fallback = parts
        .iter()
        .any(|part| part.path().is_ident("fallback") || part.path().is_ident("fallback_waiver"));
    if !fallback {
        return false;
    }
    !statement
        || !list.path.is_ident("expect")
        || parts
            .iter()
            .any(|part| part.path().is_ident("fallback_waiver"))
}
