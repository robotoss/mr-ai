use std::env;

pub fn start() {
    let project_name = env::var("PROJECT_NAME").expect("PROJECT_NAME must be set in environment");

    println!("{project_name}")
}
