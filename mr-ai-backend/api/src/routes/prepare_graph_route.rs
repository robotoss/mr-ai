use std::env;

use codegraph_prep::run::prepare_qdrant_context;

pub async fn prepare_graph() -> &'static str {
    let project_name = env::var("PROJECT_NAME").expect("PROJECT_NAME must be set in environment");
    let result = prepare_qdrant_context(&format!("code_data/{}", project_name));

    match result {
        Ok(_) => {}
        Err(ex) => println!("FAILED: {}", ex),
    }

    "Hello, World!"
}
