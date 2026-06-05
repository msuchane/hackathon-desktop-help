use std::io::{self, BufRead, Write};
use std::time::Duration;

use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};

use crate::conversation::Conversation;
use crate::llm::LlmClient;
use crate::vectordb::{RagStore, TOP_K};

/// Run the interactive terminal chat loop.
///
/// Reads user input from stdin line-by-line, queries the RAG index, and streams
/// the LLM reply to stdout. Exits on EOF (Ctrl-D) or when the user types "exit".
pub async fn run(
    client: LlmClient,
    rag: RagStore,
    system_prompt: &str,
) -> Result<()> {
    let mut rag = rag;
    let mut conversation = Conversation::new(system_prompt.to_string());

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        // Print prompt and flush so it appears before the user types
        print!("> ");
        stdout.flush()?;

        // Read one line in a block so the StdinLock is dropped before any .await calls
        let input = {
            let mut line = String::new();
            match stdin.lock().read_line(&mut line) {
                Ok(0) => break, // EOF (Ctrl-D)
                Ok(_) => line.trim().to_string(),
                Err(e) => {
                    eprintln!("Error reading input: {e}");
                    break;
                }
            }
        };

        if input.is_empty() {
            continue;
        }
        if input.eq_ignore_ascii_case("exit") {
            break;
        }

        // Build the RAG search query (prepends last assistant reply for follow-up context)
        let rag_query = conversation.build_rag_query(&input);
        let query_vec = rag.embed(&rag_query)?;
        let chunks = RagStore::search_with_vec(&rag.table, &rag_query, query_vec, TOP_K).await?;

        let user_content = Conversation::build_augmented_content(&input, &chunks);
        let llm_messages = conversation.build_llm_messages(user_content);

        // Show a spinner until the first token arrives
        let spinner = ProgressBar::new_spinner();
        spinner.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner} Thinking…")
                .unwrap(),
        );
        spinner.enable_steady_tick(Duration::from_millis(80));

        match client.chat(&llm_messages, || spinner.finish_and_clear()).await {
            Ok(reply) => {
                // Tokens were already streamed to stdout; just add a trailing newline
                println!();
                conversation.add_turn(input, reply);
            }
            Err(e) => {
                spinner.finish_and_clear();
                eprintln!("Error: {e}");
                // history is untouched — the failed turn leaves no trace
            }
        }
    }

    Ok(())
}
