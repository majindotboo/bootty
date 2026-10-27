use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
};

use base64::Engine as _;
use bootty_agents::NativeSessionConfig;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
static NEXT_THREAD: AtomicU64 = AtomicU64::new(1);

struct Peer {
    input: UnixStream,
    output: UnixStream,
    worker: Option<std::thread::JoinHandle<std::result::Result<(), String>>>,
}

impl bootty_agents::NativeChildControl for Peer {
    fn terminate(&mut self) -> std::result::Result<(), String> {
        let _ = self.input.shutdown(std::net::Shutdown::Both);
        let _ = self.output.shutdown(std::net::Shutdown::Both);
        Ok(())
    }

    fn wait(&mut self) -> std::result::Result<String, String> {
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| "provider panicked")??;
        }
        Ok("provider closed".into())
    }
}

#[derive(Clone, Default)]
pub struct Fixture {
    pub initialize_error: Arc<AtomicBool>,
    pub start_barrier: Arc<Mutex<Option<mpsc::Sender<()>>>>,
    pub first_turn_status: Option<String>,
}

impl Fixture {
    pub fn launcher(&self) -> bootty_agents::NativeLauncher {
        let fixture = self.clone();
        Arc::new(move |launch| launch_peer(launch, fixture.clone()))
    }
}

fn launch_peer(
    launch: &bootty_agents::NativeLaunch<'_>,
    fixture: Fixture,
) -> std::result::Result<bootty_agents::NativeChild, String> {
    let command = launch.command()?;
    let argv: Vec<_> = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let (stdin, input) = UnixStream::pair().map_err(|error| error.to_string())?;
    let (stdout, output) = UnixStream::pair().map_err(|error| error.to_string())?;
    let mut config = launch.config.clone();
    config.account_directory = command
        .get_envs()
        .find_map(|(key, value)| {
            (key == config.provider.account_directory_variable())
                .then_some(value)
                .flatten()
        })
        .map(|value| value.to_string_lossy().into_owned());
    config.cwd = command
        .get_current_dir()
        .ok_or("missing provider working directory")?
        .to_owned();
    let tools = launch.tool_arguments.to_vec();
    let mut peer = Peer {
        input: input.try_clone().map_err(|error| error.to_string())?,
        output: output.try_clone().map_err(|error| error.to_string())?,
        worker: None,
    };
    peer.worker = Some(std::thread::spawn(move || {
        if config.provider == bootty_agents::AgentKind::Pi {
            serve_pi(&config, &argv, input, output).map_err(|error| error.to_string())
        } else {
            serve(config, tools, fixture, input, output).map_err(|error| error.to_string())
        }
    }));
    Ok(bootty_agents::NativeChild {
        stdin: Box::new(stdin),
        stdout: Box::new(stdout),
        stderr: Box::new(std::io::empty()),
        control: Box::new(peer),
    })
}

struct Provider<W> {
    output: W,
    config: NativeSessionConfig,
    tools: Vec<String>,
    fixture: Fixture,
    thread: String,
    turn: u64,
    waiting: Option<Value>,
    held_start: Option<Value>,
    stuck: bool,
    approval: bool,
    methods: Vec<String>,
    turns: Vec<u64>,
    reverted: Vec<Value>,
}

/// Speaks the provider's stdio protocol, including real tool-bridge requests and reply barriers.
fn serve(
    config: NativeSessionConfig,
    tools: Vec<String>,
    fixture: Fixture,
    input: impl Read,
    output: impl Write,
) -> Result<()> {
    let mut provider = Provider {
        output,
        config,
        tools,
        fixture,
        thread: format!("thread-{}", NEXT_THREAD.fetch_add(1, Ordering::Relaxed)),
        turn: 0,
        waiting: None,
        held_start: None,
        stuck: false,
        approval: false,
        methods: Vec::new(),
        turns: Vec::new(),
        reverted: Vec::new(),
    };
    for line in BufReader::new(input).lines() {
        provider.request(&serde_json::from_str(&line?)?)?;
    }
    Ok(())
}

impl<W: Write> Provider<W> {
    fn emit(&mut self, value: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.output, value)?;
        self.output.write_all(b"\n")?;
        self.output.flush()?;
        Ok(())
    }

    fn reply(&mut self, id: &Value, result: &Value) -> Result<()> {
        self.emit(&json!({"id":id,"result":result}))
    }

    fn notice(&mut self, method: &str, mut params: Value) -> Result<()> {
        params["threadId"] = json!(self.thread);
        params["turnId"] = json!(format!("turn-{}", self.turn));
        self.emit(&json!({"method":method,"params":params}))
    }

    fn completed(&mut self, status: &str) -> Result<()> {
        self.notice(
            "turn/completed",
            json!({"turn":{"id":format!("turn-{}",self.turn),"status":status,"items":[]}}),
        )
    }

    fn request(&mut self, value: &Value) -> Result<()> {
        let Some(method) = value["method"].as_str() else {
            if self.approval && value.get("result").is_some() {
                self.completed("completed")?;
                self.approval = false;
            }
            return Ok(());
        };
        self.methods.push(method.to_owned());
        let id = &value["id"];
        let params = &value["params"];
        match method {
            "initialized" => Ok(()),
            "initialize" if self.fixture.initialize_error.load(Ordering::Acquire) => self.emit(&json!({"id":id,"error":{"code":-32603,"message":"provider initialization failed"}})),
            "thread/start" | "thread/resume" => {
                if let Some(thread) = params["threadId"].as_str() {
                    self.thread = thread.to_owned();
                }
                let barrier = (method == "thread/start").then(|| self.fixture.start_barrier.lock().expect("start barrier").take()).flatten();
                if let Some(barrier) = barrier {
                    self.held_start = Some(id.clone());
                    barrier.send(())?;
                    Ok(())
                } else {
                    self.reply(id, &json!({"thread":{"id":self.thread,"turns":[]}}))
                }
            }
            "__allow_start" => {
                let held = self.held_start.take().ok_or("no held start")?;
                self.reply(&held, &json!({"thread":{"id":self.thread,"turns":[]}}))?;
                self.reply(id, &json!({}))
            }
            "model/list" => self.reply(id, &json!({"data":[{"model":"qa-model","displayName":"QA Model","isDefault":true,"supportedReasoningEfforts":[{"reasoningEffort":"medium"}],"defaultReasoningEffort":"medium"}],"nextCursor":null})),
            "thread/fork" => self.reply(id, &json!({"thread":{"id":format!("fork-{}",NEXT_THREAD.fetch_add(1,Ordering::Relaxed))}})),
            "thread/items/list" => self.reply(id, &json!({"data":[],"nextCursor":null})),
            "thread/turns/list" => {
                let start = params["cursor"].as_str().unwrap_or("0").parse::<usize>()?;
                let count = usize::try_from(params["limit"].as_u64().ok_or("missing limit")?)?;
                let page: Vec<_> = self.turns.iter().rev().skip(start).take(count).map(|turn| json!({"id":format!("turn-{turn}"),"items":[{"id":format!("user-{turn}"),"type":"userMessage","content":[]}]})).collect();
                let end = start.checked_add(count).ok_or("history cursor overflow")?;
                self.reply(id, &json!({"data":page,"nextCursor":(end<self.turns.len()).then(||end.to_string())}))
            }
            "thread/revert" => {
                let before = params["beforeTurnId"].as_str().ok_or("missing turn")?.strip_prefix("turn-").ok_or("invalid turn")?.parse::<u64>()?;
                self.turns.retain(|turn| *turn < before);
                self.reverted.push(params["beforeTurnId"].clone());
                self.reply(id, &json!({"thread":{"id":self.thread}}))
            }
            "turn/start" => self.start_turn(id, params),
            "turn/interrupt" => {
                assert_target(params, &self.thread, Some(self.turn));
                if !self.stuck {
                    self.completed("interrupted")?;
                    if let Some(waiting) = self.waiting.take() {
                        self.reply(&waiting,&json!({"turn":{"id":format!("turn-{}",self.turn)}}))?;
                    }
                }
                self.reply(id, &json!({}))
            }
            "__spawn" => {
                let result = self.tool_call(params)?;
                self.reply(id, &result)
            }
            "__calls" => self.reply(id, &json!({"transport":"in-process","methods":self.methods,"reverted":self.reverted,"account_directory":self.config.account_directory,"bootty_tools":self.tools.iter().any(|arg|arg.starts_with("mcp_servers."))})),
            _ => self.reply(id, &json!({})),
        }
    }

    fn start_turn(&mut self, id: &Value, params: &Value) -> Result<()> {
        assert_target(params, &self.thread, None);
        self.turn = self.turn.checked_add(1).ok_or("turn overflow")?;
        self.turns.push(self.turn);
        let text = params["input"][0]["text"].as_str().ok_or("missing input")?;
        if let Some(first_status) = self.fixture.first_turn_status.clone() {
            let account = std::path::Path::new(
                self.config
                    .account_directory
                    .as_deref()
                    .ok_or("missing account")?,
            );
            let mut order = std::fs::OpenOptions::new().create(true).append(true).open(
                account
                    .parent()
                    .ok_or("missing fixture directory")?
                    .join("order"),
            )?;
            writeln!(order, "{text}")?;
            self.notice("turn/started", json!({"turn":{"id":format!("turn-{}",self.turn),"status":"inProgress","items":[]}}))?;
            let status = if text == "first" {
                &first_status
            } else {
                "completed"
            };
            self.completed(if status == "wrong_ack" {
                "completed"
            } else {
                status
            })?;
            return self.reply(id,&json!({"turn":{"id":if status == "wrong_ack" { "wrong-turn".into() } else { format!("turn-{}",self.turn) }}}));
        }
        if text == "visual-image" {
            assert_image(params);
        }
        if text == "reject-first" {
            return self
                .emit(&json!({"id":id,"error":{"code":-32600,"message":"first prompt rejected"}}));
        }
        if let Some(contents) = text.strip_prefix("write ") {
            std::fs::write(self.config.cwd.join("turn.txt"), format!("{contents}\n"))?;
        }
        let turn = format!("turn-{}", self.turn);
        self.notice(
            "turn/started",
            json!({"turn":{"id":turn,"status":"inProgress","items":[]}}),
        )?;
        self.notice("item/completed", json!({"item":{"id":format!("user-{}",self.turn),"type":"userMessage","content":params["input"]}}))?;
        match text {
            "wait" => { self.waiting = Some(id.clone()); return Ok(()); }
            "stuck" => self.stuck = true,
            "async-question" => self.notice("item/completed",json!({"item":{"id":format!("question-{}",self.turn),"type":"agentMessage","text":"","delivery":"async","questions":[{"title":"Which option?","options":["A","B"]}]}}))?,
            "approval" => {
                self.approval = true;
                self.emit(&json!({"id":900,"method":"item/commandExecution/requestApproval","params":{"threadId":self.thread,"turnId":turn,"itemId":"command","command":"echo approved"}}))?;
            }
            _ => {
                let command = format!("command-{}",self.turn);
                let answer = format!("answer-{}",self.turn);
                self.notice("item/started",json!({"item":{"id":command,"type":"commandExecution","command":"echo probe","aggregatedOutput":""}}))?;
                self.notice("item/commandExecution/outputDelta",json!({"itemId":command,"delta":"tool output"}))?;
                self.notice("item/agentMessage/delta",json!({"itemId":answer,"delta":format!("reply: {text}")}))?;
                self.notice("item/completed",json!({"item":{"id":answer,"type":"agentMessage","text":format!("reply: {text}")}}))?;
                self.completed("completed")?;
            }
        }
        self.reply(id, &json!({"turn":{"id":turn}}))
    }

    fn tool_call(&self, params: &Value) -> Result<Value> {
        let args = self
            .tools
            .windows(2)
            .find_map(|pair| {
                let (key, value) = pair[1].split_once('=')?;
                (pair[0] == "--config"
                    && key.starts_with("mcp_servers.")
                    && key.rsplit('.').next() == Some("args"))
                .then_some(value)
            })
            .ok_or("missing tool connection arguments")?;
        let args: Vec<String> = serde_json::from_str(args)?;
        let connection: Value = serde_json::from_slice(&std::fs::read(&args[1])?)?;
        let mut stream =
            UnixStream::connect(connection["socket"].as_str().ok_or("missing tool socket")?)?;
        serde_json::to_writer(
            &mut stream,
            &json!({"token":connection["token"],"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":params}}),
        )?;
        stream.write_all(b"\n")?;
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply)?;
        Ok(serde_json::from_str::<Value>(&reply)?["result"].clone())
    }
}

fn serve_pi(
    config: &NativeSessionConfig,
    argv: &[String],
    input: impl Read,
    mut output: impl Write,
) -> Result<()> {
    let account = std::path::Path::new(
        config
            .account_directory
            .as_deref()
            .ok_or("missing Pi account")?,
    );
    std::fs::create_dir_all(account)?;
    let session = format!("pi-child-{}", NEXT_THREAD.fetch_add(1, Ordering::Relaxed));
    let file = account.join(format!("{session}.jsonl"));
    std::fs::write(
        &file,
        format!("{}\n", json!({"type":"session","id":session,"version":3})),
    )?;
    std::fs::write(
        account.join("launch.json"),
        serde_json::to_vec(&json!({"argv":argv,"account":account}))?,
    )?;
    let mut messages = Vec::<Value>::new();
    for line in BufReader::new(input).lines() {
        let value: Value = serde_json::from_str(&line?)?;
        let kind = value["type"].as_str().ok_or("missing Pi request type")?;
        let data = match kind {
            "get_state" => {
                json!({"sessionId":session,"sessionFile":file,"isStreaming":false,"isCompacting":false})
            }
            "get_commands" => json!({"commands":[{"name":"__bootty_checkpoint"}]}),
            "switch_session" => json!({"cancelled":false}),
            "get_messages" => json!({"messages":messages}),
            "get_available_models" => json!({"models":[]}),
            "prompt" if value["message"] == "/__bootty_checkpoint" => {
                json!({"disposition":"handled"})
            }
            "prompt" => {
                let user = json!({"role":"user","content":value["message"],"timestamp":1});
                let answer = json!({"role":"assistant","content":[{"type":"text","text":"Pi child completed"}],"timestamp":2,"stopReason":"stop"});
                emit(&mut output, &json!({"type":"agent_start"}))?;
                for message in [&user, &answer] {
                    emit(
                        &mut output,
                        &json!({"type":"message_end","message":message}),
                    )?;
                }
                messages.extend([user, answer.clone()]);
                emit(
                    &mut output,
                    &json!({"type":"agent_end","messages":[answer],"willRetry":false}),
                )?;
                emit(&mut output, &json!({"type":"agent_settled"}))?;
                json!({"disposition":"started"})
            }
            _ => json!({}),
        };
        emit(
            &mut output,
            &json!({"type":"response","id":value["id"],"command":kind,"success":true,"data":data}),
        )?;
    }
    Ok(())
}

fn emit(output: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn assert_target(params: &Value, thread: &str, turn: Option<u64>) {
    assert_eq!(params["threadId"], thread);
    if let Some(turn) = turn {
        assert_eq!(params["turnId"], format!("turn-{turn}"));
    }
}

fn assert_image(params: &Value) {
    let input = params["input"].as_array().expect("image input");
    assert_eq!(input.len(), 2);
    assert_eq!(input[1]["type"], "image");
    let png = base64::engine::general_purpose::STANDARD
        .decode(
            input[1]["url"]
                .as_str()
                .expect("image URL")
                .strip_prefix("data:image/png;base64,")
                .expect("PNG data URL"),
        )
        .expect("base64 PNG");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(&png[16..24], &[0, 0, 0, 2, 0, 0, 0, 2]);
}
