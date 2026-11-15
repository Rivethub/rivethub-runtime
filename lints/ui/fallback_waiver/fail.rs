#![allow(dead_code)]

#[expect(fallback)]
fn too_broad(value: Result<u32, ()>) -> u32 {
    value.unwrap_or(42)
}

#[allow(fallback)]
fn allowed(value: Result<u32, ()>) -> u32 {
    value.unwrap_or(42)
}

fn main() {}
