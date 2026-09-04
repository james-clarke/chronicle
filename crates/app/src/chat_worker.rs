use std::path::Path;
use std::time::Instant;

use crate::status::init_logging;
use anyhow::bail;
use chronicle_core::chat::Budget;
use chronicle_core::config::Config;
use chronicle_core::models_config::ModelsConfig;
use chronicle_derive::text::{JobKind, Request, TextBackend};
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

/// Context budget for a cloud-routed chat: the whole day's rows and totals
/// fit with room to spare; local stays at `digest::MAX_TOKENS`.
const CLOUD_CONTEXT_TOKENS: usize = 24_000;
const CLOUD_HISTORY_TURNS: usize = 20;

/// Who answers: the resident local session or a cloud backend (m31).
enum ChatEngine<'m> {
    Local(chronicle_derive::ChatSession<'m>),
    Cloud {
        name: &'m str,
        model: &'m str,
        backend: &'m dyn TextBackend,
    },
}

impl ChatEngine<'_> {
    fn answer(
        &mut self,
        conn: &rusqlite::Connection,
        history: &[(String, String)],
        context: &str,
        question: &str,
        on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<String> {
        match self {
            ChatEngine::Local(session) => session.answer(history, context, question, on_token),
            ChatEngine::Cloud {
                name,
                model,
                backend,
            } => {
                let user = format!("DATA:\n{context}\n\nQuestion: {question}");
                let req = Request {
                    job: JobKind::Chat,
                    system: Some(chronicle_derive::chat::SYSTEM_PROMPT.trim()),
                    user: &user,
                    history,
                    schema: None,
                    max_output: 0,
                };
                let c = backend.complete(&req, on_token)?;
                // Chat has no queued job; a done row keeps the egress line
                // and the daily cap honest.
                if let Err(e) = chronicle_core::storage::insert_done_ai_job(
                    conn,
                    "chat",
                    name,
                    i64::from(c.input_tokens),
                    i64::from(c.output_tokens),
                    chronicle_derive::cloud::cost_usd(model, &c),
                ) {
                    tracing::error!("chat usage row failed: {e}");
                }
                Ok(c.text)
            }
        }
    }
}

/// One JSON object per line, both directions, over the chat worker's stdio.
pub(crate) mod chatproto {
    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(tag = "t", rename_all = "snake_case")]
    pub enum ClientMsg {
        Ask {
            ask: String,
        },
        /// Drop model history and re-seed from another conversation.
        /// `task_id` scopes retrieval to that task's workspace (m16).
        Switch {
            conversation_id: i64,
            #[serde(default)]
            task_id: Option<i64>,
        },
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(tag = "t", rename_all = "snake_case")]
    pub enum WorkerMsg {
        /// Model loaded; worker accepts questions.
        Ready,
        /// Sent once per question, before the first token: what the answer
        /// was retrieved from, for the panel's "Read 14 blocks ·
        /// Thu 3 Sep 08:00–14:56" footer.
        Context {
            blocks: usize,
            start_ms: Option<i64>,
            end_ms: Option<i64>,
            rows: Vec<String>,
        },
        Tok {
            text: String,
        },
        Done,
        Err {
            message: String,
        },
    }
}

/// Warm chat worker, spawned by the UI when the chat panel opens and killed
/// when it closes. The model stays resident between questions; retrieval is
/// local-DB only (time-ref range or FTS, in chronicle_core::chat).
pub(crate) fn chat_worker(
    data_dir: &Path,
    mut conversation_id: i64,
    mut task_scope: Option<i64>,
) -> anyhow::Result<()> {
    use std::io::{BufRead, Write};

    use chatproto::{ClientMsg, WorkerMsg};
    use chronicle_core::storage;

    let _guard = init_logging(data_dir)?;
    let mut stdout = std::io::stdout();
    let mut send = move |msg: &WorkerMsg| {
        let mut line = serde_json::to_string(msg).expect("worker msg serializes");
        line.push('\n');
        // A dead pipe means the panel is gone; exiting quietly is correct.
        if stdout
            .write_all(line.as_bytes())
            .and_then(|()| stdout.flush())
            .is_err()
        {
            std::process::exit(0);
        }
    };

    let config = Config::load(&data_dir.join("config.toml"))?;
    let conn = storage::open(&data_dir.join("chronicle.db"))?;
    // m31: chat routed to a cloud backend skips the local model entirely
    // and gets the whole conversation plus a far larger context budget.
    let models = ModelsConfig::load(data_dir).unwrap_or_else(|e| {
        tracing::error!("models.toml unreadable, chat runs local: {e:#}");
        ModelsConfig::default()
    });
    let cloud =
        models.route_for("chat").and_then(|(name, cfg)| {
            match chronicle_derive::cloud::build(name, cfg) {
                Ok(b) => Some((name.to_owned(), cfg.model.clone(), b)),
                Err(e) => {
                    tracing::error!("chat backend {name} unusable, running local: {e:#}");
                    None
                }
            }
        });
    let local_model;
    let mut engine = match &cloud {
        Some((name, model, backend)) => {
            tracing::info!(backend = %name, model = %model, "chat on cloud backend");
            ChatEngine::Cloud {
                name: name.as_str(),
                model: model.as_str(),
                backend: backend.as_ref(),
            }
        }
        None => {
            let Some(model_path) =
                chronicle_derive::model::resolve(config.model_path.as_deref(), data_dir)
            else {
                send(&WorkerMsg::Err {
                    message: "no model available; run `chronicle model pull`".into(),
                });
                bail!("no model available")
            };
            local_model = match chronicle_derive::ChatModel::load(&model_path) {
                Ok(m) => m,
                Err(e) => {
                    send(&WorkerMsg::Err {
                        message: format!("model load failed: {e:#}"),
                    });
                    return Err(e);
                }
            };
            ChatEngine::Local(local_model.session()?)
        }
    };
    let budget = match &engine {
        ChatEngine::Cloud { .. } => Budget::for_tokens(CLOUD_CONTEXT_TOKENS),
        ChatEngine::Local(_) => Budget::local(),
    };
    let history_msgs = match &engine {
        ChatEngine::Cloud { .. } => 2 * CLOUD_HISTORY_TURNS,
        ChatEngine::Local(_) => 2 * 3,
    };

    // Conversation tail so follow-up questions keep working across reopens.
    let seed_history = |conversation_id: i64| -> Vec<(String, String)> {
        let mut history = Vec::new();
        if let Ok(messages) = storage::recent_chat_messages(&conn, conversation_id, history_msgs) {
            let mut pending_user: Option<String> = None;
            for (role, content) in messages {
                match role.as_str() {
                    "user" => pending_user = Some(content),
                    _ => {
                        if let Some(q) = pending_user.take() {
                            history.push((q, content));
                        }
                    }
                }
            }
        }
        history
    };
    let mut history = seed_history(conversation_id);
    send(&WorkerMsg::Ready);
    tracing::info!("chat worker ready");

    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let ask = match serde_json::from_str::<ClientMsg>(&line) {
            Ok(ClientMsg::Ask { ask }) => ask,
            Ok(ClientMsg::Switch {
                conversation_id: id,
                task_id,
            }) => {
                conversation_id = id;
                task_scope = task_id;
                history = seed_history(id);
                continue;
            }
            Err(_) => continue,
        };
        let ask = ask.trim().to_owned();
        if ask.is_empty() {
            continue;
        }
        if let Err(e) =
            storage::insert_chat_message(&conn, Timestamp::now(), conversation_id, "user", &ask)
        {
            tracing::error!("chat message insert failed: {e}");
        }
        let (context, info) = match match task_scope {
            Some(task_id) => chronicle_core::chat::build_task_context_with(
                &conn,
                task_id,
                &TimeZone::system(),
                budget,
            )
            .map(|ctx| (ctx, chronicle_core::chat::ChatContextInfo::default())),
            None => chronicle_core::chat::build_context_with(&conn, &ask, &Zoned::now(), budget),
        } {
            Ok(built) => built,
            Err(e) => {
                send(&WorkerMsg::Err {
                    message: format!("retrieval failed: {e}"),
                });
                continue;
            }
        };
        // A task-scoped question builds its context from the task itself and
        // carries no rows; the panel shows no footer rather than "0 blocks".
        if !info.rows.is_empty() {
            send(&WorkerMsg::Context {
                blocks: info.blocks,
                start_ms: info.start_ms,
                end_ms: info.end_ms,
                rows: info.rows,
            });
        }
        let t0 = Instant::now();
        match engine.answer(&conn, &history, &context, &ask, &mut |piece| {
            send(&WorkerMsg::Tok { text: piece.into() });
        }) {
            Ok(answer) => {
                send(&WorkerMsg::Done);
                tracing::info!(secs = t0.elapsed().as_secs_f64(), "chat answer done");
                if let Err(e) = storage::insert_chat_message(
                    &conn,
                    Timestamp::now(),
                    conversation_id,
                    "assistant",
                    &answer,
                ) {
                    tracing::error!("chat message insert failed: {e}");
                }
                history.push((ask, answer));
            }
            Err(e) => {
                tracing::error!("chat inference failed: {e:#}");
                send(&WorkerMsg::Err {
                    message: format!("inference failed: {e:#}"),
                });
            }
        }
    }
    Ok(())
}
