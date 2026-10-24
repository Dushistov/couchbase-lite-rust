use crate::{
    ffi::{
        c4Socket_getNativeHandle, c4Socket_setNativeHandle, c4error_make, c4socket_closeRequested,
        c4socket_closed, c4socket_completedWrite, c4socket_gotHTTPResponse, c4socket_opened,
        c4socket_received, c4socket_registerFactory, C4Address, C4Error, C4ErrorCode,
        C4ErrorDomain, C4NetworkErrorCode, C4Slice, C4SliceResult, C4Socket, C4SocketFactory,
        C4SocketFraming, C4String, C4WebSocketCloseCode, FLDict_Get, FLEncoder_BeginDict,
        FLEncoder_EndDict, FLEncoder_Finish, FLEncoder_Free, FLEncoder_New, FLEncoder_WriteKey,
        FLEncoder_WriteString, FLError, FLSliceResult, FLTrust, FLValue_AsDict, FLValue_FromData,
    },
    replicator::consts::*,
    value::ValueRef,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use futures_util::{
    sink::SinkExt,
    stream::{SplitSink, SplitStream, StreamExt},
};
use log::{error, info, trace, warn};
use serde_fleece::NonNullConst;
use std::{
    future::Future,
    io::Write,
    marker::PhantomData,
    os::raw::{c_int, c_void},
    ptr::NonNull,
    str::FromStr,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    runtime::Handle,
    sync::mpsc,
    time::Instant,
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        self,
        client::IntoClientRequest,
        handshake::client::{Request, Response},
        http::{self, header::HeaderName, HeaderValue, Uri},
        protocol::{frame::coding::CloseCode, CloseFrame},
        Bytes, Message,
    },
    WebSocketStream,
};

pub fn c4socket_init(handle: Handle) {
    let handle = Box::new(handle);
    let sock_factory = C4SocketFactory {
        framing: C4SocketFraming::kC4NoFraming,
        context: Box::into_raw(handle) as *mut c_void,
        open: Some(ws_open),
        write: Some(ws_write),
        completedReceive: Some(ws_completed_receive),
        close: None,
        requestClose: Some(ws_request_close),
        dispose: Some(ws_dispose),
    };
    unsafe { c4socket_registerFactory(sock_factory) };
}

struct SocketImpl {
    commands: mpsc::UnboundedSender<Command>,
}

enum Command {
    Write(Bytes),
    CompletedReceive(usize),
    RequestClose(c_int, String),
}

enum WriterCommand {
    Write(Bytes),
    Close(CloseFrame),
    FlushCloseReply,
}

enum WriterEvent {
    Written(usize),
    CloseSent,
    CloseReplyFlushed,
    Failed(Box<tungstenite::Error>),
}

#[derive(Clone, Copy)]
enum CloseState {
    Open,
    Local {
        deadline: Instant,
        error: C4Error,
    },
    Peer {
        deadline: Instant,
        error: C4Error,
        reply_requested: bool,
    },
}

struct Terminal<C: CloseSink> {
    core: Option<C>,
}

struct AbortWriter(tokio::task::AbortHandle);

impl Drop for AbortWriter {
    fn drop(&mut self) {
        self.0.abort();
    }
}

const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

type WsReader<S> = SplitStream<WebSocketStream<S>>;
type WsWriter<S> = SplitSink<WebSocketStream<S>, Message>;

#[repr(transparent)]
#[derive(Clone, Copy, Debug)]
struct C4SocketPtr(*mut C4Socket);
unsafe impl Send for C4SocketPtr {}

struct ConnectingPhase;
struct OpenPhase;

struct FfiCore<P> {
    socket: C4SocketPtr,
    phase: PhantomData<P>,
}

impl<P> From<&FfiCore<P>> for usize {
    fn from(core: &FfiCore<P>) -> Self {
        core.socket.0 as usize
    }
}

impl<P: Send + 'static> CloseSink for FfiCore<P> {
    fn closed(self, error: C4Error) {
        unsafe { c4socket_closed(self.socket.0, error) };
    }
}

trait CloseSink: Send + 'static {
    fn closed(self, error: C4Error);
}

trait ConnectingSink: CloseSink {
    type Open: OpenSink + CloseSink;

    fn got_http_response(&mut self, status: c_int, headers: FLSliceResult);
    fn opened(self) -> Self::Open;
}

trait OpenSink: Send + 'static {
    fn received(&mut self, data: &[u8]);
    fn completed_write(&mut self, size: usize);
    fn close_requested(&mut self, status: c_int, reason: &str);
}

impl FfiCore<ConnectingPhase> {
    // The caller must keep the LiteCore socket alive while this value is in use.
    unsafe fn new(socket: C4SocketPtr) -> Self {
        Self {
            socket,
            phase: PhantomData,
        }
    }
}

impl ConnectingSink for FfiCore<ConnectingPhase> {
    type Open = FfiCore<OpenPhase>;

    fn got_http_response(&mut self, status: c_int, headers: FLSliceResult) {
        unsafe { c4socket_gotHTTPResponse(self.socket.0, status, headers.as_fl_slice()) };
    }

    fn opened(self) -> Self::Open {
        unsafe { c4socket_opened(self.socket.0) };
        FfiCore {
            socket: self.socket,
            phase: PhantomData,
        }
    }
}

impl OpenSink for FfiCore<OpenPhase> {
    fn received(&mut self, data: &[u8]) {
        unsafe { c4socket_received(self.socket.0, data.into()) };
    }

    fn completed_write(&mut self, size: usize) {
        unsafe { c4socket_completedWrite(self.socket.0, size) };
    }

    fn close_requested(&mut self, status: c_int, reason: &str) {
        unsafe { c4socket_closeRequested(self.socket.0, status, reason.as_bytes().into()) };
    }
}

#[repr(transparent)]
#[derive(Debug)]
struct Error(C4Error);

fn network_error(code: C4NetworkErrorCode, message: &str) -> C4Error {
    unsafe {
        c4error_make(
            C4ErrorDomain::NetworkDomain,
            code.0,
            message.as_bytes().into(),
        )
    }
}

fn close_error(status: c_int, reason: &str) -> C4Error {
    unsafe {
        c4error_make(
            C4ErrorDomain::WebSocketDomain,
            status,
            reason.as_bytes().into(),
        )
    }
}

impl<C: CloseSink> Terminal<C> {
    fn new(core: C) -> Self {
        Self { core: Some(core) }
    }

    fn core(&mut self) -> &mut C {
        self.core.as_mut().expect("terminal already closed")
    }

    fn finish(&mut self, error: C4Error) {
        if let Some(core) = self.core.take() {
            core.closed(error);
        }
    }
}

impl<C: ConnectingSink> Terminal<C> {
    fn opened(mut self) -> Terminal<C::Open> {
        let core = self.core.take().expect("terminal already closed");
        Terminal::new(core.opened())
    }
}

impl<C: CloseSink> Drop for Terminal<C> {
    fn drop(&mut self) {
        if self.core.is_some() {
            self.finish(network_error(
                C4NetworkErrorCode::kC4NetErrConnectionAborted,
                "socket task was cancelled",
            ));
        }
    }
}

unsafe extern "C" fn ws_open(
    c4sock: *mut C4Socket,
    addr: *const C4Address,
    options: C4Slice,
    context: *mut c_void,
) {
    assert!(!c4sock.is_null(), "ws_open received a null socket");
    assert!(!addr.is_null(), "ws_open received a null address");
    assert!(
        !context.is_null(),
        "ws_open received a null factory context"
    );
    assert!(
        c4Socket_getNativeHandle(c4sock).is_null(),
        "ws_open found an existing native handle"
    );
    let handle: &Handle = &*(context as *mut Handle);

    let request = c4address_to_request(c4sock as usize, &*addr, options);
    info!(
        "c4sock {c4sock:?}: open was called with uri: {:?}",
        request.as_ref().map(Request::uri)
    );
    let (commands, command_rx) = mpsc::unbounded_channel();
    c4Socket_setNativeHandle(
        c4sock,
        Box::into_raw(Box::new(SocketImpl { commands })) as *mut c_void,
    );
    let c4sock = C4SocketPtr(c4sock);
    let terminal = Terminal::new(FfiCore::<ConnectingPhase>::new(c4sock));
    handle.spawn(async move { do_open(terminal, request, command_rx).await });
}

unsafe extern "C" fn ws_write(c4sock: *mut C4Socket, allocated_data: C4SliceResult) {
    trace!(
        "c4sock {c4sock:?}: write allocated_data.size {}",
        allocated_data.size
    );
    let socket = socket_impl(c4sock).as_ref();
    let data = Bytes::from_owner(allocated_data);
    let _ = socket.commands.send(Command::Write(data));
}

unsafe extern "C" fn ws_completed_receive(c4sock: *mut C4Socket, byte_count: usize) {
    trace!("c4sock {c4sock:?}: completedReceive, byte_count {byte_count}");
    let socket = socket_impl(c4sock).as_ref();
    let _ = socket.commands.send(Command::CompletedReceive(byte_count));
}

unsafe extern "C" fn ws_request_close(c4sock: *mut C4Socket, status: c_int, message: C4String) {
    trace!(
        "c4sock {c4sock:?}: requestClose status {status}, message: {:?}",
        std::str::from_utf8(message.into())
    );
    let reason = String::from_utf8_lossy(message.into()).into_owned();
    let socket = socket_impl(c4sock).as_ref();
    let _ = socket.commands.send(Command::RequestClose(status, reason));
}

unsafe extern "C" fn ws_dispose(c4sock: *mut C4Socket) {
    trace!("c4sock {c4sock:?}: dispose");
    assert!(!c4sock.is_null(), "ws_dispose received a null socket");
    let native = c4Socket_getNativeHandle(c4sock) as *mut SocketImpl;
    // LiteCore can destroy a socket before calling our open callback.
    if !native.is_null() {
        c4Socket_setNativeHandle(c4sock, std::ptr::null_mut());
        drop(Box::from_raw(native));
    }
}

unsafe fn socket_impl(c4sock: *mut C4Socket) -> NonNull<SocketImpl> {
    assert!(!c4sock.is_null(), "socket callback received a null socket");
    let native = c4Socket_getNativeHandle(c4sock) as *mut SocketImpl;
    NonNull::new(native).expect("socket callback received a socket without a native handle")
}

unsafe fn c4address_to_request(
    marker: usize,
    addr: &C4Address,
    options: C4Slice,
) -> Result<Request, Error> {
    let mut authority = Vec::with_capacity(addr.hostname.size + 1 + 5);
    authority.extend_from_slice(<&[u8]>::from(addr.hostname));
    write!(authority, ":{}", addr.port).expect("append to Vec failed");
    let uri = Uri::builder()
        .scheme(<&[u8]>::from(addr.scheme))
        .authority(authority)
        .path_and_query(<&[u8]>::from(addr.path))
        .build()?;
    trace!("c4address_to_request, marker {marker:x}, uri {uri:?}");
    let mut request = uri
        .into_client_request()
        .map_err(|err| tungstenite_err_to_c4_err(err))?;
    let options =
        NonNullConst::new(FLValue_FromData(options, FLTrust::kFLUntrusted)).ok_or_else(|| {
            Error(c4error_make(
                C4ErrorDomain::NetworkDomain,
                C4NetworkErrorCode::kC4NetErrInvalidURL.0,
                "options argument in open is not fleece Value".into(),
            ))
        })?;
    let options = NonNullConst::new(FLValue_AsDict(options.as_ptr())).ok_or_else(|| {
        Error(c4error_make(
            C4ErrorDomain::NetworkDomain,
            C4NetworkErrorCode::kC4NetErrInvalidURL.0,
            "options argument in open is not fleece Dict".into(),
        ))
    })?;

    if let ValueRef::Array(opts) = ValueRef::new(FLDict_Get(
        options.as_ptr(),
        kC4ReplicatorOptionExtraHeaders.into(),
    )) {
        // ExtraHeaders is an array, not a dict, so we need to split out the header keys
        let headers = request.headers_mut();
        for i in 0..opts.len() {
            let header = match opts.get(i) {
                ValueRef::String(x) => x,
                _ => {
                    return Err(Error(c4error_make(
                        C4ErrorDomain::LiteCoreDomain,
                        C4ErrorCode::kC4ErrorInvalidParameter.0,
                        "Header value was not string".into(),
                    )))
                }
            };
            let index = header.find(':').ok_or_else(|| {
                Error(c4error_make(
                    C4ErrorDomain::LiteCoreDomain,
                    C4ErrorCode::kC4ErrorInvalidParameter.0,
                    format!("Header value {header} does not appear to be valid")
                        .as_str()
                        .into(),
                ))
            })?;

            let name = &header[..index];
            let value = &header[index + 1..];

            headers.insert(HeaderName::from_str(name)?, HeaderValue::from_str(value)?);
        }
    }

    let mut cookies = Vec::<String>::new();

    if let ValueRef::Dict(auth) = ValueRef::new(FLDict_Get(
        options.as_ptr(),
        kC4ReplicatorOptionAuthentication.into(),
    )) {
        let auth_type = if let ValueRef::String(auth_type) = auth.get(kC4ReplicatorAuthType.into())
        {
            auth_type
        } else {
            // No auth type, default to Basic
            "Basic"
        };

        if auth_type == kC4AuthTypeBasic {
            let (username, password) = match (
                auth.get(kC4ReplicatorAuthUserName.into()),
                auth.get(kC4ReplicatorAuthPassword.into()),
            ) {
                (ValueRef::String(username), ValueRef::String(password)) => (username, password),
                _ => {
                    return Err(Error(c4error_make(
                        C4ErrorDomain::LiteCoreDomain,
                        C4ErrorCode::kC4ErrorInvalidParameter.0,
                        "Can not get username or password for basic auth type".into(),
                    )))
                }
            };

            let header = http_basic_auth_header(username, password);

            request
                .headers_mut()
                .insert("Authorization", HeaderValue::from_str(&header)?);
        } else if auth_type == kC4AuthTypeSession {
            if let ValueRef::String(token) = auth.get(kC4ReplicatorAuthToken.into()) {
                let token_cookie = format!("SyncGatewaySession={token}");
                cookies.push(token_cookie);
            }
        } else {
            return Err(Error(c4error_make(
                C4ErrorDomain::LiteCoreDomain,
                C4ErrorCode::kC4ErrorInvalidParameter.0,
                format!("Invalid {auth_type}, only Basic and Session auth types are implemented")
                    .as_str()
                    .into(),
            )));
        }
    }

    if let ValueRef::String(cookie) = ValueRef::new(FLDict_Get(
        options.as_ptr(),
        kC4ReplicatorOptionCookies.into(),
    )) {
        cookies.push(cookie.to_string());
    }

    if let ValueRef::String(protocol) = ValueRef::new(FLDict_Get(
        options.as_ptr(),
        kC4SocketOptionWSProtocols.into(),
    )) {
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", HeaderValue::from_str(protocol)?);
    }

    if !cookies.is_empty() {
        let cookies = cookies.join(";");

        request
            .headers_mut()
            .insert("Cookie", HeaderValue::from_str(cookies.as_str())?);
    }

    Ok(request)
}

async fn do_open<C>(
    mut terminal: Terminal<C>,
    request: Result<Request, Error>,
    command_rx: mpsc::UnboundedReceiver<Command>,
) where
    C: ConnectingSink,
    for<'a> &'a C: Into<usize>,
    for<'a> &'a C::Open: Into<usize>,
{
    let request = match request {
        Ok(request) => request,
        Err(error) => {
            terminal.finish(error.0);
            return;
        }
    };
    do_open_with_connect(terminal, connect_async(request), command_rx).await;
}

async fn do_open_with_connect<C, S, F>(
    mut terminal: Terminal<C>,
    connect: F,
    mut command_rx: mpsc::UnboundedReceiver<Command>,
) where
    C: ConnectingSink,
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    F: Future<Output = Result<(WebSocketStream<S>, Response), tungstenite::Error>>,
    for<'a> &'a C: Into<usize>,
    for<'a> &'a C::Open: Into<usize>,
{
    let context: usize = (&*terminal.core()).into();
    let mut connect = std::pin::pin!(connect);
    let (ws_stream, http_resp) = loop {
        tokio::select! {
            result = &mut connect => {
                trace!("c4sock {context:#x}: connect_async finished");
                match result {
                    Ok(result) => break result,
                    Err(error) => {
                        if let tungstenite::Error::Http(ref response) = error {
                            match unsafe { headers_to_dict(response) } {
                                Ok(headers) =>
                                    terminal.core().got_http_response(response.status().as_u16() as c_int, headers),
                                Err(err) =>
                                    error!("c4sock {context:#x}: failed to decode http headers to fleece dict: {err:?}"),
                            }
                        }
                        terminal.finish(unsafe { tungstenite_err_to_c4_err(error) }.0);
                        return;
                    }
                }
            }
            command = command_rx.recv() => match command {
                Some(Command::RequestClose(status, reason)) => {
                    terminal.finish(close_error(status, &reason));
                    return;
                }
                Some(Command::Write(_)) => {
                    terminal.finish(network_error(
                        C4NetworkErrorCode::kC4NetErrNotConnected,
                        "write before socket opened",
                    ));
                    return;
                }
                Some(Command::CompletedReceive(size)) => {
                    warn!("c4sock {context:#x}: completedReceive before open: {size} bytes");
                }
                None => {
                    terminal.finish(network_error(
                        C4NetworkErrorCode::kC4NetErrConnectionAborted,
                        "socket command channel closed while connecting",
                    ));
                    return;
                }
            }
        }
    };

    match unsafe { headers_to_dict(&http_resp) } {
        Ok(headers) => terminal
            .core()
            .got_http_response(http_resp.status().as_u16() as c_int, headers),
        Err(error) => {
            terminal.finish(Error::from(error).0);
            return;
        }
    }
    drop(http_resp);
    run_open_socket(terminal, ws_stream, command_rx).await;
}

async fn run_open_socket<C, S>(
    terminal: Terminal<C>,
    ws_stream: WebSocketStream<S>,
    command_rx: mpsc::UnboundedReceiver<Command>,
) where
    C: ConnectingSink,
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    for<'a> &'a C::Open: Into<usize>,
{
    let (ws_writer, ws_reader) = ws_stream.split();
    let (writer_tx, writer_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let writer_task = tokio::spawn(writer_loop(ws_writer, writer_rx, event_tx));
    let _writer_guard = AbortWriter(writer_task.abort_handle());
    let mut terminal = terminal.opened();
    match main_event_loop(terminal.core(), ws_reader, command_rx, writer_tx, event_rx).await {
        Ok(()) => terminal.finish(network_error(
            C4NetworkErrorCode::kC4NetErrConnectionAborted,
            "WebSocket read loop exited",
        )),
        Err(error) => terminal.finish(error.0),
    }
}

async fn writer_loop<S>(
    mut writer: WsWriter<S>,
    mut commands: mpsc::UnboundedReceiver<WriterCommand>,
    events: mpsc::UnboundedSender<WriterEvent>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    while let Some(command) = commands.recv().await {
        match command {
            WriterCommand::Write(data) => {
                let size = data.len();
                if let Err(error) = writer.send(Message::Binary(data)).await {
                    let _ = events.send(WriterEvent::Failed(error.into()));
                    return;
                }
                if events.send(WriterEvent::Written(size)).is_err() {
                    return;
                }
            }
            WriterCommand::Close(frame) => {
                if let Err(error) = writer.send(Message::Close(Some(frame))).await {
                    let _ = events.send(WriterEvent::Failed(error.into()));
                    return;
                }
                if events.send(WriterEvent::CloseSent).is_err() {
                    return;
                }
            }
            WriterCommand::FlushCloseReply => {
                if let Err(error) = writer.flush().await {
                    let _ = events.send(WriterEvent::Failed(error.into()));
                    return;
                }
                if events.send(WriterEvent::CloseReplyFlushed).is_err() {
                    return;
                }
            }
        }
    }
}

async fn main_event_loop<C, S>(
    core: &mut C,
    mut ws_reader: WsReader<S>,
    mut command_rx: mpsc::UnboundedReceiver<Command>,
    writer_tx: mpsc::UnboundedSender<WriterCommand>,
    mut event_rx: mpsc::UnboundedReceiver<WriterEvent>,
) -> Result<(), Error>
where
    C: OpenSink,
    S: AsyncRead + AsyncWrite + Unpin,
    for<'a> &'a C: Into<usize>,
{
    let context: usize = (&*core).into();
    let mut outstanding = 0usize;
    let mut state = CloseState::Open;
    loop {
        let deadline = match state {
            CloseState::Open => None,
            CloseState::Local { deadline, .. } | CloseState::Peer { deadline, .. } => {
                Some(deadline)
            }
        };
        tokio::select! {
            command = command_rx.recv() => match command {
                Some(Command::Write(data)) => {
                    if matches!(state, CloseState::Open) {
                        writer_tx.send(WriterCommand::Write(data)).map_err(|_| {
                            Error(network_error(
                                C4NetworkErrorCode::kC4NetErrConnectionAborted,
                                "writer task stopped",
                            ))
                        })?;
                    }
                }
                Some(Command::CompletedReceive(size)) => {
                    if size > outstanding {
                        warn!("c4sock {context:#x}: completedReceive acknowledged {size} bytes, only {outstanding} pending");
                        debug_assert!(false);
                    }
                    outstanding = outstanding.saturating_sub(size);
                }
                Some(Command::RequestClose(status, reason)) => match state {
                    CloseState::Open => {
                        let error = close_error(status, &reason);
                        let code = u16::try_from(status)
                            .ok()
                            .map(CloseCode::from)
                            .filter(|code| code.is_allowed());
                        let Some(code) = code else {
                            warn!("c4sock {context:#x}: invalid close status {status}, closing transport without a close frame");
                            return Err(Error(error));
                        };
                        writer_tx.send(WriterCommand::Close(CloseFrame {
                            code,
                            reason: reason.into(),
                        })).map_err(|_| Error(network_error(
                            C4NetworkErrorCode::kC4NetErrConnectionAborted,
                            "writer task stopped before close frame",
                        )))?;
                        state = CloseState::Local {
                            deadline: Instant::now() + CLOSE_TIMEOUT,
                            error,
                        };
                    }
                    CloseState::Peer { deadline, error, reply_requested } => {
                        if !reply_requested {
                            writer_tx.send(WriterCommand::FlushCloseReply).map_err(|_| {
                                Error(network_error(
                                    C4NetworkErrorCode::kC4NetErrConnectionAborted,
                                    "writer task stopped before close reply",
                                ))
                            })?;
                            state = CloseState::Peer { deadline, error, reply_requested: true };
                        }
                    }
                    CloseState::Local { .. } => {
                        warn!("c4sock {context:#x}: duplicate requestClose ignored");
                    }
                },
                None => return Err(Error(network_error(
                    C4NetworkErrorCode::kC4NetErrConnectionAborted,
                    "socket command channel closed",
                ))),
            },
            event = event_rx.recv() => match event {
                Some(WriterEvent::Written(size)) => core.completed_write(size),
                Some(WriterEvent::CloseSent) => trace!("c4sock {context:#x}: local WebSocket close sent"),
                Some(WriterEvent::CloseReplyFlushed) => {
                    if let CloseState::Peer { error, .. } = state {
                        return Err(Error(error));
                    }
                }
                Some(WriterEvent::Failed(error)) => {
                    return Err(unsafe { tungstenite_err_to_c4_err(*error) });
                }
                None => return Err(Error(network_error(
                    C4NetworkErrorCode::kC4NetErrConnectionAborted,
                    "writer task stopped",
                ))),
            },
            message = ws_reader.next(), if outstanding == 0 && !matches!(state, CloseState::Peer { .. }) => match message {
                Some(Ok(message)) => match message {
                    m @ Message::Text(_) | m @ Message::Binary(_) => {
                        let data = m.into_data();
                        if data.is_empty() {
                            return Err(Error(unsafe { c4error_make(
                                C4ErrorDomain::LiteCoreDomain,
                                C4ErrorCode::kC4ErrorCorruptData.0,
                                "empty BLIP message".into(),
                            ) }));
                        }
                        outstanding = data.len();
                        core.received(data.as_ref());
                    }
                    Message::Close(close_frame) => {
                        let (code, reason) = close_frame
                            .map(|frame| (u16::from(frame.code) as c_int, frame.reason.to_string()))
                            .unwrap_or((C4WebSocketCloseCode::kWebSocketCloseNoCode.0, String::new()));
                        info!("c4sock {context:#x}: close frame was received: {code} {reason}");
                        match state {
                            CloseState::Open => {
                                state = CloseState::Peer {
                                    deadline: Instant::now() + CLOSE_TIMEOUT,
                                    error: close_error(code, &reason),
                                    reply_requested: false,
                                };
                                core.close_requested(code, &reason);
                            }
                            CloseState::Local { error, .. } => return Err(Error(error)),
                            CloseState::Peer { .. } => unreachable!("c4sock {context:#x}: peer read is disabled after close"),
                        }
                    }
                    Message::Frame(_) => warn!("c4sock {context:#x}: unsupported Message::Frame format"),
                    Message::Ping(_) => trace!("c4sock {context:#x}: ping frame was received"),
                    Message::Pong(_) => trace!("c4sock {context:#x}: pong frame was received"),
                },
                Some(Err(error)) => return Err(unsafe { tungstenite_err_to_c4_err(error) }),
                None => return Err(Error(network_error(
                    C4NetworkErrorCode::kC4NetErrConnectionAborted,
                    "WebSocket stream ended without a close frame",
                ))),
            },
            _ = async { if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await } }, if deadline.is_some() => {
                return Err(Error(network_error(
                    C4NetworkErrorCode::kC4NetErrTimeout,
                    "WebSocket close timed out",
                )));
            }
        }
    }
}

unsafe fn tungstenite_err_to_c4_err(err: tungstenite::Error) -> Error {
    use std::io::ErrorKind as IoErrorKind;
    use tungstenite::error::{CapacityError, Error::*, ProtocolError};
    let msg = err.to_string();
    let (domain, code) = match err {
        ConnectionClosed => (
            C4ErrorDomain::WebSocketDomain,
            C4WebSocketCloseCode::kWebSocketCloseNormal.0,
        ),
        AlreadyClosed => (
            C4ErrorDomain::WebSocketDomain,
            C4WebSocketCloseCode::kWebSocketCloseFirstAvailable.0,
        ),
        Io(error) => {
            let code = match error.kind() {
                IoErrorKind::ConnectionRefused => C4NetworkErrorCode::kC4NetErrConnectionRefused,
                IoErrorKind::ConnectionReset => C4NetworkErrorCode::kC4NetErrConnectionReset,
                IoErrorKind::ConnectionAborted => C4NetworkErrorCode::kC4NetErrConnectionAborted,
                IoErrorKind::NotConnected => C4NetworkErrorCode::kC4NetErrNotConnected,
                IoErrorKind::AddrNotAvailable => C4NetworkErrorCode::kC4NetErrAddressNotAvailable,
                IoErrorKind::BrokenPipe => C4NetworkErrorCode::kC4NetErrBrokenPipe,
                IoErrorKind::TimedOut => C4NetworkErrorCode::kC4NetErrTimeout,
                _ => C4NetworkErrorCode::kC4NetErrUnknown,
            };
            (C4ErrorDomain::NetworkDomain, code.0)
        }
        Capacity(CapacityError::MessageTooLong { .. }) => (
            C4ErrorDomain::WebSocketDomain,
            C4WebSocketCloseCode::kWebSocketCloseMessageTooBig.0,
        ),
        Capacity(CapacityError::TooManyHeaders) => (
            C4ErrorDomain::WebSocketDomain,
            http::StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE
                .as_u16()
                .into(),
        ),
        WriteBufferFull(_) => (
            C4ErrorDomain::NetworkDomain,
            C4NetworkErrorCode::kC4NetErrUnknown.0,
        ),
        Protocol(ProtocolError::ResetWithoutClosingHandshake) => (
            C4ErrorDomain::WebSocketDomain,
            C4WebSocketCloseCode::kWebSocketCloseAbnormal.0,
        ),
        Protocol(ProtocolError::SendAfterClosing) => (
            C4ErrorDomain::LiteCoreDomain,
            C4ErrorCode::kC4ErrorUnexpectedError.0,
        ),
        Protocol(
            ProtocolError::ReceivedAfterClosing
            | ProtocolError::NonZeroReservedBits
            | ProtocolError::UnmaskedFrameFromClient
            | ProtocolError::MaskedFrameFromServer
            | ProtocolError::FragmentedControlFrame
            | ProtocolError::ControlFrameTooBig
            | ProtocolError::UnknownControlFrameType(_)
            | ProtocolError::UnknownDataFrameType(_)
            | ProtocolError::UnexpectedContinueFrame
            | ProtocolError::ExpectedFragment(_)
            | ProtocolError::InvalidOpcode(_)
            | ProtocolError::InvalidCloseSequence,
        ) => (
            C4ErrorDomain::WebSocketDomain,
            C4WebSocketCloseCode::kWebSocketCloseProtocolError.0,
        ),
        Protocol(_) => (
            C4ErrorDomain::NetworkDomain,
            C4NetworkErrorCode::kC4NetErrUnknown.0,
        ),
        AttackAttempt => (
            C4ErrorDomain::NetworkDomain,
            C4NetworkErrorCode::kC4NetErrUnknown.0,
        ),
        Utf8(_) => (
            C4ErrorDomain::WebSocketDomain,
            C4WebSocketCloseCode::kWebSocketCloseBadMessageFormat.0,
        ),
        Url(_) => (
            C4ErrorDomain::NetworkDomain,
            C4NetworkErrorCode::kC4NetErrInvalidURL.0,
        ),
        Http(ref code) => (
            C4ErrorDomain::WebSocketDomain,
            code.status().as_u16().into(),
        ),
        HttpFormat(_) => (
            C4ErrorDomain::WebSocketDomain,
            C4WebSocketCloseCode::kWebSocketCloseBadMessageFormat.0,
        ),
        Tls(_) => (
            C4ErrorDomain::NetworkDomain,
            C4NetworkErrorCode::kC4NetErrTLSHandshakeFailed.0,
        ),
    };
    Error(c4error_make(domain, code as c_int, msg.as_bytes().into()))
}

unsafe fn headers_to_dict<T>(http_resp: &http::Response<T>) -> Result<FLSliceResult, FLError> {
    let enc = FLEncoder_New();
    let mut all_ok = true;
    all_ok &= FLEncoder_BeginDict(enc, http_resp.headers().len());
    for (key, value) in http_resp.headers().iter() {
        all_ok &= FLEncoder_WriteKey(enc, key.as_str().into());
        all_ok &= FLEncoder_WriteString(enc, value.as_bytes().into());
    }
    all_ok &= FLEncoder_EndDict(enc);
    let mut fl_err = FLError::kFLNoError;
    let res = FLEncoder_Finish(enc, &mut fl_err);
    FLEncoder_Free(enc);
    if !res.is_empty() && all_ok {
        Ok(res)
    } else {
        Err(fl_err)
    }
}

impl From<http::Error> for Error {
    fn from(err: http::Error) -> Self {
        let msg = err.to_string();

        Self(unsafe {
            c4error_make(
                C4ErrorDomain::NetworkDomain,
                C4NetworkErrorCode::kC4NetErrInvalidURL.0,
                msg.as_bytes().into(),
            )
        })
    }
}

impl From<http::header::InvalidHeaderName> for Error {
    fn from(err: http::header::InvalidHeaderName) -> Self {
        let msg = err.to_string();

        Self(unsafe {
            c4error_make(
                C4ErrorDomain::NetworkDomain,
                C4NetworkErrorCode::kC4NetErrInvalidURL.0,
                msg.as_bytes().into(),
            )
        })
    }
}

impl From<http::header::InvalidHeaderValue> for Error {
    fn from(err: http::header::InvalidHeaderValue) -> Self {
        let msg = err.to_string();

        Self(unsafe {
            c4error_make(
                C4ErrorDomain::NetworkDomain,
                C4NetworkErrorCode::kC4NetErrInvalidURL.0,
                msg.as_bytes().into(),
            )
        })
    }
}

impl From<FLError> for Error {
    fn from(fl_err: FLError) -> Self {
        Self(unsafe {
            c4error_make(
                C4ErrorDomain::FleeceDomain,
                fl_err.0 as c_int,
                "fleece error".into(),
            )
        })
    }
}

fn http_basic_auth_header(user_id: &str, password: &str) -> String {
    let credentials = format!("{user_id}:{password}");
    let mut ret = "Basic ".to_string();
    BASE64_STANDARD.encode_string(credentials.as_bytes(), &mut ret);
    ret
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::sync::{oneshot, Notify};

    struct CloseOnlySink;

    impl From<&CloseOnlySink> for usize {
        fn from(_: &CloseOnlySink) -> Self {
            0
        }
    }

    impl OpenSink for CloseOnlySink {
        fn received(&mut self, _: &[u8]) {
            panic!("no data message expected");
        }

        fn completed_write(&mut self, _: usize) {
            panic!("no write was requested");
        }

        fn close_requested(&mut self, _: c_int, _: &str) {
            panic!("the local close should not request a close from LiteCore");
        }
    }

    #[test]
    fn test_basic_auth_encode() {
        assert_eq!(
            "Basic Ym9iOnNlY3JldA==",
            http_basic_auth_header("bob", "secret")
        );
    }

    #[tokio::test]
    async fn test_failed_handshake_reports_http_response() {
        use std::io::Read;

        struct RecordingSink(Arc<Mutex<Vec<(c_int, String)>>>, Arc<Mutex<Vec<C4Error>>>);

        impl From<&RecordingSink> for usize {
            fn from(_: &RecordingSink) -> Self {
                0
            }
        }

        impl CloseSink for RecordingSink {
            fn closed(self, error: C4Error) {
                self.1.lock().unwrap().push(error);
            }
        }

        impl ConnectingSink for RecordingSink {
            type Open = FfiCore<OpenPhase>;

            fn got_http_response(&mut self, status: c_int, headers: FLSliceResult) {
                let header = unsafe {
                    let value = FLValue_FromData(headers.as_fl_slice(), FLTrust::kFLUntrusted);
                    let dict = FLValue_AsDict(value);
                    match ValueRef::new(FLDict_Get(dict, "x-test".into())) {
                        ValueRef::String(value) => value.to_owned(),
                        other => panic!("missing response header: {other:?}"),
                    }
                };
                self.0.lock().unwrap().push((status, header));
            }

            fn opened(self) -> Self::Open {
                panic!("a rejected handshake must not open the socket")
            }
        }

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            stream.read(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 403 Forbidden\r\nX-Test: rejected\r\nContent-Length: 0\r\n\r\n",
                )
                .unwrap();
        });

        let responses = Arc::new(Mutex::new(Vec::new()));
        let closes = Arc::new(Mutex::new(Vec::new()));
        let (_commands, command_rx) = mpsc::unbounded_channel();
        do_open(
            Terminal::new(RecordingSink(responses.clone(), closes.clone())),
            Ok(format!("ws://{address}/").into_client_request().unwrap()),
            command_rx,
        )
        .await;
        server.join().unwrap();

        assert_eq!(closes.lock().unwrap().len(), 1);
        assert_eq!(
            closes.lock().unwrap()[0].domain,
            C4ErrorDomain::WebSocketDomain
        );
        assert_eq!(closes.lock().unwrap()[0].code, 403);
        assert_eq!(*responses.lock().unwrap(), [(403, "rejected".to_owned())]);
    }

    #[tokio::test]
    async fn test_request_close_before_open_preserves_status() {
        struct NeverOpened(Arc<Mutex<Vec<C4Error>>>);

        impl From<&NeverOpened> for usize {
            fn from(_: &NeverOpened) -> Self {
                0
            }
        }

        impl CloseSink for NeverOpened {
            fn closed(self, error: C4Error) {
                self.0.lock().unwrap().push(error);
            }
        }

        impl ConnectingSink for NeverOpened {
            type Open = FfiCore<OpenPhase>;

            fn got_http_response(&mut self, _: c_int, _: FLSliceResult) {
                panic!("the handshake must not finish");
            }

            fn opened(self) -> Self::Open {
                panic!("the handshake must not finish");
            }
        }

        let (commands, command_rx) = mpsc::unbounded_channel();
        let closes = Arc::new(Mutex::new(Vec::new()));
        let (connecting_tx, connecting_rx) = oneshot::channel();
        let connect = async move {
            let _ = connecting_tx.send(());
            std::future::pending::<
                Result<(WebSocketStream<tokio::io::DuplexStream>, Response), tungstenite::Error>,
            >()
            .await
        };
        let open_task = tokio::spawn(do_open_with_connect(
            Terminal::new(NeverOpened(closes.clone())),
            connect,
            command_rx,
        ));

        tokio::time::timeout(Duration::from_secs(2), connecting_rx)
            .await
            .unwrap()
            .unwrap();
        commands
            .send(Command::RequestClose(504, "Timed out".into()))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), open_task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(closes.lock().unwrap().len(), 1);
        assert_eq!(
            closes.lock().unwrap()[0].domain,
            C4ErrorDomain::WebSocketDomain
        );
        assert_eq!(closes.lock().unwrap()[0].code, 504);
    }

    #[tokio::test]
    async fn test_cancelled_connect_reports_closed_once() {
        struct NeverOpened(Arc<Mutex<Vec<C4Error>>>);

        impl From<&NeverOpened> for usize {
            fn from(_: &NeverOpened) -> Self {
                0
            }
        }

        impl CloseSink for NeverOpened {
            fn closed(self, error: C4Error) {
                self.0.lock().unwrap().push(error);
            }
        }

        impl ConnectingSink for NeverOpened {
            type Open = FfiCore<OpenPhase>;

            fn got_http_response(&mut self, _: c_int, _: FLSliceResult) {
                panic!("the handshake must not finish");
            }

            fn opened(self) -> Self::Open {
                panic!("the handshake must not finish");
            }
        }

        let closes = Arc::new(Mutex::new(Vec::new()));
        let (_commands, command_rx) = mpsc::unbounded_channel();
        let (started_tx, started_rx) = oneshot::channel();
        let connect = async move {
            let _ = started_tx.send(());
            std::future::pending::<
                Result<(WebSocketStream<tokio::io::DuplexStream>, Response), tungstenite::Error>,
            >()
            .await
        };
        let task = tokio::spawn(do_open_with_connect(
            Terminal::new(NeverOpened(closes.clone())),
            connect,
            command_rx,
        ));
        started_rx.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let closes = closes.lock().unwrap();
        assert_eq!(closes.len(), 1);
        assert_eq!(closes[0].domain, C4ErrorDomain::NetworkDomain);
        assert_eq!(
            closes[0].code,
            C4NetworkErrorCode::kC4NetErrConnectionAborted.0
        );
    }

    #[tokio::test]
    async fn test_cancelled_open_socket_reports_closed_once() {
        use tokio_tungstenite::tungstenite::protocol::Role;

        struct ConnectingRecorder {
            closes: Arc<Mutex<Vec<C4Error>>>,
            opened: oneshot::Sender<()>,
        }

        struct OpenRecorder(Arc<Mutex<Vec<C4Error>>>);

        impl From<&ConnectingRecorder> for usize {
            fn from(_: &ConnectingRecorder) -> Self {
                0
            }
        }

        impl From<&OpenRecorder> for usize {
            fn from(_: &OpenRecorder) -> Self {
                0
            }
        }

        impl CloseSink for ConnectingRecorder {
            fn closed(self, error: C4Error) {
                self.closes.lock().unwrap().push(error);
            }
        }

        impl CloseSink for OpenRecorder {
            fn closed(self, error: C4Error) {
                self.0.lock().unwrap().push(error);
            }
        }

        impl ConnectingSink for ConnectingRecorder {
            type Open = OpenRecorder;

            fn got_http_response(&mut self, _: c_int, _: FLSliceResult) {}

            fn opened(self) -> Self::Open {
                self.opened.send(()).unwrap();
                OpenRecorder(self.closes)
            }
        }

        impl OpenSink for OpenRecorder {
            fn received(&mut self, _: &[u8]) {
                panic!("no data message expected");
            }

            fn completed_write(&mut self, _: usize) {
                panic!("no write was requested");
            }

            fn close_requested(&mut self, _: c_int, _: &str) {
                panic!("no close frame expected");
            }
        }

        let (client_io, server_io) = tokio::io::duplex(4096);
        let client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let _server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let response = http::Response::builder().status(101).body(None).unwrap();
        let (_commands, command_rx) = mpsc::unbounded_channel();
        let closes = Arc::new(Mutex::new(Vec::new()));
        let (opened_tx, opened_rx) = oneshot::channel();
        let terminal = Terminal::new(ConnectingRecorder {
            closes: closes.clone(),
            opened: opened_tx,
        });
        let task = tokio::spawn(do_open_with_connect(
            terminal,
            std::future::ready(Ok((client, response))),
            command_rx,
        ));
        opened_rx.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let closes = closes.lock().unwrap();
        assert_eq!(closes.len(), 1);
        assert_eq!(closes[0].domain, C4ErrorDomain::NetworkDomain);
        assert_eq!(
            closes[0].code,
            C4NetworkErrorCode::kC4NetErrConnectionAborted.0
        );
    }

    #[tokio::test]
    async fn test_empty_websocket_message_is_rejected_before_litecore() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        struct NoMessages;

        impl From<&NoMessages> for usize {
            fn from(_: &NoMessages) -> Self {
                0
            }
        }

        impl OpenSink for NoMessages {
            fn received(&mut self, _: &[u8]) {
                panic!("empty message must not reach LiteCore");
            }

            fn completed_write(&mut self, _: usize) {
                panic!("no write was requested");
            }

            fn close_requested(&mut self, _: c_int, _: &str) {
                panic!("no close frame expected");
            }
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.ends_with(b"\r\n\r\n") {
                let count = tcp.read(&mut buffer).await.unwrap();
                assert!(count > 0 && request.len() + count <= 4096);
                request.extend_from_slice(&buffer[..count]);
            }
            let request = std::str::from_utf8(&request).unwrap();
            let key = request
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("Sec-WebSocket-Key"))
                .unwrap()
                .1
                .trim();
            let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
            let response = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            );
            tcp.write_all(response.as_bytes()).await.unwrap();
            tcp.write_all(&[0x82, 0]).await.unwrap();
            tcp
        });

        let (socket, _) = connect_async(format!("ws://{address}/")).await.unwrap();
        let (_writer, reader) = socket.split();
        let _server_tcp = server.await.unwrap();
        let (_commands, command_rx) = mpsc::unbounded_channel();
        let (writer_tx, _writer_rx) = mpsc::unbounded_channel();
        let (_written_tx, written_rx) = mpsc::unbounded_channel();
        let mut sink = NoMessages;
        let error = tokio::time::timeout(
            Duration::from_secs(2),
            main_event_loop(&mut sink, reader, command_rx, writer_tx, written_rx),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(error.0.domain, C4ErrorDomain::LiteCoreDomain);
        assert_eq!(error.0.code, C4ErrorCode::kC4ErrorCorruptData.0);
    }

    #[tokio::test]
    async fn test_peer_close_reports_status_and_reason() {
        use tokio_tungstenite::tungstenite::protocol::Role;

        struct RecordingSink(
            Arc<Mutex<Vec<(c_int, String)>>>,
            mpsc::UnboundedSender<Command>,
        );

        impl From<&RecordingSink> for usize {
            fn from(_: &RecordingSink) -> Self {
                0
            }
        }

        impl OpenSink for RecordingSink {
            fn received(&mut self, _: &[u8]) {
                panic!("no data message expected");
            }

            fn completed_write(&mut self, _: usize) {
                panic!("no write was requested");
            }

            fn close_requested(&mut self, status: c_int, reason: &str) {
                self.0.lock().unwrap().push((status, reason.to_owned()));
                self.1
                    .send(Command::RequestClose(status, reason.to_owned()))
                    .unwrap();
            }
        }

        let (client_io, server_io) = tokio::io::duplex(4096);
        let client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let (writer, reader) = client.split();
        server
            .send(Message::Close(Some(CloseFrame {
                code: CloseCode::Normal,
                reason: "peer shutdown".into(),
            })))
            .await
            .unwrap();

        let callbacks = Arc::new(Mutex::new(Vec::new()));
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (writer_tx, writer_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let writer_task = tokio::spawn(writer_loop(writer, writer_rx, event_tx));
        let mut sink = RecordingSink(callbacks.clone(), commands);
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            main_event_loop(&mut sink, reader, command_rx, writer_tx, event_rx),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(result.0.domain, C4ErrorDomain::WebSocketDomain);
        assert_eq!(result.0.code, 1000);
        assert!(matches!(server.next().await, Some(Ok(Message::Close(_)))));
        writer_task.await.unwrap();
        assert_eq!(*callbacks.lock().unwrap(), [(1000, "peer shutdown".into())]);
    }

    #[tokio::test]
    async fn test_local_close_waits_for_peer_close_frame() {
        use tokio_tungstenite::tungstenite::protocol::Role;

        let (client_io, server_io) = tokio::io::duplex(4096);
        let client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let (writer, reader) = client.split();
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (writer_tx, writer_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let writer_task = tokio::spawn(writer_loop(writer, writer_rx, event_tx));
        let read_task = tokio::spawn(async move {
            let mut sink = CloseOnlySink;
            main_event_loop(&mut sink, reader, command_rx, writer_tx, event_rx).await
        });

        commands
            .send(Command::RequestClose(1000, "done".into()))
            .unwrap();
        assert!(matches!(server.next().await, Some(Ok(Message::Close(_)))));
        assert!(!read_task.is_finished());
        server.flush().await.unwrap();

        let error = tokio::time::timeout(Duration::from_secs(2), read_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.0.domain, C4ErrorDomain::WebSocketDomain);
        assert_eq!(error.0.code, 1000);
        writer_task.await.unwrap();
    }

    #[tokio::test]
    async fn test_invalid_close_status_sends_no_close_frame() {
        use tokio_tungstenite::tungstenite::protocol::Role;

        let (client_io, server_io) = tokio::io::duplex(4096);
        let client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let (writer, reader) = client.split();
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (writer_tx, writer_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let writer_task = tokio::spawn(writer_loop(writer, writer_rx, event_tx));
        commands
            .send(Command::RequestClose(504, "timeout".into()))
            .unwrap();
        let mut sink = CloseOnlySink;
        let error = main_event_loop(&mut sink, reader, command_rx, writer_tx, event_rx)
            .await
            .unwrap_err();

        assert_eq!(error.0.domain, C4ErrorDomain::WebSocketDomain);
        assert_eq!(error.0.code, 504);
        writer_task.await.unwrap();
        let message = tokio::time::timeout(Duration::from_secs(2), server.next())
            .await
            .unwrap();
        assert!(!matches!(message, Some(Ok(Message::Close(_)))));
    }

    #[tokio::test]
    async fn test_next_message_waits_for_completed_receive() {
        use tokio_tungstenite::tungstenite::protocol::Role;

        struct RecordingSink(Arc<Mutex<Vec<Vec<u8>>>>, Arc<Notify>);

        impl From<&RecordingSink> for usize {
            fn from(_: &RecordingSink) -> Self {
                0
            }
        }

        impl OpenSink for RecordingSink {
            fn received(&mut self, data: &[u8]) {
                self.0.lock().unwrap().push(data.to_vec());
                self.1.notify_one();
            }

            fn completed_write(&mut self, _: usize) {
                panic!("no write was requested");
            }

            fn close_requested(&mut self, _: c_int, _: &str) {
                panic!("no close frame expected");
            }
        }

        let (client_io, server_io) = tokio::io::duplex(4096);
        let client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let (_writer, reader) = client.split();
        server
            .send(Message::Binary(Bytes::from_static(b"one")))
            .await
            .unwrap();
        server
            .send(Message::Binary(Bytes::from_static(b"two")))
            .await
            .unwrap();

        let messages = Arc::new(Mutex::new(Vec::new()));
        let received = Arc::new(Notify::new());
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (writer_tx, _writer_rx) = mpsc::unbounded_channel();
        let (_event_tx, event_rx) = mpsc::unbounded_channel();
        let messages_for_task = messages.clone();
        let received_for_task = received.clone();
        let read_task = tokio::spawn(async move {
            let mut sink = RecordingSink(messages_for_task, received_for_task);
            main_event_loop(&mut sink, reader, command_rx, writer_tx, event_rx).await
        });

        received.notified().await;
        tokio::task::yield_now().await;
        assert_eq!(*messages.lock().unwrap(), [b"one".to_vec()]);
        commands.send(Command::CompletedReceive(1)).unwrap();
        tokio::task::yield_now().await;
        assert_eq!(messages.lock().unwrap().len(), 1);
        commands.send(Command::CompletedReceive(2)).unwrap();
        tokio::time::timeout(Duration::from_secs(2), received.notified())
            .await
            .unwrap();
        assert_eq!(
            *messages.lock().unwrap(),
            [b"one".to_vec(), b"two".to_vec()]
        );

        drop(commands);
        assert_eq!(
            read_task.await.unwrap().unwrap_err().0.domain,
            C4ErrorDomain::NetworkDomain
        );
    }

    #[tokio::test]
    async fn test_writer_reports_transport_failure() {
        use tokio_tungstenite::tungstenite::protocol::Role;

        let (client_io, server_io) = tokio::io::duplex(4096);
        let client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let (writer, _reader) = client.split();
        drop(server_io);

        let (commands, command_rx) = mpsc::unbounded_channel();
        let (events, mut event_rx) = mpsc::unbounded_channel();
        let writer_task = tokio::spawn(writer_loop(writer, command_rx, events));
        commands
            .send(WriterCommand::Write(Bytes::from_static(b"payload")))
            .unwrap();

        let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .unwrap();
        assert!(matches!(event, Some(WriterEvent::Failed(_))));
        writer_task.await.unwrap();
    }

    #[tokio::test]
    async fn test_close_progresses_while_receive_acknowledgement_is_pending() {
        use tokio_tungstenite::tungstenite::protocol::Role;

        struct RecordingSink(Arc<Notify>);

        impl From<&RecordingSink> for usize {
            fn from(_: &RecordingSink) -> Self {
                0
            }
        }

        impl OpenSink for RecordingSink {
            fn received(&mut self, _: &[u8]) {
                self.0.notify_one();
            }

            fn completed_write(&mut self, _: usize) {
                panic!("no write was requested");
            }

            fn close_requested(&mut self, _: c_int, _: &str) {
                panic!("the peer did not send a close frame");
            }
        }

        let (client_io, server_io) = tokio::io::duplex(4096);
        let client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let (writer, reader) = client.split();
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (writer_tx, writer_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let writer_task = tokio::spawn(writer_loop(writer, writer_rx, event_tx));
        let received = Arc::new(Notify::new());
        let received_for_task = received.clone();
        let read_task = tokio::spawn(async move {
            let mut sink = RecordingSink(received_for_task);
            main_event_loop(&mut sink, reader, command_rx, writer_tx, event_rx).await
        });

        server
            .send(Message::Binary(Bytes::from_static(b"data")))
            .await
            .unwrap();
        received.notified().await;
        commands
            .send(Command::RequestClose(1000, "done".into()))
            .unwrap();
        let close = tokio::time::timeout(Duration::from_secs(2), server.next())
            .await
            .unwrap();
        assert!(matches!(close, Some(Ok(Message::Close(_)))));
        commands.send(Command::CompletedReceive(4)).unwrap();
        server.flush().await.unwrap();

        let error = tokio::time::timeout(Duration::from_secs(2), read_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.0.domain, C4ErrorDomain::WebSocketDomain);
        assert_eq!(error.0.code, 1000);
        writer_task.await.unwrap();
    }
}
