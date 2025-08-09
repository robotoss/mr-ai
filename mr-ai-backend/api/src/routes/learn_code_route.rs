use std::env;

use graph_prepare::parce;

pub async fn learn_code() -> &'static str {
    let project_name = env::var("PROJECT_NAME").expect("PROJECT_NAME must be set in environment");

    let result = parce::parse_and_save(&format!("code_data/{}", project_name));

    match result {
        Ok(_) => {}
        Err(ex) => println!("Failed learn {}", ex),
    }

    "Hello, World!"
}
