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
    mem,
    os::raw::{c_int, c_void},
    ptr::NonNull,
    str::FromStr,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    runtime::Handle,
    sync::{mpsc, oneshot, Mutex as TokioMutex, Notify},
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
    handle: Handle,
    commands: mpsc::UnboundedSender<Command>,
    send_queue: mpsc::UnboundedSender<WriterCommand>,
    close_control: Arc<CloseControl>,
}

enum Command {
    CompletedReceive(usize),
}

enum WriterCommand {
    Write(Bytes),
    Close(CloseFrame, oneshot::Sender<()>),
}

struct CloseControl {
    confirm: Notify,
    stop_read_loop: TokioMutex<Option<oneshot::Sender<()>>>,
    stop_error: Mutex<Option<C4Error>>,
    state: AtomicLegacyCloseState,
    signaled: AtomicBool,
    writer_task: Mutex<Option<tokio::task::AbortHandle>>,
}

#[repr(u8)]
#[derive(Debug)]
enum LegacyCloseState {
    None = 0,
    Server = 1,
    Client = 2,
}

#[repr(transparent)]
struct AtomicLegacyCloseState(AtomicU8);
impl AtomicLegacyCloseState {
    fn new(val: LegacyCloseState) -> Self {
        Self(AtomicU8::new(val as u8))
    }
    // Claim an unstarted close and return the state observed before the attempt.
    fn claim(&self, new_state: LegacyCloseState) -> LegacyCloseState {
        let previous = match self.0.compare_exchange(
            LegacyCloseState::None as u8,
            new_state as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(previous) | Err(previous) => previous,
        };
        match previous {
            0 => LegacyCloseState::None,
            1 => LegacyCloseState::Server,
            2 => LegacyCloseState::Client,
            _ => unreachable!(),
        }
    }
}

type WsReader<S> = SplitStream<WebSocketStream<S>>;
type WsWriter<S> = SplitSink<WebSocketStream<S>, Message>;

#[repr(transparent)]
#[derive(Clone, Copy, Debug)]
struct C4SocketPtr(*mut C4Socket);
unsafe impl Send for C4SocketPtr {}

trait WriteSink {
    fn completed_write(&mut self, size: usize);
}

impl WriteSink for C4SocketPtr {
    fn completed_write(&mut self, size: usize) {
        unsafe { c4socket_completedWrite(self.0, size) };
    }
}

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

trait ConnectingSink: Send + 'static {
    type Open: OpenSink;

    fn socket(&self) -> C4SocketPtr;
    fn got_http_response(&mut self, status: c_int, headers: FLSliceResult);
    fn opened(self) -> Self::Open;
}

trait OpenSink: Send + 'static {
    fn received(&mut self, data: &[u8]);
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

    fn socket(&self) -> C4SocketPtr {
        self.socket
    }

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
    let (stop_tx, stop_rx) = oneshot::channel();
    let (commands, command_rx) = mpsc::unbounded_channel();
    let (send_q_tx, send_q_rx) = mpsc::unbounded_channel();
    let sock_impl = Box::new(SocketImpl {
        handle: handle.clone(),
        commands,
        close_control: Arc::new(CloseControl::new(Some(stop_tx))),
        send_queue: send_q_tx,
    });
    let close_control = sock_impl.close_control.clone();
    c4Socket_setNativeHandle(c4sock, Box::into_raw(sock_impl) as *mut c_void);
    let c4sock = C4SocketPtr(c4sock);
    let core = FfiCore::<ConnectingPhase>::new(c4sock);
    handle.spawn(async move {
        let close_ctl = close_control.clone();
        match do_open(core, request, stop_rx, command_rx, send_q_rx, close_control).await {
            Ok(()) => {}
            Err(err) => {
                trace!("c4sock {c4sock:?}: signal closed");
                close_ctl.signal_closed(c4sock, err.0);
            }
        }
    });
}

unsafe extern "C" fn ws_write(c4sock: *mut C4Socket, allocated_data: C4SliceResult) {
    trace!(
        "c4sock {c4sock:?}: write allocated_data.size {}",
        allocated_data.size
    );
    let socket = socket_impl(c4sock).as_ref();
    let data = Bytes::from_owner(allocated_data);
    let _ = socket.send_queue.send(WriterCommand::Write(data));
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
    let writer = socket.send_queue.clone();
    let close_control = socket.close_control.clone();
    let c4sock = C4SocketPtr(c4sock);

    socket.handle.spawn(async move {
        trace!("c4sock {c4sock:?}: start closing");
        let state = close_control.state.claim(LegacyCloseState::Client);
        let is_client_close = match state {
            LegacyCloseState::None => true,
            LegacyCloseState::Server => false,
            LegacyCloseState::Client => {
                panic!("Internal logic error: duplicate requestClose calls")
            }
        };
        let err = close_error(status, &reason);
        if close_control.writer_task.lock().unwrap().is_none() {
            close_control.stop_with_error(c4sock, err).await;
            info!("c4sock {c4sock:?}: requestClose before writer initialized");
            return;
        }
        let code = u16::try_from(status)
            .ok()
            .map(CloseCode::from)
            .filter(|code| code.is_allowed());
        let Some(code) = code else {
            warn!("c4sock {c4sock:?}: invalid close status {status}, closing transport without a close frame");
            close_control.stop_with_error(c4sock, err).await;
            close_control.signal_closed(c4sock, err);
            return;
        };
        let (sent_tx, sent_rx) = oneshot::channel();
        if writer
            .send(WriterCommand::Close(
                CloseFrame {
                    code,
                    reason: reason.into(),
                },
                sent_tx,
            ))
            .is_err()
            || sent_rx.await.is_err()
        {
            close_control.stop_with_error(c4sock, err).await;
            close_control.signal_closed(c4sock, err);
            return;
        }
        if is_client_close {
            // acording to comment from c4SocketTypes.h
            const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
            if tokio::time::timeout(CLOSE_TIMEOUT, close_control.confirm.notified())
                .await
                .is_err()
            {
                warn!("c4sock {c4sock:?}: timeout for waiting close ack expired");
                close_control.signal_to_stop_read_loop(c4sock).await;
            }
        }
        close_control.signal_closed(c4sock, err);
    });
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
    core: C,
    request: Result<Request, Error>,
    stop_rx: oneshot::Receiver<()>,
    command_rx: mpsc::UnboundedReceiver<Command>,
    writer_rx: mpsc::UnboundedReceiver<WriterCommand>,
    close_control: Arc<CloseControl>,
) -> Result<(), Error>
where
    C: ConnectingSink,
    for<'a> &'a C: Into<usize>,
    for<'a> &'a C::Open: Into<usize>,
{
    let request = request?;
    do_open_with_connect(
        core,
        connect_async(request),
        stop_rx,
        command_rx,
        writer_rx,
        close_control,
    )
    .await
}

async fn do_open_with_connect<C, S, F>(
    mut core: C,
    connect: F,
    mut stop_rx: oneshot::Receiver<()>,
    command_rx: mpsc::UnboundedReceiver<Command>,
    writer_rx: mpsc::UnboundedReceiver<WriterCommand>,
    close_control: Arc<CloseControl>,
) -> Result<(), Error>
where
    C: ConnectingSink,
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    F: Future<Output = Result<(WebSocketStream<S>, Response), tungstenite::Error>>,
    for<'a> &'a C: Into<usize>,
    for<'a> &'a C::Open: Into<usize>,
{
    let context: usize = (&core).into();
    let (ws_stream, http_resp) = tokio::select! {
        v = connect => {
            trace!("c4sock {context:#x}: connect_async finished");
            match v {
                Ok(result) => result,
                Err(err) => {
                    if let tungstenite::Error::Http(ref response) = err {
                        if let Ok(headers) = unsafe { headers_to_dict(response) } {
                            core.got_http_response(response.status().as_u16() as c_int, headers);
                        }
                    }
                    return Err(unsafe { tungstenite_err_to_c4_err(err) });
                }
            }
        }
        _ = (&mut stop_rx) => {
            trace!("c4sock {context:#x}: do_open interrupted");
            return Err(Error(close_control.stop_error.lock().unwrap().unwrap_or_else(|| {
                network_error(
                    C4NetworkErrorCode::kC4NetErrNotConnected,
                    "open was interrupted by requestClose",
                )
            })));
        }
    };

    {
        let headers = unsafe { headers_to_dict(&http_resp) }?;
        core.got_http_response(http_resp.status().as_u16() as c_int, headers);
        mem::drop(http_resp);
    }
    run_open_socket(
        core,
        ws_stream,
        stop_rx,
        command_rx,
        writer_rx,
        close_control,
    )
    .await
}

async fn run_open_socket<C, S>(
    core: C,
    ws_stream: WebSocketStream<S>,
    stop_rx: oneshot::Receiver<()>,
    command_rx: mpsc::UnboundedReceiver<Command>,
    writer_rx: mpsc::UnboundedReceiver<WriterCommand>,
    close_control: Arc<CloseControl>,
) -> Result<(), Error>
where
    C: ConnectingSink,
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    for<'a> &'a C::Open: Into<usize>,
{
    let (ws_writer, ws_reader) = ws_stream.split();
    let c4sock = core.socket();
    let writer_task = tokio::spawn(writer_loop(
        ws_writer,
        writer_rx,
        close_control.clone(),
        c4sock,
    ));
    *close_control.writer_task.lock().unwrap() = Some(writer_task.abort_handle());
    let core = core.opened();
    main_read_loop(core, ws_reader, stop_rx, command_rx, close_control).await?;
    Ok(())
}

async fn writer_loop<S>(
    mut writer: WsWriter<S>,
    mut commands: mpsc::UnboundedReceiver<WriterCommand>,
    close_control: Arc<CloseControl>,
    mut c4sock: C4SocketPtr,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    while let Some(command) = commands.recv().await {
        match command {
            WriterCommand::Write(data) => {
                let size = data.len();
                if let Err(error) = writer.send(Message::Binary(data)).await {
                    close_control.signal_write_failure(c4sock, error).await;
                    return;
                }
                c4sock.completed_write(size);
            }
            WriterCommand::Close(frame, sent) => {
                trace!("c4sock {c4sock:?}: sending close message");
                if let Err(error) = writer.send(Message::Close(Some(frame))).await {
                    close_control.signal_write_failure(c4sock, error).await;
                    return;
                }
                let _ = sent.send(());
            }
        }
    }
}

async fn main_read_loop<C, S>(
    mut core: C,
    mut ws_reader: WsReader<S>,
    mut stop_rx: oneshot::Receiver<()>,
    mut command_rx: mpsc::UnboundedReceiver<Command>,
    close_control: Arc<CloseControl>,
) -> Result<(), Error>
where
    C: OpenSink,
    S: AsyncRead + AsyncWrite + Unpin,
    for<'a> &'a C: Into<usize>,
{
    let context: usize = (&core).into();
    'read_loop: loop {
        tokio::select! {
            message = ws_reader.next() => {
                let message = match message {
                    Some(x) => x,
                    None => break 'read_loop,
                };
                let message = message.map_err(|err| unsafe { tungstenite_err_to_c4_err(err) })?;
                match message {
                    m @ Message::Text(_) | m @ Message::Binary(_) => {

                        let data = m.into_data();
                        if data.is_empty() {
                            return Err(Error(unsafe {
                                c4error_make(
                                    C4ErrorDomain::LiteCoreDomain,
                                    C4ErrorCode::kC4ErrorCorruptData.0,
                                    "empty BLIP message".into(),
                                )
                            }));
                        }
                        core.received(data.as_ref());
                        let mut remaining = data.len();
                        loop {
                            let command = tokio::select! {
                                command = command_rx.recv() => command,
                                _ = &mut stop_rx => {
                                    return Err(Error(network_error(
                                        C4NetworkErrorCode::kC4NetErrNotConnected,
                                        "receive was interrupted by requestClose",
                                    )));
                                }
                            };
                            let Some(Command::CompletedReceive(byte_count)) = command else {
                                return Err(Error(network_error(
                                    C4NetworkErrorCode::kC4NetErrConnectionAborted,
                                    "receive acknowledgement channel closed",
                                )));
                            };
                            remaining = remaining.saturating_sub(byte_count);
                            if remaining == 0 {
                                break;
                            }
                        }
                    }
                    Message::Close(close_frame) => {
                        let (code, reason) = close_frame
                            .map(|frame| (u16::from(frame.code) as c_int, frame.reason))
                            .unwrap_or((C4WebSocketCloseCode::kWebSocketCloseNoCode.0, "".into()));
                        let state = close_control.state.claim(LegacyCloseState::Server);
                        info!(
                            "c4sock {context:#x}: close frame was received, state {state:?}"
                        );
                        match state {
                            LegacyCloseState::None => {
                                core.close_requested(code, &reason);
                            }
                            LegacyCloseState::Server => {
                                warn!("c4sock {context:#x} duplicate close message from server: {code} {reason}");
                                continue 'read_loop;
                            }
                            LegacyCloseState::Client => {
                                close_control.confirm.notify_one();
                            }
                        }
                        break 'read_loop;
                    }
                    Message::Frame(_) =>
                        warn!("unsupport Message::Frame format"),
                    Message::Ping(_) => {
                        trace!("c4sock {context:#x}: ping frame was received");
                        warn!("ping handling not implemented");
                    }
                    Message::Pong(_) => {
                        trace!("c4sock {context:#x}: pong frame was received");
                        warn!("pong handling not implemented");
                    }
                }

            }

            _ = (&mut stop_rx) => {
                info!("c4sock {context:#x}: read from websocket was interrupted by requestClose");
                return Err(Error(network_error(
                    C4NetworkErrorCode::kC4NetErrNotConnected,
                    "open was interrupted by requestClose",
                )));
            }

        };
    }
    trace!("c4sock {context:#x}: main read loop end");
    Ok(())
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

impl CloseControl {
    fn new(stop_read_loop: Option<oneshot::Sender<()>>) -> Self {
        Self {
            confirm: Notify::new(),
            stop_read_loop: TokioMutex::new(stop_read_loop),
            stop_error: Mutex::new(None),
            state: AtomicLegacyCloseState::new(LegacyCloseState::None),
            signaled: AtomicBool::new(false),
            writer_task: Mutex::new(None),
        }
    }

    async fn stop_with_error(&self, ctx: C4SocketPtr, error: C4Error) {
        *self.stop_error.lock().unwrap() = Some(error);
        self.signal_to_stop_read_loop(ctx).await;
    }

    async fn signal_write_failure(&self, ctx: C4SocketPtr, error: tungstenite::Error) {
        error!("c4sock {ctx:?}: writer.send failure: {error}");
        let error = unsafe { tungstenite_err_to_c4_err(error) }.0;
        self.stop_with_error(ctx, error).await;
        self.signal_closed(ctx, error);
    }

    fn signal_closed(&self, ctx: C4SocketPtr, error: C4Error) {
        if !self.signaled.swap(true, Ordering::SeqCst) {
            if let Some(writer_task) = self.writer_task.lock().unwrap().take() {
                writer_task.abort();
            }
            let error = self.stop_error.lock().unwrap().unwrap_or(error);
            unsafe { c4socket_closed(ctx.0, error) };
        }
    }

    async fn signal_to_stop_read_loop(&self, ctx: C4SocketPtr) {
        let mut lock = self.stop_read_loop.lock().await;
        if let Some(stop_tx) = lock.take() {
            trace!("c4sock {:?}: stoping connect/read loop", ctx.0);
            let _ = stop_tx.send(());
        }
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

        struct RecordingSink(Arc<Mutex<Vec<(c_int, String)>>>);

        impl From<&RecordingSink> for usize {
            fn from(_: &RecordingSink) -> Self {
                0
            }
        }

        impl ConnectingSink for RecordingSink {
            type Open = FfiCore<OpenPhase>;

            fn socket(&self) -> C4SocketPtr {
                unreachable!("a rejected handshake must not access the socket")
            }

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
        let (stop_tx, stop_rx) = oneshot::channel();
        let (_commands, command_rx) = mpsc::unbounded_channel();
        let (_send_tx, send_rx) = mpsc::unbounded_channel();
        let close_control = Arc::new(CloseControl::new(Some(stop_tx)));
        let error = do_open(
            RecordingSink(responses.clone()),
            Ok(format!("ws://{address}/").into_client_request().unwrap()),
            stop_rx,
            command_rx,
            send_rx,
            close_control,
        )
        .await
        .unwrap_err();
        server.join().unwrap();

        assert_eq!(error.0.domain, C4ErrorDomain::WebSocketDomain);
        assert_eq!(error.0.code, 403);
        assert_eq!(*responses.lock().unwrap(), [(403, "rejected".to_owned())]);
    }

    #[tokio::test]
    async fn test_request_close_before_open_preserves_status() {
        struct NeverOpened;

        impl From<&NeverOpened> for usize {
            fn from(_: &NeverOpened) -> Self {
                0
            }
        }

        impl ConnectingSink for NeverOpened {
            type Open = FfiCore<OpenPhase>;

            fn socket(&self) -> C4SocketPtr {
                unreachable!("the handshake must not finish")
            }

            fn got_http_response(&mut self, _: c_int, _: FLSliceResult) {
                panic!("the handshake must not finish");
            }

            fn opened(self) -> Self::Open {
                panic!("the handshake must not finish");
            }
        }

        let (stop_tx, stop_rx) = oneshot::channel();
        let (_commands, command_rx) = mpsc::unbounded_channel();
        let (_send_tx, send_rx) = mpsc::unbounded_channel();
        let close_control = Arc::new(CloseControl::new(Some(stop_tx)));
        let (connecting_tx, connecting_rx) = oneshot::channel();
        let connect = async move {
            let _ = connecting_tx.send(());
            std::future::pending::<
                Result<(WebSocketStream<tokio::io::DuplexStream>, Response), tungstenite::Error>,
            >()
            .await
        };
        let open_task = tokio::spawn(do_open_with_connect(
            NeverOpened,
            connect,
            stop_rx,
            command_rx,
            send_rx,
            close_control.clone(),
        ));

        tokio::time::timeout(Duration::from_secs(2), connecting_rx)
            .await
            .unwrap()
            .unwrap();
        close_control
            .stop_with_error(
                C4SocketPtr(std::ptr::null_mut()),
                close_error(504, "Timed out"),
            )
            .await;
        let error = tokio::time::timeout(Duration::from_secs(2), open_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.0.domain, C4ErrorDomain::WebSocketDomain);
        assert_eq!(error.0.code, 504);
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
        let (_stop_tx, stop_rx) = oneshot::channel();
        let (_commands, command_rx) = mpsc::unbounded_channel();
        let close_control = Arc::new(CloseControl::new(None));
        let error = tokio::time::timeout(
            Duration::from_secs(2),
            main_read_loop(NoMessages, reader, stop_rx, command_rx, close_control),
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

        struct RecordingSink(Arc<Mutex<Vec<(c_int, String)>>>);

        impl From<&RecordingSink> for usize {
            fn from(_: &RecordingSink) -> Self {
                0
            }
        }

        impl OpenSink for RecordingSink {
            fn received(&mut self, _: &[u8]) {
                panic!("no data message expected");
            }

            fn close_requested(&mut self, status: c_int, reason: &str) {
                self.0.lock().unwrap().push((status, reason.to_owned()));
            }
        }

        let (client_io, server_io) = tokio::io::duplex(4096);
        let client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let (_writer, reader) = client.split();
        server
            .send(Message::Close(Some(CloseFrame {
                code: CloseCode::Normal,
                reason: "peer shutdown".into(),
            })))
            .await
            .unwrap();

        let callbacks = Arc::new(Mutex::new(Vec::new()));
        let (_stop_tx, stop_rx) = oneshot::channel();
        let (_commands, command_rx) = mpsc::unbounded_channel();
        let close_control = Arc::new(CloseControl::new(None));
        tokio::time::timeout(
            Duration::from_secs(2),
            main_read_loop(
                RecordingSink(callbacks.clone()),
                reader,
                stop_rx,
                command_rx,
                close_control,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(*callbacks.lock().unwrap(), [(1000, "peer shutdown".into())]);
    }

    #[tokio::test]
    async fn test_close_interrupts_receive_wait_without_a_close_frame() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

            fn close_requested(&mut self, _: c_int, _: &str) {
                panic!("the peer did not send a close frame");
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
            tcp.write_all(&[0x82, 4, b'd', b'a', b't', b'a'])
                .await
                .unwrap();
            // The client must close TCP without sending a WebSocket Close frame.
            tcp.read(&mut buffer).await.unwrap()
        });

        let (socket, _) = connect_async(format!("ws://{address}/")).await.unwrap();
        let (writer, reader) = socket.split();
        let (stop_tx, stop_rx) = oneshot::channel();
        let (_commands, command_rx) = mpsc::unbounded_channel();
        let close_control = Arc::new(CloseControl::new(Some(stop_tx)));
        let received = Arc::new(Notify::new());
        let read_task = tokio::spawn(main_read_loop(
            RecordingSink(received.clone()),
            reader,
            stop_rx,
            command_rx,
            close_control.clone(),
        ));

        tokio::time::timeout(Duration::from_secs(2), received.notified())
            .await
            .unwrap();
        drop(writer);
        close_control
            .signal_to_stop_read_loop(C4SocketPtr(std::ptr::null_mut()))
            .await;
        let error = tokio::time::timeout(Duration::from_secs(2), read_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.0.domain, C4ErrorDomain::NetworkDomain);
        assert_eq!(error.0.code, C4NetworkErrorCode::kC4NetErrNotConnected.0);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), server)
                .await
                .unwrap()
                .unwrap(),
            0
        );
    }
}
