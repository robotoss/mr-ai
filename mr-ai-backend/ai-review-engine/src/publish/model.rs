/// Target of a comment inside a change request.
///
/// This abstraction allows the review engine to express intent without
/// dealing with provider-specific APIs.
#[derive(Debug, Clone)]
pub enum CommentTarget {
    /// Global note on the MR/PR without any file/line association.
    Global,
    /// Comment associated with a specific file but not a concrete line.
    File { path: String },
    /// Comment attached to a specific 1-based line in a file diff.
    Line { path: String, line: u32 },
}

/// Draft comment prepared by the review engine.
///
/// A draft comment is a pure data structure which does not know anything
/// about transport or provider details.
#[derive(Debug, Clone)]
pub struct DraftComment {
    /// Comment body in markdown format.
    pub body: String,
    /// Target location of the comment.
    pub target: CommentTarget,
}

/// Result of successfully publishing a comment.
///
/// This structure returns minimal information which is useful for logging
/// and potential later updates (for example, editing or deleting comments).
#[derive(Debug, Clone)]
pub struct PublishedComment {
    /// Target of the comment (copied from the draft).
    pub target: CommentTarget,
    /// Provider-specific identifier of the created comment, if available.
    pub provider_comment_id: Option<String>,
}
