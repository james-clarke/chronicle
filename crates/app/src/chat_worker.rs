use std::path::Path;
use std::time::Instant;

use crate::status::init_logging;
use anyhow::bail;
use chronicle_core::config::Config;
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

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
        Tok {
            text: String,
        },
        Done,
        Err {
            message: String,
        },
    }

    /// Sidecar to [`WorkerMsg`], sent once before the first token: what the
    /// answer was retrieved from, for the panel's "Read 14 blocks ·
    /// Thu 3 Sep 08:00–14:56" footer. Its own enum until the panel matches
    /// on it — an unknown `t` fails to deserialize there and is skipped.
    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(tag = "t", rename_all = "snake_case")]
    pub enum WorkerNote {
        Context {
            blocks: usize,
            start_ms: Option<i64>,
            end_ms: Option<i64>,
            rows: Vec<String>,
        },
    }
}

fn send_note(note: &chatproto::WorkerNote) {
    use std::io::Write;

    let mut line = serde_json::to_string(note).expect("worker note serializes");
    line.push('\n');
    let mut stdout = std::io::stdout();
    let _ = stdout
        .write_all(line.as_bytes())
        .and_then(|()| stdout.flush());
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
    let Some(model_path) = chronicle_derive::model::resolve(config.model_path.as_deref(), data_dir)
    else {
        send(&WorkerMsg::Err {
            message: "no model available; run `chronicle model pull`".into(),
        });
        bail!("no model available")
    };
    let model = match chronicle_derive::ChatModel::load(&model_path) {
        Ok(m) => m,
        Err(e) => {
            send(&WorkerMsg::Err {
                message: format!("model load failed: {e:#}"),
            });
            return Err(e);
        }
    };
    let mut session = model.session()?;

    // Conversation tail so follow-up questions keep working across reopens.
    let seed_history = |conversation_id: i64| -> Vec<(String, String)> {
        let mut history = Vec::new();
        if let Ok(messages) = storage::recent_chat_messages(&conn, conversation_id, 2 * 3) {
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
            Some(task_id) => {
                chronicle_core::chat::build_task_context(&conn, task_id, &TimeZone::system())
                    .map(|ctx| (ctx, chronicle_core::chat::ChatContextInfo::default()))
            }
            None => chronicle_core::chat::build_context(&conn, &ask, &Zoned::now()),
        } {
            Ok(built) => built,
            Err(e) => {
                send(&WorkerMsg::Err {
                    message: format!("retrieval failed: {e}"),
                });
                continue;
            }
        };
        send_note(&chatproto::WorkerNote::Context {
            blocks: info.blocks,
            start_ms: info.start_ms,
            end_ms: info.end_ms,
            rows: info.rows,
        });
        let t0 = Instant::now();
        match session.answer(&history, &context, &ask, &mut |piece| {
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
