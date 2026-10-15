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

fn busy_job_output() -> io::Result<()> {
    let mailbox = std::env::var_os("PLANK_GUI_JOB_CONTROL")
        .ok_or_else(|| io::Error::other("Missing output control mailbox"))?;
    let mut page = serde_json::json!({"id":1,"page":1,"total":4,"start":0,"end":4,
        "text":"live","lossy":false,"tail":true,"following":true,"observer":"fixture-observer"});
    eprintln!(
        "+PLANK_TELEMETRY {}",
        serde_json::json!({"type":"metrics","snapshot":{"kind":"job_output","snapshot":page}})
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    // Deliberately never read stdin while the main turn is blocked.
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::other("Output control fixture timed out"));
        }
        if page["following"] == true
            && std::fs::read(&mailbox)
                .ok()
                .and_then(|body| serde_json::from_slice::<serde_json::Value>(&body).ok())
                .is_some_and(|request| request["observer"] == page["observer"])
        {
            page["following"] = false.into();
            eprintln!(
                "+PLANK_TELEMETRY {}",
                serde_json::json!({"type":"metrics","snapshot":{"kind":"job_output","snapshot":page}})
            );
        }
        if std::path::Path::new("release-main").exists() {
            assert_eq!(page["following"], false);
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn busy_focus() -> io::Result<()> {
    let mailbox = std::path::PathBuf::from(
        std::env::var_os("PLANK_GUI_FOCUS_CONTROL")
            .ok_or_else(|| io::Error::other("Missing private focus mailbox"))?,
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut previous = None;
    let mut changes = 0;
    // Main input is blocked; focus must arrive independently of stdin polling.
    while !std::path::Path::new("release-focus").exists() {
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::other("Focus fixture timed out"));
        }
        let focused = nplank::guifocus::read_focused(&mailbox, nplank::guifocus::now_ms());
        if previous != Some(focused) {
            changes += 1;
            println!("Focus update {changes}: {focused}");
            io::stdout().flush()?;
            previous = Some(focused);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
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
    let busy_aside = std::env::args().any(|arg| arg == "--busy-aside");
    let busy_live = std::env::args().any(|arg| arg == "--busy-live-control");
    let mut controls = serde_json::json!({"version": 1, "commands": [
        {"name": "/commands", "args": "", "description": "List loaded controls", "source": "builtin"},
        {"name": "/project-review", "args": "<path>", "description": "Review this project's code", "source": "skill"},
        {"name": "/subagent:research", "args": "<task>", "description": "Research helper", "source": "agent"}
    ]});
    if busy_aside {
        controls["features"] = serde_json::json!([{"name":"console_asides","label":"Console side questions","available":true,"detail":"Independent side-question input."}]);
    }
    if busy_live {
        controls["features"] = serde_json::json!([{"name":"console_live_controls","label":"Live controls","available":true,"detail":"Independent setting input"}, {"name":"live_hints","label":"Hints","available":true,"detail":"Hints independent of main prompt"}, {"name":"live_notify","label":"Notify","available":true,"detail":"Independent notification policy"}]);
        controls["settings"] = serde_json::json!({"model":"fixture", "temperature":0.5,"power_percent":100,"speculative":false,"speculation_available":false,"micro_compaction":true,"loop_guard":true,"steering":null});
    }
    if std::env::args().any(|arg| arg == "--busy-job-output") {
        controls["features"] = serde_json::json!([{"name":"job_output_cancel","label":"Independent output stop","available":true,"detail":"Observer-scoped output control."}]);
    }
    if std::env::args().any(|arg| arg == "--focus-control") {
        controls["features"] = serde_json::json!([{"name":"gui_focus","label":"Graphical notification focus","available":true,"detail":"Expiring focus mailbox."}]);
    }
    let framed = !std::env::args().any(|arg| arg == "--legacy-prompts");
    let editor_stream = std::env::args().any(|arg| arg == "--editor-stream");
    if controls["features"].is_null() {
        controls["features"] = serde_json::json!([]);
    }
    if framed {
        controls["features"].as_array_mut().unwrap().push(serde_json::json!({
            "name":"console_prompts","label":"Framed input","available":true,"detail":"Opaque submissions"
        }));
    }
    if editor_stream {
        controls["features"].as_array_mut().unwrap().push(serde_json::json!({
            "name":"editor_stream_save","label":"Chunked saves","available":true,"detail":"Fixture"
        }));
    }
    catalog(&controls)?;
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
        let line = if framed {
            let value: serde_json::Value = serde_json::from_str(&line)?;
            assert_eq!(value["type"], "prompt");
            value["text"].as_str().unwrap().to_owned()
        } else {
            line
        };
        turns += 1;
        if matches!(line.trim(), "/quit" | "/exit") {
            println!("Final output before exit");
            io::stdout().flush()?;
            if !std::env::args().any(|arg| arg == "--exit-without-marker") {
                eprintln!("+PLANK_EXITING");
            }
            if std::env::args().any(|arg| arg == "--fail-exit") {
                std::process::exit(7);
            }
            return Ok(());
        }
        #[cfg(feature = "gui")]
        if matches!(line.as_str(), "/buddy on" | "/buddy off") {
            let snapshot = if line == "/buddy on" {
                let rgba = image::RgbaImage::from_pixel(2, 2, image::Rgba([200, 100, 50, 128]));
                let mut encoded = std::io::Cursor::new(Vec::new());
                rgba.write_to(&mut encoded, image::ImageFormat::Png)
                    .map_err(io::Error::other)?;
                Some(nplank::buddyframe::Frame {
                    revision: turns,
                    png: encoded.into_inner(),
                })
            } else {
                None
            };
            eprintln!(
                "+PLANK_TELEMETRY {}",
                serde_json::json!({"type":"buddy", "snapshot":snapshot})
            );
            println!("Buddy control completed");
            eprintln!("+DWARFSTAR_WAITING");
            continue;
        }
        if line == "main-with-live-controls" && busy_live {
            println!("Live control main started");
            io::stdout().flush()?;
            for expected in [
                nplank::livecontrol::LiveControl::MicroCompaction(false),
                nplank::livecontrol::LiveControl::LoopGuard(false),
                nplank::livecontrol::LiveControl::Hints(true),
                nplank::livecontrol::LiveControl::Notifications(
                    nplank::livecontrol::NotifyMode::Never,
                ),
            ] {
                let frame = lines
                    .next()
                    .ok_or_else(|| io::Error::other("Missing live control"))??;
                let frame: serde_json::Value = serde_json::from_str(&frame)?;
                assert_eq!(frame["type"], "live_control");
                let request: nplank::livecontrol::LiveControl =
                    serde_json::from_value(frame["request"].clone())?;
                assert_eq!(request, expected);
                match request {
                    nplank::livecontrol::LiveControl::MicroCompaction(enabled) => {
                        controls["settings"]["micro_compaction"] = enabled.into()
                    }
                    nplank::livecontrol::LiveControl::LoopGuard(enabled) => {
                        controls["settings"]["loop_guard"] = enabled.into()
                    }
                    nplank::livecontrol::LiveControl::Hints(enabled) => {
                        controls["settings"]["hints"] = enabled.into()
                    }
                    nplank::livecontrol::LiveControl::Notifications(mode) => {
                        controls["settings"]["notify_mode"] = mode.as_str().into()
                    }
                }
                catalog(&controls)?;
            }
            eprintln!("+PLANK_NOTIFY {{broken");
            eprintln!(
                "+PLANK_NOTIFY {}",
                serde_json::json!({"title":"Ignored","body":"Malformed mode","mode":"unknown"})
            );
            eprintln!(
                "+PLANK_NOTIFY {}",
                serde_json::json!({"title":"Local completion","body":"Local fixture notice","mode":"never"})
            );
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !std::path::Path::new("release-live-control").exists() {
                if std::time::Instant::now() >= deadline {
                    return Err(io::Error::other("Live control fixture timed out"));
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            println!("Live control main completed");
            eprintln!("+DWARFSTAR_WAITING");
            continue;
        }
        if line == "main-with-focus" {
            busy_focus()?;
            println!("Focus main turn complete");
            eprintln!("+DWARFSTAR_WAITING");
            continue;
        }
        if line == "main-with-output" {
            println!("Main turn blocked with a live job");
            io::stdout().flush()?;
            busy_job_output()?;
            println!("Main turn completed after explicit release");
            eprintln!("+DWARFSTAR_WAITING");
            continue;
        }
        if line == "main-with-aside" && busy_aside {
            print!("Main prefix ");
            io::stdout().flush()?;
            for index in 1..=2 {
                let frame = lines
                    .next()
                    .ok_or_else(|| io::Error::other("Missing side question"))??;
                let frame: serde_json::Value = serde_json::from_str(&frame)?;
                assert_eq!(frame["type"], "btw");
                assert_eq!(frame["text"], format!("Question {index}?"));
                eprintln!(
                    "+PLANK_TELEMETRY {}",
                    serde_json::json!({"type":"aside","role":"answer","text":format!("Side answer {index}")})
                );
            }
            println!("main continuation.");
            eprintln!("+DWARFSTAR_WAITING");
            continue;
        }
        if line == "crash" {
            std::process::exit(7);
        }
        if line == "/frame demo" {
            turns += 1;
            let mut frame = nplank::frame::Snapshot {
                id: format!("fixture-frame-{turns}"),
                component: "demo".into(),
                w: 4,
                h: 1,
                cells: vec![nplank::frame::Cell {
                    x: 0,
                    y: 0,
                    ch: 'X',
                    fg: [255, 255, 255],
                    bg: None,
                    bold: false,
                }],
            };
            eprintln!(
                "+PLANK_TELEMETRY {}",
                serde_json::json!({"type":"frame", "snapshot":frame})
            );
            for line in lines.by_ref() {
                let value: serde_json::Value = serde_json::from_str(&line?)?;
                assert_eq!(value["type"], "frame_input");
                let request: nplank::frame::Request = serde_json::from_value(value)?;
                assert_eq!(request.id, frame.id);
                assert!(request.is_valid());
                match request.input {
                    nplank::frame::Input::Close => break,
                    nplank::frame::Input::Key { code, text } => {
                        if code == "crash" {
                            std::process::exit(7);
                        }
                        frame.cells[0].ch = text.unwrap_or('K');
                    }
                    nplank::frame::Input::Mouse { .. } => frame.cells[0].ch = 'M',
                    nplank::frame::Input::Text { text } => {
                        frame.cells[0].ch = text.chars().last().unwrap()
                    }
                    nplank::frame::Input::Resize { w, h } => {
                        frame.w = w;
                        frame.h = h;
                    }
                }
                eprintln!(
                    "+PLANK_TELEMETRY {}",
                    serde_json::json!({"type":"frame", "snapshot":frame})
                );
            }
            eprintln!(
                "+PLANK_TELEMETRY {}",
                serde_json::json!({"type":"frame", "id":frame.id, "snapshot":null})
            );
            eprintln!("+DWARFSTAR_WAITING");
            continue;
        }
        if line == "/export md" || line == "/open report.html" {
            let preview = line.starts_with("/open ");
            assert_eq!(std::env::var("PLANK_GUI_EXPORT_STREAM").as_deref(), Ok("1"));
            for event in [
                serde_json::json!({"phase":"begin", "id":"export-fixture",
                    "purpose":if preview {"preview"} else {"conversation"},
                    "name":if preview {"report.html"} else {"transcript.md"},
                    "format":if preview {"html"} else {"md"}, "bytes":5,
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
                &serde_json::json!({"version":1,"features":controls["features"],"commands":[{"name":"/commands","args":"","description":"List controls","source":"builtin"}]}),
            )?;
            println!("Switched engine; saved conversation restored");
            io::stdout().flush()?;
            eprintln!("+DWARFSTAR_WAITING");
            continue;
        }
        if line == "/open editor.txt" {
            if std::env::args().any(|arg| arg == "--editor-open-stream") {
                assert_eq!(std::env::var("PLANK_GUI_EDITOR_STREAM").as_deref(), Ok("1"));
                let initial = "\u{0}🦀\\\r\n".repeat(20_000);
                for frame in nplank::documenttransfer::Sender::new("opening-1", &initial)
                    .map_err(io::Error::other)?
                {
                    if matches!(frame, nplank::documenttransfer::Frame::End { .. }) {
                        std::fs::write("opening-started", "")?;
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(10);
                        while !std::path::Path::new("opening-finish").exists() {
                            if std::time::Instant::now() >= deadline {
                                return Err(io::Error::other("Editor opening fixture timed out"));
                            }
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        if std::env::args().any(|arg| arg == "--editor-open-cancel") {
                            eprintln!(
                                "+PLANK_EDITOR_DONE {}",
                                serde_json::json!({"id":"editor-1"})
                            );
                        }
                    }
                    eprintln!(
                        "+PLANK_EDITOR_STREAM {}",
                        serde_json::json!({"id":"editor-1","title":"editor.txt","frame":frame})
                    );
                }
                if std::env::args().any(|arg| arg == "--editor-open-cancel") {
                    eprintln!("+DWARFSTAR_WAITING");
                    continue;
                }
            } else {
                eprintln!(
                    "+PLANK_EDITOR {}",
                    serde_json::json!({"id": "editor-1", "title": "editor.txt", "text": "initial\n"})
                );
            }
            let mut receiver = nplank::documenttransfer::Receiver::default();
            let mut streamed = false;
            let edited = loop {
                let answer = lines
                    .next()
                    .ok_or_else(|| io::Error::other("Missing document decision"))??;
                let answer: serde_json::Value = serde_json::from_str(&answer)?;
                assert_eq!(answer["id"], "editor-1");
                assert_eq!(answer["type"], "dialog_reply");
                if answer["text"].is_object() {
                    assert!(editor_stream);
                    streamed = true;
                    let frame: nplank::documenttransfer::Frame =
                        serde_json::from_value(answer["text"].clone())?;
                    if std::env::args().any(|arg| arg == "--pause-editor-stream") {
                        std::fs::write("stream-started", "")?;
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(10);
                        while !std::path::Path::new("close-editor").exists() {
                            if std::time::Instant::now() >= deadline {
                                return Err(io::Error::other("Editor close fixture timed out"));
                            }
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        // Another controller closes the form while this pipe
                        // remains unread. The daemon must still consume stderr.
                        eprintln!(
                            "+PLANK_EDITOR_DONE {}",
                            serde_json::json!({"id":"editor-1"})
                        );
                        std::thread::sleep(std::time::Duration::from_secs(15));
                    }
                    if let Some(text) = receiver.accept(&frame).map_err(io::Error::other)? {
                        break Some(text);
                    }
                } else if let Some(text) = answer["text"].as_str() {
                    break Some(text.to_owned());
                } else {
                    assert!(answer["text"].is_null());
                    break None;
                }
            };
            if streamed {
                std::fs::write("stream-received", "")?;
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                while !std::path::Path::new("stream-ack").exists() {
                    if std::time::Instant::now() >= deadline {
                        return Err(io::Error::other("Editor ack fixture timed out"));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
            if let Some(text) = edited {
                std::fs::write("editor.txt", text)?;
                println!("Saved editor.txt");
            } else {
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
