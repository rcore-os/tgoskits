extern crate spin;

fn main() {
    let mutex = spin::Mutex::new(42);
    println!("{:?}", mutex);
    {
        let x = mutex.lock();
        println!("{:?}, {:?}", mutex, *x);
    }
}
