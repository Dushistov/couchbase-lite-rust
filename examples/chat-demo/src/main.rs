use clap::{Parser, ValueEnum};
use couchbase_lite::{
    fallible_streaming_iterator::FallibleStreamingIterator,
    ffi::{C4ErrorCode, C4ErrorDomain, C4RevisionFlags, FLDict},
    resolve_conflict,
    serde_fleece::{from_fl_dict, Dict},
    C4CollectionSpec, C4DocumentEnded, C4String, Database, DatabaseFlags, DocEnumeratorFlags,
    Document, DocumentFlags, Error as DbError, Replicator, ReplicatorAuthentication,
    ReplicatorParameters, ReplicatorState,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    io::{self, Write},
    path::PathBuf,
    str,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Condvar, Mutex,
    },
    thread,
    time::Duration,
};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

type AppResult<T> = Result<T, String>;

const MAX_BATCH_MESSAGES: usize = 128;
const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;
const JOB_TIMEOUT: Duration = Duration::from_secs(60);
const STOP_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Serialize, Deserialize, Debug, Default)]
enum MessageType {
    #[default]
    Message,
}

#[derive(Serialize, Deserialize, Debug)]
struct Message {
    #[serde(rename = "type", default)]
    message_type: MessageType,
    msg: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    channels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
}

impl Message {
    fn new(msg: String, id: Option<String>) -> Self {
        Self {
            message_type: MessageType::Message,
            msg,
            channels: Vec::new(),
            id,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OutputMode {
    Human,
    Jsonl,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ConflictPolicy {
    Resolve,
    Report,
}

#[derive(Clone)]
struct Config {
    db_path: PathBuf,
    url: String,
    auth: ReplicatorAuthentication,
    channel: Option<String>,
    initial_messages: Vec<String>,
    output_mode: OutputMode,
    offline: bool,
    conflicts: ConflictPolicy,
}

/// Chat with a local Couchbase Lite database and optionally sync with a server.
#[derive(Parser)]
#[command(
    after_help = "Commands on stdin: text, !edit ID, !send JSON_ARRAY, !list, !offline, !online, !retry, !quit"
)]
struct Cli {
    /// Path to the local database
    #[arg(value_name = "DB_PATH")]
    db_path: PathBuf,
    /// Sync Gateway WebSocket URL
    #[arg(value_name = "URL")]
    url: Option<String>,
    /// Session token, or username when PASSWORD is given
    #[arg(value_name = "SESSION_TOKEN_OR_USER")]
    token_or_user: Option<String>,
    /// Password for basic authentication
    #[arg(value_name = "PASSWORD", requires = "token_or_user")]
    password: Option<String>,
    /// Send new messages to this chat channel
    #[arg(long, value_name = "NAME", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    channel: Option<String>,
    /// Add a message to one startup batch (repeatable)
    #[arg(long = "message", value_name = "TEXT")]
    initial_messages: Vec<String>,
    /// Select interactive or JSON Lines output
    #[arg(long, value_enum, default_value = "human")]
    output: OutputMode,
    /// Open the local chat without replication
    #[arg(long)]
    offline: bool,
    /// Resolve conflicts automatically or only report them
    #[arg(long, value_enum, default_value = "resolve")]
    conflicts: ConflictPolicy,
}

impl TryFrom<Cli> for Config {
    type Error = String;

    fn try_from(cli: Cli) -> AppResult<Self> {
        if !cli.initial_messages.is_empty() {
            validate_batch_size(
                cli.initial_messages.len(),
                cli.initial_messages.iter().map(String::len).sum(),
            )?;
        }
        let auth = match (cli.token_or_user, cli.password) {
            (None, None) => ReplicatorAuthentication::None,
            (Some(token), None) => ReplicatorAuthentication::SessionToken(token.into()),
            (Some(username), Some(password)) => ReplicatorAuthentication::Basic {
                username: username.into(),
                password: password.into(),
            },
            (None, Some(_)) => unreachable!("clap requires a username for PASSWORD"),
        };
        Ok(Self {
            db_path: cli.db_path,
            url: cli
                .url
                .unwrap_or_else(|| "ws://192.168.1.32:4984/demo/".into()),
            auth,
            channel: cli.channel,
            initial_messages: cli.initial_messages,
            output_mode: cli.output,
            offline: cli.offline,
            conflicts: cli.conflicts,
        })
    }
}

fn validate_batch_size(count: usize, bytes: usize) -> AppResult<()> {
    if count == 0 {
        return Err("message batch is empty".into());
    }
    if count > MAX_BATCH_MESSAGES || bytes > MAX_INPUT_BYTES {
        return Err("message batch exceeds 128 messages or 4 MiB".into());
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OutputEvent {
    State {
        generation: u64,
        state: &'static str,
        domain: Option<u8>,
        code: Option<i32>,
    },
    #[serde(rename = "document_error")]
    ObserverError {
        message: String,
    },
    #[serde(rename = "document_error")]
    ReplicationDocumentError {
        generation: u64,
        id: String,
        pushing: bool,
        domain: u8,
        code: i32,
        transient: bool,
    },
    #[serde(rename = "document_error")]
    DocumentError {
        id: String,
        message: String,
    },
    #[serde(rename = "conflict")]
    Conflict {
        id: String,
    },
    #[serde(rename = "conflict")]
    ReplicationConflict {
        generation: u64,
        id: String,
    },
    ListBegin {
        command: u64,
    },
    Document {
        command: u64,
        id: String,
        revision: Option<String>,
        sequence: Option<u64>,
        flags: u32,
        deleted: bool,
        conflicted: bool,
        msg: Option<String>,
        channels: Vec<String>,
    },
    ListEnd {
        command: u64,
    },
    ReceivedDeleted {
        id: String,
        flags: u8,
    },
    Received {
        id: String,
        msg: String,
        channels: Vec<String>,
        flags: u8,
    },
    ValidationRejected {
        generation: u64,
        id: String,
    },
    Saved {
        command: u64,
        ids: Vec<String>,
    },
    EditTarget {
        command: u64,
        id: String,
    },
    Offline {
        command: u64,
    },
    Online {
        command: u64,
        generation: u64,
    },
    Retry {
        command: u64,
        reconnecting: bool,
    },
    Error {
        command: u64,
        message: String,
    },
    Quit {
        command: u64,
        result: &'static str,
    },
}

fn write_human(event: &OutputEvent, stdout: &mut impl Write) -> io::Result<()> {
    let value = serde_json::to_value(event).map_err(io::Error::other)?;
    let fields = value
        .as_object()
        .expect("an event serializes as a JSON object");
    let event_type = fields["type"].as_str().expect("an event has a type");
    write!(stdout, "{event_type}")?;
    for (name, value) in fields {
        if name != "type" && !value.is_null() {
            write!(stdout, " {name}={value}")?;
        }
    }
    writeln!(stdout)
}

struct Output {
    tx: mpsc::SyncSender<OutputEvent>,
    overflow: AtomicBool,
    failed: Arc<AtomicBool>,
}

impl Output {
    fn start(mode: OutputMode) -> (Arc<Self>, thread::JoinHandle<()>) {
        let (tx, rx) = mpsc::sync_channel::<OutputEvent>(256);
        let failed = Arc::new(AtomicBool::new(false));
        let output = Arc::new(Self {
            tx,
            overflow: AtomicBool::new(false),
            failed: failed.clone(),
        });
        let worker = thread::spawn(move || {
            let stdout = io::stdout();
            let mut stdout = stdout.lock();
            while let Ok(event) = rx.recv() {
                let result = match mode {
                    OutputMode::Human => write_human(&event, &mut stdout),
                    OutputMode::Jsonl => serde_json::to_writer(&mut stdout, &event)
                        .map_err(io::Error::other)
                        .and_then(|()| writeln!(stdout)),
                }
                .and_then(|()| stdout.flush());
                if result.is_err() {
                    failed.store(true, Ordering::Relaxed);
                }
            }
        });
        (output, worker)
    }

    fn emit(&self, event: OutputEvent) {
        if self.tx.send(event).is_err() {
            self.failed.store(true, Ordering::Relaxed);
        }
    }

    fn callback(&self, event: OutputEvent) {
        if self.tx.try_send(event).is_err() {
            self.overflow.store(true, Ordering::Relaxed);
        }
    }
}

#[derive(Clone)]
struct DbExecutor {
    tx: mpsc::Sender<DbJob>,
}

type Job = Box<dyn FnOnce(&mut DbState) + Send>;

enum DbJob {
    Run(Job),
    Shutdown(mpsc::SyncSender<()>),
}

impl DbExecutor {
    fn schedule(&self, job: impl FnOnce(&mut DbState) + Send + 'static) {
        let _ = self.tx.send(DbJob::Run(Box::new(job)));
    }

    fn call<R: Send + 'static>(
        &self,
        job: impl FnOnce(&mut DbState) -> AppResult<R> + Send + 'static,
    ) -> AppResult<R> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(DbJob::Run(Box::new(move |state| {
                let _ = reply_tx.send(job(state));
            })))
            .map_err(|_| "database thread has stopped".to_string())?;
        reply_rx
            .recv_timeout(JOB_TIMEOUT)
            .map_err(|err| format!("database job did not complete: {err}"))?
    }

    fn shutdown(&self) -> AppResult<()> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(DbJob::Shutdown(reply_tx))
            .map_err(|_| "database thread has stopped".to_string())?;
        reply_rx
            .recv_timeout(JOB_TIMEOUT)
            .map_err(|err| format!("database shutdown did not complete: {err}"))
    }
}

struct StateMonitor {
    stopped: Mutex<bool>,
    changed: Condvar,
}

impl StateMonitor {
    fn new() -> Self {
        Self {
            stopped: Mutex::new(false),
            changed: Condvar::new(),
        }
    }

    fn record(&self, state: &ReplicatorState) {
        if matches!(state, ReplicatorState::Stopped(_)) {
            if let Ok(mut stopped) = self.stopped.lock() {
                *stopped = true;
                self.changed.notify_all();
            }
        }
    }

    fn is_stopped(&self) -> AppResult<bool> {
        self.stopped
            .lock()
            .map(|stopped| *stopped)
            .map_err(|_| "replicator state lock poisoned".to_string())
    }

    fn reset(&self) -> AppResult<()> {
        *self
            .stopped
            .lock()
            .map_err(|_| "replicator state lock poisoned".to_string())? = false;
        Ok(())
    }

    fn wait_stopped(&self) -> AppResult<()> {
        let stopped = self
            .stopped
            .lock()
            .map_err(|_| "replicator state lock poisoned".to_string())?;
        let (stopped, timeout) = self
            .changed
            .wait_timeout_while(stopped, STOP_TIMEOUT, |stopped| !*stopped)
            .map_err(|_| "replicator state lock poisoned".to_string())?;
        if timeout.timed_out() && !*stopped {
            return Err("replicator did not stop within 20 seconds".into());
        }
        Ok(())
    }
}

struct Replication {
    repl: Replicator,
    monitor: Arc<StateMonitor>,
    generation: u64,
    stop_requested: bool,
}

struct DbState {
    db: Database,
    replication: Option<Replication>,
    next_generation: u64,
    config: Config,
    output: Arc<Output>,
    executor: DbExecutor,
}

fn state_event(generation: u64, state: &ReplicatorState) -> OutputEvent {
    let (name, domain, code) = match state {
        ReplicatorState::Stopped(err) => {
            let (domain, code) = error_domain_code(err);
            ("stopped", domain, code)
        }
        ReplicatorState::Offline => ("offline", None, None),
        ReplicatorState::Connecting => ("connecting", None, None),
        ReplicatorState::Idle => ("idle", None, None),
        ReplicatorState::Busy(_) => ("busy", None, None),
    };
    OutputEvent::State {
        generation,
        state: name,
        domain,
        code,
    }
}

fn error_domain_code(err: &DbError) -> (Option<u8>, Option<i32>) {
    match err {
        DbError::C4Error(c4) => (Some(c4.domain.0), Some(c4.code)),
        _ => (None, None),
    }
}

impl DbState {
    fn open(config: Config, output: Arc<Output>, executor: DbExecutor) -> AppResult<Self> {
        let db = Database::open_with_flags(&config.db_path, DatabaseFlags::CREATE)
            .map_err(|err| err.to_string())?;
        let mut state = Self {
            db,
            replication: None,
            next_generation: 1,
            config,
            output,
            executor,
        };
        state.check_conflicts()?;
        if state.config.output_mode == OutputMode::Human {
            state.list(0)?;
        }
        let observer_exec = state.executor.clone();
        state
            .db
            .register_observer(move || {
                observer_exec.schedule(|state| {
                    if let Err(err) = state.print_external_changes() {
                        state
                            .output
                            .callback(OutputEvent::ObserverError { message: err });
                    }
                });
            })
            .map_err(|err| err.to_string())?;
        if !state.config.offline {
            state.start_replication()?;
        }
        Ok(state)
    }

    fn check_conflicts(&mut self) -> AppResult<()> {
        let mut ids = Vec::new();
        let mut it = self
            .db
            .enumerate_all_docs(DocEnumeratorFlags::empty())
            .map_err(|err| err.to_string())?;
        while let Some(item) = it.next().map_err(|err| err.to_string())? {
            ids.push(
                item.get_doc()
                    .map_err(|err| err.to_string())?
                    .id()
                    .to_string(),
            );
        }
        drop(it);
        for id in ids {
            self.output.emit(OutputEvent::Conflict { id: id.clone() });
            if self.config.conflicts == ConflictPolicy::Resolve {
                resolve_conflict(&mut self.db, &id).map_err(|err| err.to_string())?;
            }
        }
        Ok(())
    }

    fn start_replication(&mut self) -> AppResult<u64> {
        if let Some(replication) = &mut self.replication {
            if replication.monitor.is_stopped()? {
                replication.monitor.reset()?;
                replication
                    .repl
                    .start(false)
                    .map_err(|err| err.to_string())?;
                replication.stop_requested = false;
            } else if replication.stop_requested {
                return Err("replication is still stopping".into());
            }
            return Ok(replication.generation);
        }
        let generation = self.next_generation;
        self.next_generation += 1;
        let monitor = Arc::new(StateMonitor::new());
        let state_monitor = monitor.clone();
        let state_output = self.output.clone();
        let output_busy = AtomicBool::new(false);
        let docs_output = self.output.clone();
        let validation_output = self.output.clone();
        let conflict_exec = self.executor.clone();
        let conflicts = self.config.conflicts;
        let params = ReplicatorParameters::default()
            .with_auth(self.config.auth.clone())
            .with_state_changed_callback(move |state| {
                // Progress updates can report Busy repeatedly without changing
                // the event we show to users. Keep the output queue available
                // for state transitions and errors while a large pull is active.
                let busy = matches!(&state, ReplicatorState::Busy(_));
                let was_busy = output_busy.swap(busy, Ordering::Relaxed);
                if !busy || !was_busy {
                    state_output.callback(state_event(generation, &state));
                }
                state_monitor.record(&state);
            })
            .with_documents_ended_callback(
                move |pushing: bool, docs: &mut dyn Iterator<Item = &C4DocumentEnded>| {
                    for doc in docs {
                        let id =
                            String::from_utf8_lossy(doc.docID.as_fl_slice().into()).into_owned();
                        if doc.error.code != 0 {
                            docs_output.callback(OutputEvent::ReplicationDocumentError {
                                generation,
                                id: id.clone(),
                                pushing,
                                domain: doc.error.domain.0,
                                code: doc.error.code,
                                transient: doc.errorIsTransient,
                            });
                        }
                        if !pushing
                            && (doc.flags & C4RevisionFlags::kRevIsConflict) != C4RevisionFlags(0)
                        {
                            docs_output.callback(OutputEvent::ReplicationConflict {
                                generation,
                                id: id.clone(),
                            });
                            if conflicts == ConflictPolicy::Resolve {
                                conflict_exec.schedule(move |state| {
                                    if let Err(err) = resolve_conflict(&mut state.db, &id) {
                                        state.output.emit(OutputEvent::DocumentError {
                                            id,
                                            message: err.to_string(),
                                        });
                                    }
                                });
                            }
                        }
                    }
                },
            )
            .with_validation_func(move |coll_spec, doc_id, rev_id, flags, body| {
                input_doc_filter(
                    coll_spec,
                    doc_id,
                    rev_id,
                    flags,
                    body,
                    &validation_output,
                    generation,
                )
            });
        let mut repl =
            Replicator::new(&self.db, &self.config.url, params).map_err(|err| err.to_string())?;
        repl.start(false).map_err(|err| err.to_string())?;
        self.replication = Some(Replication {
            repl,
            monitor,
            generation,
            stop_requested: false,
        });
        Ok(generation)
    }

    fn request_stop(&mut self) -> Option<Arc<StateMonitor>> {
        self.replication.as_mut().map(|replication| {
            replication.stop_requested = true;
            replication.repl.stop();
            replication.monitor.clone()
        })
    }

    fn finish_stop(&mut self) {
        self.replication.take();
    }

    fn retry(&mut self) -> AppResult<bool> {
        let replication = self
            .replication
            .as_mut()
            .ok_or_else(|| "replication is offline; use !online first".to_string())?;
        replication.repl.retry().map_err(|err| err.to_string())
    }

    fn save_batch(&mut self, messages: Vec<Message>, edit: bool) -> AppResult<Vec<String>> {
        validate_batch_size(
            messages.len(),
            messages.iter().map(|message| message.msg.len()).sum(),
        )?;
        let mut seen = HashSet::new();
        for message in &messages {
            if let Some(id) = &message.id {
                if id.is_empty() || !seen.insert(id.as_str()) {
                    return Err("message IDs must be nonempty and unique in a batch".into());
                }
                match self.db.get_existing(id) {
                    Ok(_) if !edit => return Err(format!("message ID already exists: {id}")),
                    Ok(_) => {}
                    Err(DbError::C4Error(err))
                        if err.domain == C4ErrorDomain::LiteCoreDomain
                            && err.code == C4ErrorCode::kC4ErrorNotFound.0 =>
                    {
                        if edit {
                            return Err(format!("message ID does not exist: {id}"));
                        }
                    }
                    Err(err) => return Err(err.to_string()),
                }
            } else if edit {
                return Err("edit requires an ID".into());
            }
        }
        let mut transaction = self.db.transaction().map_err(|err| err.to_string())?;
        let mut ids = Vec::with_capacity(messages.len());
        for mut message in messages {
            let id = message.id.take();
            message.channels = self.config.channel.iter().cloned().collect();
            let encoder = transaction
                .shared_encoder_session()
                .map_err(|err| err.to_string())?;
            let mut doc = match id {
                Some(id) => {
                    Document::new_with_id(id, &message, encoder).map_err(|err| err.to_string())?
                }
                None => Document::new(&message, encoder).map_err(|err| err.to_string())?,
            };
            transaction.save(&mut doc).map_err(|err| err.to_string())?;
            ids.push(doc.id().to_string());
        }
        transaction.commit().map_err(|err| err.to_string())?;
        Ok(ids)
    }

    fn list(&self, command: u64) -> AppResult<()> {
        self.output.emit(OutputEvent::ListBegin { command });
        let flags = DocEnumeratorFlags::INCLUDE_DELETED
            | DocEnumeratorFlags::INCLUDE_NON_CONFLICTED
            | DocEnumeratorFlags::INCLUDE_BODIES;
        let mut it = self
            .db
            .enumerate_all_docs(flags)
            .map_err(|err| err.to_string())?;
        while let Some(item) = it.next().map_err(|err| err.to_string())? {
            let doc = item.get_doc().map_err(|err| err.to_string())?;
            let id = doc.id().to_string();
            let flags = doc.flags().unwrap_or_else(DocumentFlags::empty);
            let revision = doc.revision_id().map(str::to_owned);
            let sequence = doc.sequence();
            let deleted = flags.contains(DocumentFlags::DELETED);
            let conflicted = flags.contains(DocumentFlags::CONFLICTED);
            let body = if deleted {
                None
            } else {
                Some(
                    doc.decode_body::<Message>()
                        .map_err(|err| err.to_string())?,
                )
            };
            let (msg, channels) = match body {
                Some(body) => (Some(body.msg), body.channels),
                None => (None, Vec::new()),
            };
            self.output.emit(OutputEvent::Document {
                command,
                id,
                revision,
                sequence,
                flags: flags.bits(),
                deleted,
                conflicted,
                msg,
                channels,
            });
        }
        self.output.emit(OutputEvent::ListEnd { command });
        Ok(())
    }

    fn print_external_changes(&mut self) -> AppResult<()> {
        let mut ids = HashMap::new();
        for change in self.db.observed_changes() {
            if change.external() {
                ids.insert(
                    change.doc_id().map_err(|err| err.to_string())?.to_owned(),
                    change.revision_flags().0,
                );
            }
        }
        for (id, flags) in ids {
            if (C4RevisionFlags(flags)
                & (C4RevisionFlags::kRevDeleted | C4RevisionFlags::kRevPurged))
                != C4RevisionFlags(0)
            {
                self.output.emit(OutputEvent::ReceivedDeleted { id, flags });
                continue;
            }
            match self.db.get_existing(&id) {
                Ok(doc) => match doc.decode_body::<Message>() {
                    Ok(body) => self.output.emit(OutputEvent::Received {
                        id,
                        msg: body.msg,
                        channels: body.channels,
                        flags,
                    }),
                    Err(err) => self.output.emit(OutputEvent::DocumentError {
                        id,
                        message: err.to_string(),
                    }),
                },
                Err(err) => self.output.emit(OutputEvent::DocumentError {
                    id,
                    message: err.to_string(),
                }),
            }
        }
        Ok(())
    }
}

fn input_doc_filter(
    _coll_spec: C4CollectionSpec,
    doc_id: C4String,
    _rev_id: C4String,
    flags: C4RevisionFlags,
    body: FLDict,
    output: &Output,
    generation: u64,
) -> bool {
    if (flags & (C4RevisionFlags::kRevDeleted | C4RevisionFlags::kRevPurged)) != C4RevisionFlags(0)
    {
        return true;
    }
    let valid = Dict::new(&body)
        .filter(|body| body.get_as_str("type") == Some("Message"))
        .is_some_and(|body| from_fl_dict::<Message, _>(body).is_ok());
    if !valid {
        let id = String::from_utf8_lossy(doc_id.into()).into_owned();
        output.callback(OutputEvent::ValidationRejected { generation, id });
    }
    valid
}

fn run_db_thread(
    config: Config,
    output: Arc<Output>,
) -> (
    DbExecutor,
    mpsc::Receiver<AppResult<()>>,
    thread::JoinHandle<()>,
) {
    let (tx, rx) = mpsc::channel::<DbJob>();
    let executor = DbExecutor { tx };
    let worker_exec = executor.clone();
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        let mut state = match DbState::open(config, output, worker_exec) {
            Ok(state) => {
                let _ = ready_tx.send(Ok(()));
                state
            }
            Err(err) => {
                let _ = ready_tx.send(Err(err));
                return;
            }
        };
        while let Ok(job) = rx.recv() {
            match job {
                DbJob::Run(job) => job(&mut state),
                DbJob::Shutdown(reply) => {
                    state.db.clear_observers();
                    state.replication.take();
                    let _ = reply.send(());
                    break;
                }
            }
        }
    });
    (executor, ready_rx, worker)
}

fn stop_replication(executor: &DbExecutor) -> AppResult<()> {
    let monitor = executor.call(|state| Ok(state.request_stop()))?;
    if let Some(monitor) = monitor {
        monitor.wait_stopped()?;
        executor.call(|state| {
            state.finish_stop();
            Ok(())
        })?;
    }
    Ok(())
}

async fn read_command_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> AppResult<Option<String>> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().await.map_err(|err| err.to_string())?;
        if available.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            break;
        }
        let size = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if bytes.len() + size > MAX_INPUT_BYTES {
            return Err("input line exceeds 4 MiB".into());
        }
        let ended = available[size - 1] == b'\n';
        bytes.extend_from_slice(&available[..size]);
        reader.consume(size);
        if ended {
            break;
        }
    }
    let line = String::from_utf8(bytes).map_err(|err| err.to_string())?;
    Ok(Some(line.trim_end_matches(['\r', '\n']).to_owned()))
}

async fn interact(executor: &DbExecutor, output: &Output, config: &Config) -> AppResult<u64> {
    let mut command = 0;
    if !config.initial_messages.is_empty() {
        command += 1;
        let messages: Vec<_> = config
            .initial_messages
            .iter()
            .map(|msg| Message::new(msg.clone(), None))
            .collect();
        let ids = executor.call(move |state| state.save_batch(messages, false))?;
        output.emit(OutputEvent::Saved { command, ids });
    }

    let mut stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut edit_id = None;
    loop {
        let Some(line) = read_command_line(&mut stdin).await? else {
            command += 1;
            return Ok(command);
        };
        if line.is_empty() {
            continue;
        }
        command += 1;
        if line == "!quit" {
            return Ok(command);
        }
        let result: AppResult<()> = if let Some(id) = line.strip_prefix("!edit ") {
            if id.is_empty() {
                Err("edit ID must not be empty".into())
            } else {
                edit_id = Some(id.to_owned());
                output.emit(OutputEvent::EditTarget {
                    command,
                    id: id.to_owned(),
                });
                Ok(())
            }
        } else if line == "!list" {
            executor.call(move |state| state.list(command))
        } else if line == "!offline" {
            let result = stop_replication(executor);
            if result.is_ok() {
                output.emit(OutputEvent::Offline { command });
            }
            result
        } else if line == "!online" {
            executor
                .call(|state| state.start_replication())
                .map(|generation| {
                    output.emit(OutputEvent::Online {
                        command,
                        generation,
                    });
                })
        } else if line == "!retry" {
            executor.call(|state| state.retry()).map(|reconnecting| {
                output.emit(OutputEvent::Retry {
                    command,
                    reconnecting,
                });
            })
        } else if let Some(json_batch) = line.strip_prefix("!send ") {
            let messages: AppResult<Vec<Message>> =
                serde_json::from_str(json_batch).map_err(|err| err.to_string());
            match messages {
                Ok(messages) => executor
                    .call(move |state| state.save_batch(messages, false))
                    .map(|ids| {
                        output.emit(OutputEvent::Saved { command, ids });
                    }),
                Err(err) => Err(err),
            }
        } else if line.starts_with('!') && edit_id.is_none() {
            Err(format!("unknown command: {line}"))
        } else {
            let message = Message::new(line, edit_id.take());
            let edit = message.id.is_some();
            executor
                .call(move |state| state.save_batch(vec![message], edit))
                .map(|ids| {
                    output.emit(OutputEvent::Saved { command, ids });
                })
        };
        if let Err(err) = result {
            output.emit(OutputEvent::Error {
                command,
                message: err,
            });
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::try_from(Cli::parse())?;
    env_logger::init();
    let runtime = tokio::runtime::Runtime::new().map_err(|err| err.to_string())?;
    Database::init_socket_impl(
        #[cfg(feature = "use-tokio-websocket")]
        runtime.handle().clone(),
    );

    let (output, output_worker) = Output::start(config.output_mode);
    let (executor, ready, db_worker) = run_db_thread(config.clone(), output.clone());
    let ready_result = ready
        .recv_timeout(JOB_TIMEOUT)
        .map_err(|err| format!("database initialization did not complete: {err}"))
        .and_then(|result| result);
    let mut interaction = None;
    if ready_result.is_ok() {
        interaction = Some(runtime.block_on(interact(&executor, &output, &config)));
    }
    let stop_result = if ready_result.is_ok() {
        stop_replication(&executor)
    } else {
        Ok(())
    };
    let shutdown_result = if ready_result.is_ok() {
        executor.shutdown()
    } else {
        Ok(())
    };
    drop(executor);
    let db_join = db_worker
        .join()
        .map_err(|_| "database thread panicked".to_string());
    let quit_command = interaction.as_ref().and_then(|result| result.as_ref().ok());
    if let Some(command) = quit_command {
        if let Some(err) = stop_result
            .as_ref()
            .err()
            .or(shutdown_result.as_ref().err())
        {
            output.emit(OutputEvent::Error {
                command: *command,
                message: err.clone(),
            });
        } else {
            output.emit(OutputEvent::Quit {
                command: *command,
                result: "stopped",
            });
        }
    }
    let output_failed = output.failed.clone();
    let overflow = output.overflow.load(Ordering::Relaxed);
    drop(output);
    let output_join = output_worker
        .join()
        .map_err(|_| "output thread panicked".to_string());

    ready_result?;
    if let Some(result) = interaction {
        result?;
    }
    stop_result?;
    shutdown_result?;
    db_join?;
    output_join?;
    if overflow || output_failed.load(Ordering::Relaxed) {
        return Err("output queue overflowed or stdout write failed".into());
    }
    Ok(())
}
