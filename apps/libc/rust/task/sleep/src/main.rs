fn main() {
    println!("Sleep for 5 seconds from Rust!");
    let duration = std::time::Duration::from_secs(5);
    std::thread::sleep(duration);
    println!("Woke up from sleep!");
}
