use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::process::Command;

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use text_splitter::TextSplitter;

// Maximum characters per chunk; keeps each chunk within a useful slice of LLM context
const CHUNK_SIZE: usize = 512;
// Output dimension of BGE-small-en-v1.5; written into the index header so the runtime can verify
const EMBEDDING_DIM: usize = 384;

// Documentation repositories cloned into docs/ at build time.
// Each tuple is (clone URL, subdirectory name under docs/).
const DOCS_REPOS: &[(&str, &str)] = &[
    ("https://github.com/ubuntu/ubuntu-desktop-documentation", "ubuntu-desktop-documentation"),
    // ("https://github.com/canonical/ubuntu-server-documentation", "ubuntu-server-documentation"),
    // ("https://github.com/canonical/ubuntu-core-docs", "ubuntu-core-docs"),
];

fn main() -> anyhow::Result<()> {
    // Only re-run this build script when build.rs itself changes.
    // We intentionally do NOT watch docs/ here: since we clone into docs/ ourselves,
    // watching it would cause an infinite rebuild loop.
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = env::var("OUT_DIR")?;
    let index_path = Path::new(&out_dir).join("index.bin");

    // In debug builds, skip cloning and embedding entirely — write an empty index
    // so the app compiles and runs (with no RAG context). Use `cargo build --release`
    // for a fully functional binary.
    let profile = env::var("PROFILE").unwrap_or_default();
    if profile == "debug" {
        println!("cargo:warning=Debug build: skipping doc cloning and vectorisation (empty RAG index).");
        write_index(&index_path, EMBEDDING_DIM, &[], &[])?;
        return Ok(());
    }

    clone_or_update_repos("docs")?;

    let chunks = load_chunks("docs");

    if chunks.is_empty() {
        println!("cargo:warning=No markdown files found in docs/; RAG index will be empty.");
        write_index(&index_path, EMBEDDING_DIM, &[], &[])?;
        return Ok(());
    }

    println!(
        "cargo:warning=Building RAG index from {} chunks (BGE-small model downloads ~130 MB on first run)…",
        chunks.len()
    );

    let mut embedder = TextEmbedding::try_new(
        InitOptions::new(EmbeddingModel::BGESmallENV15).with_show_download_progress(true),
    )?;

    let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
    let embeddings = embedder.embed(texts, None)?;

    write_index(&index_path, EMBEDDING_DIM, &chunks, &embeddings)?;

    println!(
        "cargo:warning=RAG index ready: {} vectors ({} dims).",
        chunks.len(),
        EMBEDDING_DIM
    );

    Ok(())
}

// Ensures every repo in DOCS_REPOS is present under `docs_dir`.
// Clones with --depth 1 on first run; does `git pull --ff-only` on subsequent runs.
fn clone_or_update_repos(docs_dir: &str) -> anyhow::Result<()> {
    fs::create_dir_all(docs_dir)?;
    for (url, name) in DOCS_REPOS {
        let dest = Path::new(docs_dir).join(name);
        if dest.join(".git").is_dir() {
            println!("cargo:warning=Updating {name}…");
            let status = Command::new("git")
                .args(["-C", dest.to_str().unwrap(), "pull", "--ff-only", "--quiet"])
                .status()?;
            if !status.success() {
                println!("cargo:warning=Warning: `git pull` failed for {name}; using existing checkout.");
            }
        } else {
            println!("cargo:warning=Cloning {url} into docs/{name}…");
            let status = Command::new("git")
                .args(["clone", "--depth", "1", "--quiet", url, dest.to_str().unwrap()])
                .status()?;
            if !status.success() {
                anyhow::bail!("Failed to clone {url}");
            }
            println!("cargo:warning=Cloned {name}.");
        }
    }
    Ok(())
}

struct Chunk {
    source: String,
    text: String,
}

// Walks `dir` recursively for .md files, strips markdown to plain text, and splits into chunks.
fn load_chunks(dir: &str) -> Vec<Chunk> {
    let mut md_files = Vec::new();
    collect_md_files(Path::new(dir), &mut md_files);
    // Sort for a deterministic index regardless of filesystem ordering
    md_files.sort();

    let splitter = TextSplitter::new(CHUNK_SIZE);
    let mut chunks = Vec::new();

    for file_path in &md_files {
        let raw = match fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        // Inline any MyST {include} directives before parsing so that
        // reused content is present in the plain-text output
        let expanded = expand_includes(&raw, file_path);
        let plain = markdown_to_plain_text(&expanded);
        let source = file_path.display().to_string();
        for chunk_text in splitter.chunks(&plain) {
            let text = chunk_text.trim().to_string();
            if !text.is_empty() {
                chunks.push(Chunk { source: source.clone(), text });
            }
        }
    }

    chunks
}

// Recursively collects .md files and .txt files that are inside a `reuse` directory.
fn collect_md_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(iter) => iter,
        Err(_) => return,
    };
    let in_reuse = dir.file_name().and_then(|n| n.to_str()) == Some("reuse");
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            collect_md_files(&path, out);
        } else {
            match path.extension().and_then(|s| s.to_str()) {
                Some("md") => out.push(path),
                Some("txt") if in_reuse => out.push(path),
                _ => {}
            }
        }
    }
}

// Expands MyST `{include}` directives in `content` by inlining the referenced files.
//
// Supported fence styles:
//   ```{include} path/to/file.txt
//   ```
//   ::::{include} path/to/file.txt
//   ::::
//
// Paths may be relative to the current file or absolute within the repo
// (starting with `/`, resolved from the nearest ancestor `docs/` directory).
fn expand_includes(content: &str, file_path: &Path) -> String {
    // Resolve the docs root as the first ancestor directory named "docs"
    let docs_root: Option<std::path::PathBuf> = file_path
        .ancestors()
        .find(|a| a.file_name().and_then(|n| n.to_str()) == Some("docs"))
        .map(|p| p.to_path_buf());

    let file_dir = file_path.parent().unwrap_or(Path::new("."));

    let mut output = String::with_capacity(content.len());
    let mut lines = content.lines().peekable();

    while let Some(line) = lines.next() {
        // Match opening fences: backtick (3+) or colon (4+) followed by {include}
        let trimmed = line.trim_start();
        if let Some(include_path) = parse_include_directive(trimmed) {
            // Consume lines up to and including the matching closing fence
            let fence_char = trimmed.chars().next().unwrap_or('`');
            let fence_len = trimmed.chars().take_while(|&c| c == fence_char).count();
            let closing: String = std::iter::repeat(fence_char).take(fence_len).collect();
            for next in lines.by_ref() {
                if next.trim() == closing || next.trim().starts_with(&closing) {
                    break;
                }
            }

            // Resolve the included file path
            let resolved = if include_path.starts_with('/') {
                // Absolute path within the repo: resolve from docs root
                docs_root
                    .as_deref()
                    .map(|r| r.join(include_path.trim_start_matches('/')))
            } else {
                Some(file_dir.join(&include_path))
            };

            if let Some(inc_path) = resolved {
                match fs::read_to_string(&inc_path) {
                    Ok(inc) => {
                        output.push_str(&inc);
                        if !inc.ends_with('\n') {
                            output.push('\n');
                        }
                    }
                    Err(_) => {
                        // File not found — emit a note so the chunk at least mentions it
                        output.push_str(&format!("[included file: {include_path}]\n"));
                    }
                }
            }
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }

    output
}

// Returns the include path if `line` is a MyST {include} directive opener, otherwise None.
fn parse_include_directive(line: &str) -> Option<String> {
    // Accept ``` or :::: fences of any length
    let rest = if line.starts_with("```") {
        line.trim_start_matches('`')
    } else if line.starts_with("::::") || line.starts_with(":::") {
        line.trim_start_matches(':')
    } else {
        return None;
    };
    // rest should now be `{include} path`
    let rest = rest.trim();
    let rest = rest.strip_prefix("{include}")?.trim();
    if rest.is_empty() {
        return None;
    }
    Some(rest.to_string())
}

// Strips markdown syntax to plain text using pulldown-cmark's event stream.
fn markdown_to_plain_text(markdown: &str) -> String {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let parser = Parser::new_ext(markdown, opts);
    let mut text = String::new();
    for event in parser {
        match event {
            Event::Text(t) | Event::Code(t) => text.push_str(&t),
            Event::SoftBreak | Event::HardBreak => text.push(' '),
            Event::End(TagEnd::Paragraph)
            | Event::End(TagEnd::Heading { .. })
            | Event::End(TagEnd::Item)
            | Event::End(TagEnd::CodeBlock) => text.push('\n'),
            Event::Start(Tag::CodeBlock(_)) => text.push('\n'),
            _ => {}
        }
    }
    text
}

// Binary index format written to $OUT_DIR/index.bin and embedded by include_bytes! at runtime:
//   dim       u64 le   — embedding dimension (384 for BGE-small)
//   n_chunks  u64 le   — number of entries
//   per entry:
//     src_len u64 le + src_bytes   — source file path
//     txt_len u64 le + txt_bytes   — chunk plain text
//     dim × f32 le                 — embedding vector
fn write_index(
    path: &Path,
    dim: usize,
    chunks: &[Chunk],
    embeddings: &[Vec<f32>],
) -> anyhow::Result<()> {
    let mut f = File::create(path)?;
    f.write_all(&(dim as u64).to_le_bytes())?;
    f.write_all(&(chunks.len() as u64).to_le_bytes())?;
    for (chunk, vec) in chunks.iter().zip(embeddings.iter()) {
        let src = chunk.source.as_bytes();
        f.write_all(&(src.len() as u64).to_le_bytes())?;
        f.write_all(src)?;
        let txt = chunk.text.as_bytes();
        f.write_all(&(txt.len() as u64).to_le_bytes())?;
        f.write_all(txt)?;
        for val in vec {
            f.write_all(&val.to_le_bytes())?;
        }
    }
    Ok(())
}
