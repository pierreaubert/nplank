//! Model-free Rust process fixture for the documented Plank console protocol.

// Rust guideline compliant 2026-02-21
use std::io::{self, BufRead, Write};

fn catalog(controls: &serde_json::Value) -> io::Result<()> {
    let wire = format!("+PLANK_CONTROLS {controls}\n");
    if std::env::args().any(|arg| arg == "--fragmented-controls") {
        let split = "+PLANK_CONTROLS ".len();
        io::stderr().write_all(&wire.as_bytes()[..split])?;
        io::stderr().flush()?;
        println!("Reading engine controls");
        io::stdout().flush()?;
        std::thread::sleep(std::time::Duration::from_millis(40));
        io::stderr().write_all(&wire.as_bytes()[split..])?;
        io::stderr().flush()?;
    } else {
        eprint!("{wire}");
    }
    Ok(())
}

fn main() -> io::Result<()> {
    let mut lines = io::stdin().lock().lines();
    if std::env::args().any(|arg| arg == "--startup-question") {
        eprintln!(
            "+PLANK_QUESTION {}",
            serde_json::json!({"id":"startup-1", "header":"Startup", "question":"Select engine before loading", "multi":false,"options":[{"label":"Continue"}]})
        );
        let answer = lines
            .next()
            .ok_or_else(|| io::Error::other("Missing startup answer"))??;
        let answer: serde_json::Value = serde_json::from_str(&answer)?;
        assert_eq!(answer["id"], "startup-1");
        assert_eq!(answer["type"], "dialog_reply");
        assert_eq!(answer["answer"], "1");
    }
    catalog(&serde_json::json!({"version": 1, "commands": [
        {"name": "/commands", "args": "", "description": "List loaded controls", "source": "builtin"},
        {"name": "/project-review", "args": "<path>", "description": "Review this project's code", "source": "skill"},
        {"name": "/subagent:research", "args": "<task>", "description": "Research helper", "source": "agent"}
    ]}))?;
    eprintln!(
        "+PLANK_TELEMETRY {}",
        serde_json::json!({"type":"status", "status":{
            "state":"idle", "ctx_used":2048, "ctx_size":8192,
            "generated":0, "gen_tps":0.0, "elapsed_secs":0.0, "think":"med"
        }})
    );
    eprintln!(
        "+PLANK_TELEMETRY {}",
        serde_json::json!({"type":"tasks", "completed":1,"total":3,
            "report":"goal: Build the UI (iteration 1/20, active)\n- [2] in_progress: Implement controls\n"})
    );
    eprintln!("+DWARFSTAR_WAITING");
    eprintln!(
        "+PLANK_TELEMETRY {}",
        serde_json::json!({"type":"jobs","snapshot":{
            "running":1,"total":1,"jobs":[{"id":1,"pid":std::process::id(),"elapsed_secs":1.5,
                "state":"running","exit_code":null,"path":"/tmp/job-output"}]
        }})
    );
    let mut turns = 0;
    while let Some(line) = lines.next() {
        let line = line?;
        turns += 1;
        if line == "crash" {
            std::process::exit(7);
        }
        if line == "/export md" {
            assert_eq!(std::env::var("PLANK_GUI_EXPORT_STREAM").as_deref(), Ok("1"));
            for event in [
                serde_json::json!({"phase":"begin", "id":"export-fixture", "name":"transcript.md", "format":"md", "bytes":5,
                    "sha256":"2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"}),
                serde_json::json!({"phase":"chunk", "id":"export-fixture", "offset":0, "text":"he"}),
                serde_json::json!({"phase":"chunk", "id":"export-fixture", "offset":2, "text":"llo"}),
                serde_json::json!({"phase":"end", "id":"export-fixture", "bytes":5}),
            ] {
                eprintln!(
                    "+PLANK_TELEMETRY {}",
                    serde_json::json!({"type":"artifact", "event":event})
                );
            }
            println!("exported session to transcript.md");
            io::stdout().flush()?;
            eprintln!("+DWARFSTAR_WAITING");
            continue;
        }
        if line == "/remote-editor" {
            eprintln!(
                "+PLANK_EDITOR {}",
                serde_json::json!({"id":"remote-editor-1","title":"editor.txt","text":"initial\n"})
            );
            // Another controller resolves the shared editor; no local stdin
            // answer is sent, so only EDITOR_DONE can close its local approval.
            std::thread::sleep(std::time::Duration::from_millis(300));
            std::fs::write("editor.txt", "edited 🦀\n")?;
            eprintln!(
                "+PLANK_EDITOR_DONE {}",
                serde_json::json!({"id":"remote-editor-1"})
            );
            std::thread::sleep(std::time::Duration::from_millis(500));
            eprintln!("+DWARFSTAR_WAITING");
            continue;
        }
        if line == "/remote-turn" {
            // Simulate work arriving from another controller after local idle.
            eprintln!("+DWARFSTAR_WAITING");
            std::thread::sleep(std::time::Duration::from_millis(100));
            eprintln!("+PLANK_BUSY");
            eprintln!(
                "+PLANK_QUESTION {}",
                serde_json::json!({"id":"remote-question", "header":"Remote", "question":"Continue the remote turn?", "multi":false,"options":[{"label":"Continue"}]})
            );
            let answer = lines
                .next()
                .ok_or_else(|| io::Error::other("Missing remote-turn answer"))??;
            let answer: serde_json::Value = serde_json::from_str(&answer)?;
            assert_eq!(answer["id"], "remote-question");
            assert_eq!(answer["type"], "dialog_reply");
            assert_eq!(answer["answer"], "1");
            println!("Remote turn answered explicitly");
            io::stdout().flush()?;
            eprintln!("+DWARFSTAR_WAITING");
            continue;
        }
        if line == "/commands" {
            println!("Loaded 3 engine controls");
            io::stdout().flush()?;
            eprintln!("+DWARFSTAR_WAITING");
            continue;
        }
        if line == "/engines" {
            eprintln!("+PLANK_RESTARTING");
            eprintln!(
                "+PLANK_PROGRESS {}",
                serde_json::json!({"label":"Downloading test-engine", "current":50,"total":100})
            );
            eprintln!(
                "+PLANK_QUESTION {}",
                serde_json::json!({"id":"engine-1","header":"Engine","question":"Choose engine", "multi":false,"options":[{"label":"Old engine"},{"label":"New engine"}]})
            );
            let answer = lines
                .next()
                .ok_or_else(|| io::Error::other("Missing engine choice"))??;
            let answer: serde_json::Value = serde_json::from_str(&answer)?;
            assert_eq!(answer["id"], "engine-1");
            assert_eq!(answer["type"], "dialog_reply");
            assert_eq!(answer["answer"], "2");
            eprintln!(
                "+PLANK_LAUNCH {}",
                serde_json::json!({"args":["--model:new-engine"]})
            );
            catalog(
                &serde_json::json!({"version":1,"commands":[{"name":"/commands","args":"","description":"List controls","source":"builtin"}]}),
            )?;
            println!("Switched engine; saved conversation restored");
            io::stdout().flush()?;
            eprintln!("+DWARFSTAR_WAITING");
            continue;
        }
        if line == "/open editor.txt" {
            eprintln!(
                "+PLANK_EDITOR {}",
                serde_json::json!({"id": "editor-1", "title": "editor.txt", "text": "initial\n"})
            );
            let answer = lines
                .next()
                .ok_or_else(|| io::Error::other("Missing document decision"))??;
            let answer: serde_json::Value = serde_json::from_str(&answer)?;
            assert_eq!(answer["id"], "editor-1");
            assert_eq!(answer["type"], "dialog_reply");
            if let Some(text) = answer["text"].as_str() {
                std::fs::write("editor.txt", text)?;
                println!("Saved editor.txt");
            } else {
                assert!(answer["text"].is_null());
                println!("Editor cancelled");
            }
            eprintln!(
                "+PLANK_EDITOR_DONE {}",
                serde_json::json!({"id":"editor-1"})
            );
            // Resolve the editor while the process remains busy saving its turn.
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        if line == "question" {
            eprintln!(
                "+PLANK_QUESTION {}",
                serde_json::json!({
                    "id": "question-1", "header": "Choose", "question": "Which targets?",
                    "multi": true, "options": [
                        {"label": "Native", "description": "Desktop"},
                        {"label": "Browser", "description": "WASM"}
                    ]
                })
            );
            let answer = lines
                .next()
                .ok_or_else(|| io::Error::other("Missing answer"))??;
            let answer: serde_json::Value = serde_json::from_str(&answer)?;
            assert_eq!(answer["id"], "question-1");
            assert_eq!(answer["type"], "dialog_reply");
            println!("explicit answer: {}", answer["answer"]);
        }
        if line == "/auto-download" {
            eprintln!(
                "+PLANK_PROGRESS {}",
                serde_json::json!({"label":"Downloading test-engine","current":50,"total":100})
            );
            eprintln!(
                "+PLANK_QUESTION {}",
                serde_json::json!({"id":"download-1","header":"Download","question":"Wait, leave, or cancel","multi":false,"options":[{"label":"Leave downloading"}]})
            );
            std::thread::sleep(std::time::Duration::from_millis(300));
            eprintln!(
                "+PLANK_QUESTION_DONE {}",
                serde_json::json!({"id":"download-1"})
            );
            eprintln!("+DWARFSTAR_WAITING");
            continue;
        }
        println!("turn {turns}: {line} €");
        io::stdout().flush()?;
        std::thread::sleep(std::time::Duration::from_millis(150));
        eprintln!("+DWARFSTAR_WAITING");
    }
    Ok(())
}
