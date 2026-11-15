#[expect(typemacro)]
macro_rules! documented {
    ($value:expr) => {{
        #[expect(fallback)]
        let number = $value.ok();
        number
    }};
}

fn main() {
    let _ = documented!(Ok::<u32, ()>(7));
}
