//! Small utilities shared across review pipeline.

pub fn lang_from_path(path_opt: Option<&str>) -> Option<&'static str> {
    let path = path_opt?;
    if let Some(ext) = path.rsplit('.').next() {
        return match ext {
            "dart" => Some("dart"),
            "kt" | "kts" => Some("kotlin"),
            "java" => Some("java"),
            "ts" => Some("typescript"),
            "tsx" => Some("tsx"),
            "js" => Some("javascript"),
            "swift" => Some("swift"),
            "rs" => Some("rust"),
            _ => None,
        };
    }
    None
}
