extern crate spin;

fn main() {
    static VALUE: spin::LazyLock<u32> = spin::LazyLock::new(|| 42);

    println!("{:?}", VALUE);
    println!("{:?}", *VALUE);
}
