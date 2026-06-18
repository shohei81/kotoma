use crate::msg::{TranslatorStatus, UiMsg};
use crate::transcribe::TranscriptLine;
use anyhow::{anyhow, Context, Result};
use crossbeam_channel::{Receiver, Sender};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

const CONTEXT_PAIRS: usize = 3;

struct ContextWindow {
    pairs: VecDeque<(String, String, String)>, // (src_lang, src_text, dst_text)
}

impl ContextWindow {
    fn new() -> Self {
        Self {
            pairs: VecDeque::with_capacity(CONTEXT_PAIRS + 1),
        }
    }

    fn push(&mut self, src_lang: &str, src: &str, dst: &str) {
        self.pairs
            .push_back((src_lang.to_string(), src.to_string(), dst.to_string()));
        while self.pairs.len() > CONTEXT_PAIRS {
            self.pairs.pop_front();
        }
    }

    /// Prior pairs in the same direction, oldest first, as (src, dst).
    /// Same-direction only: mixed directions would contradict the system
    /// prompt and thrash the server's prompt cache.
    fn history_for(&self, src_lang: &str) -> Vec<(&str, &str)> {
        self.pairs
            .iter()
            .filter(|(lang, _, _)| lang == src_lang)
            .map(|(_, s, d)| (s.as_str(), d.as_str()))
            .collect()
    }
}

#[derive(Clone)]
pub struct TranslatorConfig {
    pub binary: PathBuf,
    pub model_path: PathBuf,
    pub port: u16,
    pub n_ctx: u32,
    pub max_new_tokens: u32,
    pub startup_timeout_secs: u64,
    pub log_dir: PathBuf,
}

struct ServerHandle {
    child: Option<Child>,
}

impl ServerHandle {
    fn start(cfg: &TranslatorConfig) -> Result<Self> {
        std::fs::create_dir_all(&cfg.log_dir).ok();
        let log_path = cfg.log_dir.join("llama-server.log");
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("opening {}", log_path.display()))?;
        let log_err = log_file.try_clone()?;

        let child = Command::new(&cfg.binary)
            .arg("--model")
            .arg(&cfg.model_path)
            .arg("--port")
            .arg(cfg.port.to_string())
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--ctx-size")
            .arg(cfg.n_ctx.to_string())
            .arg("--n-gpu-layers")
            .arg("999")
            .arg("--threads")
            .arg("4")
            // Use the model's own chat template so any GGUF (Gemma, Qwen, …)
            // gets its correct prompt format via /v1/chat/completions.
            .arg("--jinja")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(log_err))
            .spawn()
            .with_context(|| {
                format!(
                    "spawning llama-server ({}). Install it with `brew install llama.cpp` or set translator.binary in kotoma.toml.",
                    cfg.binary.display()
                )
            })?;
        info!(pid = child.id(), port = cfg.port, "llama-server spawned");
        Ok(Self { child: Some(child) })
    }
}

impl ServerHandle {
    fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.as_mut().and_then(|c| c.try_wait().ok().flatten())
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
            info!("llama-server stopped");
        }
    }
}

pub fn spawn(cfg: TranslatorConfig, line_rx: Receiver<TranscriptLine>, ui_tx: Sender<UiMsg>) {
    std::thread::spawn(move || {
        let _ = ui_tx.send(UiMsg::TranslatorStatus(TranslatorStatus::Loading));
        match run(&cfg, &line_rx, &ui_tx) {
            Ok(()) => info!("translator thread exiting cleanly"),
            Err(e) => {
                error!("translator thread failed: {e:#}");
                let _ = ui_tx.send(UiMsg::TranslatorStatus(TranslatorStatus::Failed));
                // Drain and drop — the fanout thread already sent NewLine to the UI.
                while line_rx.recv().is_ok() {}
            }
        }
    });
}

const MAX_SERVER_RESTARTS: u32 = 5;

fn run(
    cfg: &TranslatorConfig,
    line_rx: &Receiver<TranscriptLine>,
    ui_tx: &Sender<UiMsg>,
) -> Result<()> {
    let mut context = ContextWindow::new();
    // A line whose translation failed because the server died; retried after restart.
    let mut pending: Option<TranscriptLine> = None;
    let mut restarts = 0u32;

    loop {
        let mut server = ServerHandle::start(cfg)?;
        if let Err(e) = wait_for_health(cfg, &mut server) {
            if restarts >= MAX_SERVER_RESTARTS {
                return Err(e).context("waiting for llama-server readiness");
            }
            restarts += 1;
            warn!(restarts, error = %e, "llama-server not ready — restarting");
            continue;
        }
        let _ = ui_tx.send(UiMsg::TranslatorStatus(TranslatorStatus::Ready));
        info!("llama-server ready");

        loop {
            let line = match pending.take() {
                Some(l) => l,
                None => match line_rx.recv() {
                    Ok(l) => l,
                    Err(_) => return Ok(()), // upstream closed — clean shutdown
                },
            };
            let id = line.id;
            match translate_once(cfg, &line, &context) {
                Ok((translated, off_target)) if !translated.is_empty() => {
                    debug!(id, text = %translated, "translation complete");
                    // Caching an off-target line would feed it back as a
                    // few-shot example and make the leak self-perpetuate —
                    // show it once, but don't let it poison the context.
                    if off_target {
                        warn!(id, text = %translated, "off-target output — not caching to context");
                    } else {
                        context.push(&line.src_lang, &line.text, &translated);
                    }
                    let _ = ui_tx.send(UiMsg::TranslationReady { id, translated });
                    restarts = 0;
                }
                Ok(_) => {
                    debug!(id, "empty translation");
                }
                Err(e) => {
                    if server.exited().is_some() {
                        if restarts >= MAX_SERVER_RESTARTS {
                            return Err(anyhow!(
                                "llama-server keeps crashing (gave up after {MAX_SERVER_RESTARTS} restarts)"
                            ));
                        }
                        restarts += 1;
                        warn!(id, restarts, error = %e, "llama-server died — restarting");
                        let _ = ui_tx.send(UiMsg::TranslatorStatus(TranslatorStatus::Loading));
                        pending = Some(line);
                        break; // restart the server, then retry this line
                    }
                    // Transient (timeout etc.) — drop this line, keep going.
                    warn!(id, error = %e, "translation failed");
                }
            }
        }
    }
}

fn wait_for_health(cfg: &TranslatorConfig, server: &mut ServerHandle) -> Result<()> {
    let url = format!("http://127.0.0.1:{}/health", cfg.port);
    let deadline = Instant::now() + Duration::from_secs(cfg.startup_timeout_secs);
    let mut last_err: Option<String> = None;
    while Instant::now() < deadline {
        // A stale llama-server from a crashed session can hold the port: our
        // child dies on bind, yet /health answers (from the stale server with
        // the wrong model). Fail fast instead of "succeeding" against it.
        if let Some(status) = server.exited() {
            return Err(anyhow!(
                "llama-server exited during startup ({status}) — is port {} already in use \
                 by a stale llama-server? Kill it (`pkill llama-server`) and retry. \
                 See llama-server.log for details.",
                cfg.port
            ));
        }
        match ureq::get(&url).timeout(Duration::from_secs(2)).call() {
            Ok(resp) if resp.status() == 200 => return Ok(()),
            Ok(resp) => last_err = Some(format!("status {}", resp.status())),
            Err(e) => last_err = Some(e.to_string()),
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(anyhow!(
        "llama-server did not become ready within {}s (last: {})",
        cfg.startup_timeout_secs,
        last_err.unwrap_or_else(|| "unknown".into())
    ))
}

fn system_prompt(src_lang: &str) -> &'static str {
    if src_lang == "ja" {
        "You are a professional simultaneous interpreter translating from Japanese to English.\n\
Rules:\n\
- Output ONLY the translation. No explanations, no quotes, no prefixes.\n\
- The output MUST be written in English only. Never output Japanese, Chinese, or any other language.\n\
- If the input is a sentence fragment (no period, cut mid-thought), translate it as a fragment. Do NOT pad or complete.\n\
- Preserve tone: formal stays formal, casual stays casual.\n\
- Keep proper nouns, technical terms, and numbers exact.\n\
- Match punctuation style of the target language."
    } else {
        "You are a professional simultaneous interpreter translating from English to Japanese.\n\
Rules:\n\
- Output ONLY the translation. No explanations, no quotes, no prefixes.\n\
- The output MUST be written in Japanese only. Never output Korean (Hangul), Simplified or Traditional Chinese. Use hiragana, katakana, and Japanese-style kanji; do NOT output Hangul, Chinese-only characters, or Pinyin.\n\
- If the input is a sentence fragment (no period, cut mid-thought), translate it as a fragment. Do NOT pad or complete.\n\
- Preserve tone: formal stays formal, casual stays casual.\n\
- Keep proper nouns, technical terms, and numbers exact.\n\
- Match punctuation style of the target language."
    }
}

/// Generation cap proportional to the input — short fragments never need the
/// full budget, and a tight cap stops repetition loops from burning the GPU.
fn max_tokens_for(text: &str, cap: u32) -> u32 {
    let n = text.chars().count() as u32;
    (n * 2 + 16).clamp(48, cap.max(48))
}

/// Characters that exist only as Simplified Chinese forms — never in Japanese.
const SIMPLIFIED_ONLY: &[char] = &[
    '们', '这', '说', '话', '时', '现', '见', '关', '车', '长', '门', '问', '读', '汉', '译',
    '应', '给', '让', '还', '么', '吗', '呢', '吧', '头', '买', '卖', '为', '发', '样', '边',
    '过', '别', '帮', '请',
];

/// Any Hangul (syllables, conjoining/compatibility Jamo) — Korean never belongs
/// in either translation direction, so it is always a leak.
fn has_hangul(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(c as u32,
            0xAC00..=0xD7A3 | 0x1100..=0x11FF | 0x3130..=0x318F | 0xA960..=0xA97F | 0xD7B0..=0xD7FF)
    })
}

/// Heuristic: the output is in the wrong language for the target. Catches
/// Korean (Hangul) in either direction and Chinese in the Japanese direction.
fn looks_off_target(target_is_ja: bool, text: &str) -> bool {
    // A Korean leak self-perpetuates once it enters the context history, so
    // flag it regardless of direction.
    if has_hangul(text) {
        return true;
    }
    if !target_is_ja {
        return false;
    }
    if text.chars().any(|c| SIMPLIFIED_ONLY.contains(&c)) {
        return true;
    }
    let has_kana = text
        .chars()
        .any(|c| matches!(c as u32, 0x3040..=0x30FF));
    let han = text
        .chars()
        .filter(|c| matches!(*c as u32, 0x4E00..=0x9FFF))
        .count();
    // Real Japanese sentences of any length virtually always contain kana.
    !has_kana && han >= 6
}

fn chat_request(
    port: u16,
    messages: &serde_json::Value,
    max_tokens: u32,
    temperature: f32,
) -> Result<String> {
    let url = format!("http://127.0.0.1:{}/v1/chat/completions", port);
    let body = serde_json::json!({
        "messages": messages,
        "max_tokens": max_tokens,
        "temperature": temperature,
        "top_p": 0.9,
        "cache_prompt": true
    });
    let resp: serde_json::Value = ureq::post(&url)
        .timeout(Duration::from_secs(30))
        .send_json(body)?
        .into_json()?;
    let text = resp
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    Ok(text)
}

/// Returns `(text, off_target)`. `off_target` is true when the text is still in
/// the wrong language after the corrective retry — the caller must keep it out
/// of the context history so the leak does not become a few-shot example.
fn translate_once(
    cfg: &TranslatorConfig,
    line: &TranscriptLine,
    context: &ContextWindow,
) -> Result<(String, bool)> {
    let target_is_ja = line.src_lang != "ja";

    // Static system prompt + history as real chat turns. The system prompt
    // and old pairs form a stable prefix, so the server's prompt cache
    // (cache_prompt) actually hits instead of reprocessing every request.
    let mut messages: Vec<serde_json::Value> = Vec::with_capacity(2 * CONTEXT_PAIRS + 2);
    messages.push(serde_json::json!({
        "role": "system",
        "content": system_prompt(&line.src_lang)
    }));
    for (src, dst) in context.history_for(&line.src_lang) {
        messages.push(serde_json::json!({"role": "user", "content": src}));
        messages.push(serde_json::json!({"role": "assistant", "content": dst}));
    }
    messages.push(serde_json::json!({"role": "user", "content": line.text}));

    let cap = max_tokens_for(&line.text, cfg.max_new_tokens);
    let msgs = serde_json::Value::Array(messages.clone());
    let text = chat_request(cfg.port, &msgs, cap, 0.2)?;

    if !looks_off_target(target_is_ja, &text) {
        return Ok((text, false));
    }

    // One corrective retry: keep the cached prefix intact and append the bad
    // answer + a fix-it turn. Higher temperature breaks the greedy-decoding rut.
    warn!(id = line.id, text = %text, "off-target language leak detected — retrying");
    let fix = if target_is_ja {
        "That was not Japanese. Rewrite it as natural Japanese using hiragana, katakana, and Japanese kanji — never Korean or Chinese. Output only the Japanese translation."
    } else {
        "That was not English. Rewrite it as natural English. Output only the English translation."
    };
    messages.push(serde_json::json!({"role": "assistant", "content": text}));
    messages.push(serde_json::json!({"role": "user", "content": fix}));
    let msgs = serde_json::Value::Array(messages);
    let retry = chat_request(cfg.port, &msgs, cap, 0.7)?;
    if retry.is_empty() {
        return Ok((text, true));
    }
    let still_off = looks_off_target(target_is_ja, &retry);
    Ok((retry, still_off))
}

/// Tail of the transcript fed to the summarizer — must stay well inside the
/// server's context window (n_ctx defaults to 8192).
const SUMMARY_MAX_CHARS: usize = 8_000;

pub struct Summary {
    pub ja: String,
    /// English rendering of the same bullets; None when that step failed.
    pub en: Option<String>,
}

/// Summarize a finished session using the already-running llama-server:
/// Japanese bullets first, then the same bullets translated to English so
/// both sides of a bilingual meeting can read the recap. Called from the
/// quit path; any failure is non-fatal.
pub fn summarize(port: u16, lines: &[TranscriptLine]) -> Result<Summary> {
    let mut transcript = String::new();
    for l in lines {
        transcript.push_str(&format!("[{}] {}\n", l.started_at.format("%H:%M"), l.text));
    }
    let total = transcript.chars().count();
    if total > SUMMARY_MAX_CHARS {
        transcript = transcript
            .chars()
            .skip(total - SUMMARY_MAX_CHARS)
            .collect();
    }

    let messages = serde_json::json!([
        {"role": "system", "content": "あなたは議事録アシスタントです。与えられた会話の書き起こしを日本語で要約してください。\n- 主なトピックと決定事項を3〜6点の箇条書きで\n- 各項目は「- 」で始まる1行で簡潔に\n- 出力は箇条書きのみ。前置きや説明は不要"},
        {"role": "user", "content": transcript}
    ]);
    let ja = chat_request(port, &messages, 256, 0.3)?;
    if ja.is_empty() {
        return Ok(Summary { ja, en: None });
    }

    // Translating the finished bullets (instead of re-summarizing in English)
    // keeps both sections saying the same thing.
    let messages = serde_json::json!([
        {"role": "system", "content": "Translate the following Japanese meeting-summary bullet points into English.\n- Keep the bullet-list format (one \"- \" item per line)\n- Output only the translated bullets, nothing else"},
        {"role": "user", "content": ja}
    ]);
    let en = match chat_request(port, &messages, 256, 0.2) {
        Ok(s) if !s.is_empty() => Some(s),
        Ok(_) => None,
        Err(e) => {
            warn!(error = %e, "english summary failed — keeping Japanese only");
            None
        }
    };
    Ok(Summary { ja, en })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_tokens_has_floor_scale_and_cap() {
        assert_eq!(max_tokens_for("hi", 512), 48); // floor
        assert_eq!(max_tokens_for(&"あ".repeat(100), 512), 216); // 100*2+16
        assert_eq!(max_tokens_for(&"a".repeat(1000), 512), 512); // cap
    }

    #[test]
    fn simplified_chars_flag_chinese_leak() {
        assert!(looks_like_chinese(true, "我们说这个"));
        assert!(!looks_like_chinese(true, "私たちはこれについて話します"));
    }

    #[test]
    fn kana_free_long_han_run_is_suspicious() {
        assert!(looks_like_chinese(true, "今天天气很好我想去公园散步"));
    }

    #[test]
    fn english_target_never_flags() {
        assert!(!looks_like_chinese(false, "我们说这个"));
    }

    #[test]
    fn context_history_filters_by_direction() {
        let mut c = ContextWindow::new();
        c.push("en", "hello", "こんにちは");
        c.push("ja", "元気です", "I'm fine");
        c.push("en", "bye", "さようなら");
        assert_eq!(
            c.history_for("en"),
            vec![("hello", "こんにちは"), ("bye", "さようなら")]
        );
        assert_eq!(c.history_for("ja"), vec![("元気です", "I'm fine")]);
    }

    #[test]
    fn context_window_keeps_only_recent_pairs() {
        let mut c = ContextWindow::new();
        for i in 0..10 {
            c.push("en", &format!("s{i}"), &format!("d{i}"));
        }
        assert_eq!(c.pairs.len(), CONTEXT_PAIRS);
        assert_eq!(c.pairs.front().unwrap().1, "s7");
    }
}
