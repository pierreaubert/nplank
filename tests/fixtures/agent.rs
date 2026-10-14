//! Deterministic process fixtures for provider lifecycle and approval protocols.

// Rust guideline compliant 2026-02-21
use serde_json::{Value, json};
use std::io::{self, BufRead, Read, Write};

fn emit(value: Value) {
    println!("{value}");
    io::stdout().flush().expect("stdout");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let provider = args
        .windows(2)
        .find(|a| a[0] == "--fixture-provider")
        .expect("fixture provider")[1]
        .clone();
    if provider == "muse" {
        muse();
        return;
    }
    let mut prompt = String::new();
    io::stdin().read_to_string(&mut prompt).expect("stdin");
    let id = "fixture-conversation";
    let resumed = args.iter().any(|arg| arg == id);
    if provider == "codex" {
        emit(json!({"type":"thread.started","thread_id":id}));
        if prompt == "fail" {
            emit(json!({"type":"turn.failed","error":{"message":"fixture failure"}}));
            return;
        }
        if prompt == "hang" {
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
        emit(
            json!({"type":"item.completed","item":{"type":"agent_message","text":format!("resumed={resumed}: {prompt}")}}),
        );
        emit(json!({"type":"turn.completed"}));
    } else {
        emit(json!({"type":"system","subtype":"init","session_id":id}));
        emit(
            json!({"type":"assistant","message":{"content":[{"type":"text","text":format!("resumed={resumed}: {prompt}")}]}}),
        );
        emit(
            json!({"type":"result","subtype":if prompt == "fail" { "error_during_execution" } else { "success" },"is_error":prompt == "fail","session_id":id}),
        );
    }
}

fn muse() {
    let session = "fixture-muse-session";
    let mut turn = String::new();
    for line in io::stdin().lock().lines() {
        let value: Value = serde_json::from_str(&line.expect("stdin")).expect("json");
        let id = &value["id"];
        let params = &value["params"];
        match value["method"].as_str().unwrap_or("") {
            "initialize" => emit(
                json!({"jsonrpc":"2.0","id":id,"result":{"schema":{"version":1},"sessionDurability":"durable"}}),
            ),
            "session/start" | "session/resume" => {
                emit(json!({"jsonrpc":"2.0","id":id,"result":{"session":{"sessionId":session}}}))
            }
            "turn/start" => {
                assert_eq!(params["ifBusy"], "queue");
                turn = params["input"][0]["text"].as_str().expect("prompt").into();
                emit(json!({"jsonrpc":"2.0","id":id,"result":{"status":"accepted"}}));
                if turn == "approve" {
                    emit(
                        json!({"jsonrpc":"2.0","id":"server-request","method":"approval/request","params":{"approvalId":"approval-1","currentRequirementId":"revision-1","toolName":"write_file","rawArgs":"{\"path\":\"hello.rs\"}","availableChoices":[{"choiceId":"allow","label":"Allow once"},{"choiceId":"deny","label":"Deny"}]}}),
                    );
                } else {
                    finish(&turn);
                }
            }
            "approval/decide" => {
                assert_eq!(params["requirementId"], "revision-1");
                emit(json!({"jsonrpc":"2.0","id":id,"result":{}}));
                emit(json!({"method":"approval/resolved","params":{"approvalId":"approval-1"}}));
                finish(&format!(
                    "{turn}: {}",
                    params["choiceId"].as_str().expect("choice")
                ));
            }
            _ => {}
        }
    }
}

fn finish(prompt: &str) {
    emit(
        json!({"method":"item/started","params":{"item":{"itemId":"message","kind":"agentMessage"}}}),
    );
    let text = format!("Muse: {prompt} €");
    emit(json!({"method":"item/delta","params":{"itemId":"message","field":"text","delta":text}}));
    emit(
        json!({"method":"item/completed","params":{"item":{"itemId":"message","kind":"agentMessage","text":text}}}),
    );
    emit(json!({"method":"turn/completed","params":{"terminal":"completed"}}));
}
