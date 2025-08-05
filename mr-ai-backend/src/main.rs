use std::error::Error;

use api;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    println!("Hello, world!");

    api::start();

    Ok(())
}
