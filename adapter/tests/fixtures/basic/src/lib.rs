pub fn borrowed_message() -> usize {
    let message = String::from("hello from RustOwl");
    let borrowed = &message;
    borrowed.len()
}
