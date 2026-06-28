extern crate spin;

fn main() {
    let rwlock = spin::RwLock::new(42);
    println!("{:?}", rwlock);
    {
        let x = rwlock.read();
        println!("{:?}, {:?}", rwlock, *x);
    }
    {
        let x = rwlock.write();
        println!("{:?}, {:?}", rwlock, *x);
    }
}
