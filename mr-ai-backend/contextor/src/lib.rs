//! RAG + LLM gateway with a single public function.
//!
//! Public API: [`ask`]. It embeds the question, retrieves top-K context from
//! `rag-store`, runs MMR selection (keeps strong #2), optionally expands with
//! neighbors from the same source/FQN, builds a compact prompt, calls Ollama,
//! and returns the model answer.

mod cfg;
mod error;
mod llm;
mod progress;
mod prompt;
mod select;

mod api_types;

pub use api_types::{AskOptions, QaAnswer, UsedChunk};

pub use error::ContextorError;

pub use progress::{IndicatifProgress, NoopProgress, Progress};

use cfg::ContextorConfig;
use rag_store::{
    RagQuery, RagStore,
    embed::ollama::{OllamaConfig, OllamaEmbedder},
};

/// Ask the LLM with RAG augmentation and get a final answer as plain text.
///
/// This is a convenience wrapper over [`ask_with_opts`] that uses defaults
/// from environment variables.
///
/// # Example
/// ```no_run
/// # use contextor::ask;
/// # #[tokio::main] async fn main() {
/// let answer = ask("Where is `gamesIcon` defined?").await.unwrap();
/// println!("{answer}");
/// # }
/// ```
pub async fn ask(question: &str) -> Result<String, ContextorError> {
    let qa = ask_with_opts(question, AskOptions::default()).await?;
    Ok(qa.answer)
}

/// Ask the LLM with RAG augmentation and get both answer and used context.
///
/// It embeds the question, retrieves top-K, applies MMR, optionally expands
/// by neighbors in the same source/FQN, builds a compact prompt, calls Ollama
/// chat, and returns the final answer together with the context fed to the LLM.
///
/// Any `AskOptions` field set to `0` is replaced by the corresponding value
/// from environment-driven config (`ContextorConfig`).
///
/// # Errors
/// Propagates `ContextorError` from networking, embedding, retrieval, or chat.
///
/// # Example
/// ```no_run
/// # use contextor::{ask_with_opts, AskOptions};
/// # #[tokio::main] async fn main() {
/// let qa = ask_with_opts("Where is gamesIcon defined?",
///                        AskOptions { top_k: 8, context_k: 5 })
///     .await
///     .unwrap();
/// println!("Answer: {}", qa.answer);
/// println!("Context items: {}", qa.context.len());
/// # }
/// ```
pub async fn ask_with_opts(question: &str, opts: AskOptions) -> Result<QaAnswer, ContextorError> {
    let prog = IndicatifProgress::spinner();

    // 1) Load config from env
    prog.message("loading config");
    let gcfg = ContextorConfig::from_env();

    // Resolve effective knobs (0 => use env default)
    let top_k = if opts.top_k == 0 {
        gcfg.initial_top_k
    } else {
        opts.top_k
    };
    let context_k = if opts.context_k == 0 {
        gcfg.context_k
    } else {
        opts.context_k
    };

    // 2) Create facades
    prog.step("creating store and clients");
    let store = RagStore::new(gcfg.make_rag_config())?;
    let emb_cfg = OllamaConfig {
        url: gcfg.ollama_host.clone(),
        model: gcfg.embed_model.clone(),
        dim: gcfg.make_rag_config().embedding_dim.unwrap_or(1024),
    };
    let embedder = OllamaEmbedder::new(emb_cfg);
    let chat = llm::OllamaChat::new(&gcfg.ollama_host, &gcfg.chat_model)?;

    // 3) Retrieve
    prog.step("embedding + retrieving from qdrant");
    let query = RagQuery {
        text: question,
        top_k,
        filter: gcfg.initial_filter.clone(),
    };
    let mut hits = store.rag_context(query, &embedder).await?;

    // 4) MMR selection
    prog.step("MMR selecting context");
    let selected =
        select::mmr_select(question, &embedder, &mut hits, context_k, gcfg.mmr_lambda).await?;

    // 5) Optional neighbor expansion
    let expanded = if gcfg.expand_neighbors {
        select::maybe_expand_neighbors(
            &store,
            &embedder,
            &selected,
            gcfg.neighbor_k,
            gcfg.score_floor,
        )
        .await?
    } else {
        selected
    };

    // 6) Build prompts + chat
    prog.step("building prompts");
    let system_prompt = prompt::DEFAULT_SYSTEM;
    let user_prompt = prompt::build_user_prompt(question, &expanded, gcfg.max_ctx_chars);
    prog.step("chatting with model");
    let answer = chat.chat(system_prompt, &user_prompt).await?;

    // 7) Convert used context for callers
    prog.finish("done");
    let context = expanded
        .into_iter()
        .map(|h| {
            // Prefer snippet if present, otherwise `text`. Clamp for transport/UI.
            let body = if let Some(s) = h.snippet {
                rag_store::record::clamp_snippet(&s, 800, 20)
            } else {
                rag_store::record::clamp_snippet(&h.text, 800, 20)
            };
            api_types::UsedChunk {
                score: h.score,
                source: h.source,
                fqn: h.fqn,
                kind: h.kind,
                text: body,
            }
        })
        .collect();

    Ok(api_types::QaAnswer { answer, context })
}
