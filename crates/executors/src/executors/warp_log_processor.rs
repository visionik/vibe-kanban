use std::{path::Path, sync::Arc};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use workspace_utils::{log_msg::LogMsg, msg_store::MsgStore};

use crate::logs::{
    utils::{ConversationPatch, EntryIndexProvider},
    ActionType, CommandExitStatus, CommandRunResult, NormalizedEntry, NormalizedEntryType,
    ToolResult, ToolResultValueType, ToolStatus,
};

/// Warp-specific JSON message types
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WarpJson {
    System {
        #[serde(default)]
        event_type: Option<String>,
        #[serde(default)]
        conversation_id: Option<String>,
    },
    Agent {
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        conversation_id: Option<String>,
    },
    ToolCall {
        tool: String,
        #[serde(default)]
        command: Option<String>,
        #[serde(flatten)]
        data: serde_json::Value,
    },
    ToolResult {
        tool: String,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        exit_code: Option<i32>,
        #[serde(default)]
        output: Option<String>,
        #[serde(flatten)]
        data: serde_json::Value,
    },
}

pub struct WarpLogProcessor;

impl WarpLogProcessor {
    /// Process raw logs and convert them to normalized entries
    pub fn process_logs(
        msg_store: Arc<MsgStore>,
        _current_dir: &Path,
        entry_index_provider: EntryIndexProvider,
    ) {
        tokio::spawn(async move {
            let mut stream = msg_store.history_plus_stream();
            let mut buffer = String::new();
            let mut session_id_extracted = false;

            while let Some(Ok(msg)) = stream.next().await {
                let chunk = match msg {
                    LogMsg::Stdout(x) => x,
                    LogMsg::JsonPatch(_)
                    | LogMsg::SessionId(_)
                    | LogMsg::Stderr(_)
                    | LogMsg::Ready => continue,
                    LogMsg::Finished => break,
                };

                buffer.push_str(&chunk);

                // Process complete JSON lines
                for line in buffer
                    .split_inclusive('\n')
                    .filter(|l| l.ends_with('\n'))
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
                {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }

                    match serde_json::from_str::<WarpJson>(trimmed) {
                        Ok(warp_json) => {
                            // Extract session ID if present
                            if !session_id_extracted {
                                if let Some(session_id) = Self::extract_session_id(&warp_json) {
                                    msg_store.push_session_id(session_id);
                                    session_id_extracted = true;
                                }
                            }

                            let patch = Self::normalize_entry(&warp_json, &entry_index_provider);
                            if let Some(patch) = patch {
                                msg_store.push_patch(patch);
                            }
                        }
                        Err(_) => {
                            // Handle non-JSON output as raw system message
                            if !trimmed.is_empty() {
                                let entry = NormalizedEntry {
                                    timestamp: None,
                                    entry_type: NormalizedEntryType::SystemMessage,
                                    content: trimmed.to_string(),
                                    metadata: None,
                                };

                                let patch_id = entry_index_provider.next();
                                let patch =
                                    ConversationPatch::add_normalized_entry(patch_id, entry);
                                msg_store.push_patch(patch);
                            }
                        }
                    }
                }

                // Keep the partial line in the buffer
                buffer = buffer.rsplit('\n').next().unwrap_or("").to_owned();
            }

            // Handle any remaining content in buffer
            if !buffer.trim().is_empty() {
                let entry = NormalizedEntry {
                    timestamp: None,
                    entry_type: NormalizedEntryType::SystemMessage,
                    content: buffer.trim().to_string(),
                    metadata: None,
                };

                let patch_id = entry_index_provider.next();
                let patch = ConversationPatch::add_normalized_entry(patch_id, entry);
                msg_store.push_patch(patch);
            }
        });
    }

    fn extract_session_id(warp_json: &WarpJson) -> Option<String> {
        match warp_json {
            WarpJson::System { conversation_id, .. } => conversation_id.clone(),
            WarpJson::Agent { conversation_id, .. } => conversation_id.clone(),
            _ => None,
        }
    }

    fn normalize_entry(
        warp_json: &WarpJson,
        entry_index_provider: &EntryIndexProvider,
    ) -> Option<json_patch::Patch> {
        match warp_json {
            WarpJson::System { event_type, .. } => {
                let content = match event_type.as_deref() {
                    Some("conversation_started") => "Conversation started".to_string(),
                    Some(event) => format!("System: {}", event),
                    None => "System message".to_string(),
                };

                let entry = NormalizedEntry {
                    timestamp: None,
                    entry_type: NormalizedEntryType::SystemMessage,
                    content,
                    metadata: Some(
                        serde_json::to_value(warp_json).unwrap_or(serde_json::Value::Null),
                    ),
                };

                let idx = entry_index_provider.next();
                Some(ConversationPatch::add_normalized_entry(idx, entry))
            }
            WarpJson::Agent { text, .. } => {
                if let Some(text) = text {
                    let entry = NormalizedEntry {
                        timestamp: None,
                        entry_type: NormalizedEntryType::AssistantMessage,
                        content: text.clone(),
                        metadata: Some(
                            serde_json::to_value(warp_json).unwrap_or(serde_json::Value::Null),
                        ),
                    };

                    let idx = entry_index_provider.next();
                    Some(ConversationPatch::add_normalized_entry(idx, entry))
                } else {
                    None
                }
            }
            WarpJson::ToolCall { tool, command, data } => {
                let tool_name = tool.clone();
                let content = if tool == "run_command" {
                    command.as_deref().unwrap_or("<command>").to_string()
                } else if tool == "read_files" {
                    // Extract file paths from the data
                    if let Some(files) = data.get("files").and_then(|v| v.as_array()) {
                        let paths: Vec<String> = files
                            .iter()
                            .filter_map(|f| f.get("path").and_then(|p| p.as_str()))
                            .map(|p| {
                                // Show just the filename or last 2 path components for readability
                                let parts: Vec<&str> = p.rsplitn(3, '/').collect();
                                if parts.len() >= 2 {
                                    format!("{}/{}", parts[1], parts[0])
                                } else {
                                    parts[0].to_string()
                                }
                            })
                            .collect();
                        
                        if paths.is_empty() {
                            "read_files".to_string()
                        } else if paths.len() == 1 {
                            paths[0].clone()
                        } else {
                            format!("{} files: {}", paths.len(), paths.join(", "))
                        }
                    } else {
                        "read_files".to_string()
                    }
                } else {
                    // For other tools, show a compact representation
                    format!("{}", serde_json::to_string_pretty(data).unwrap_or_default())
                };

                let entry = NormalizedEntry {
                    timestamp: None,
                    entry_type: NormalizedEntryType::ToolUse {
                        tool_name: tool_name.clone(),
                        action_type: if tool == "run_command" {
                            ActionType::CommandRun {
                                command: content.clone(),
                                result: None,
                            }
                        } else {
                            ActionType::Tool {
                                tool_name: tool_name.clone(),
                                arguments: Some(data.clone()),
                                result: None,
                            }
                        },
                        status: ToolStatus::Created,
                    },
                    content,
                    metadata: Some(
                        serde_json::to_value(warp_json).unwrap_or(serde_json::Value::Null),
                    ),
                };

                let idx = entry_index_provider.next();
                Some(ConversationPatch::add_normalized_entry(idx, entry))
            }
            WarpJson::ToolResult {
                tool,
                status,
                exit_code,
                output,
                data,
            } => {
                let tool_name = tool.clone();
                let is_error = status.as_deref() != Some("complete");

                let entry = if tool == "run_command" {
                    let result = CommandRunResult {
                        exit_status: exit_code.map(|code| CommandExitStatus::ExitCode { code }),
                        output: output.clone(),
                    };

                    // Try to get command from data (flattened fields) or use a placeholder
                    let command = data
                        .get("command")
                        .and_then(|v| v.as_str())
                        .unwrap_or("<command>")
                        .to_string();

                    NormalizedEntry {
                        timestamp: None,
                        entry_type: NormalizedEntryType::ToolUse {
                            tool_name: tool_name.clone(),
                            action_type: ActionType::CommandRun {
                                command: command.clone(),
                                result: Some(result),
                            },
                            status: if is_error {
                                ToolStatus::Failed
                            } else {
                                ToolStatus::Success
                            },
                        },
                        content: command,
                        metadata: None,
                    }
                } else {
                    let content = format!("{}", serde_json::to_string_pretty(data).unwrap_or_default());
                    
                    NormalizedEntry {
                        timestamp: None,
                        entry_type: NormalizedEntryType::ToolUse {
                            tool_name: tool_name.clone(),
                            action_type: ActionType::Tool {
                                tool_name: tool_name.clone(),
                                arguments: Some(data.clone()),
                                result: Some(ToolResult {
                                    r#type: ToolResultValueType::Json,
                                    value: data.clone(),
                                }),
                            },
                            status: if is_error {
                                ToolStatus::Failed
                            } else {
                                ToolStatus::Success
                            },
                        },
                        content,
                        metadata: None,
                    }
                };

                let idx = entry_index_provider.next();
                Some(ConversationPatch::add_normalized_entry(idx, entry))
            }
        }
    }
}
