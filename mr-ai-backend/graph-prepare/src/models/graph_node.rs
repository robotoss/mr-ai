#[derive(Debug, serde::Deserialize)]
pub struct GraphNode {
    pub id: usize,
    pub name: String,
    #[serde(rename = "type")]
    pub node_type: String, // "file","class","function","method",...
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
}
