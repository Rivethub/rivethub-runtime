#[expect(typemacro)]
macro_rules! broad {
    () => {
        #[expect(fallback)]
        fn converted(value: Result<u32, ()>) -> Option<u32> {
            value.ok()
        }
    };
}

broad!();

fn main() {
    let _ = converted(Ok(7));
}
