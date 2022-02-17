use crate::{
    ffi::{
        c4Socket_getNativeHandle, c4Socket_setNativeHandle, c4error_make, c4socket_closeRequested,
        c4socket_closed, c4socket_completedWrite, c4socket_gotHTTPResponse, c4socket_opened,
        c4socket_received, c4socket_registerFactory, kC4ReplicatorOptionCookies,
        kC4ReplicatorOptionExtraHeaders, kC4SocketOptionWSProtocols, C4Address, C4Error,
        C4ErrorDomain, C4NetworkErrorCode, C4Slice, C4SliceResult, C4Socket, C4SocketFactory,
        C4SocketFraming, C4String, C4WebSocketCloseCode, FLDict_Get, FLEncoder_BeginDict,
        FLEncoder_EndDict, FLEncoder_Finish, FLEncoder_Free, FLEncoder_New, FLEncoder_WriteKey,
        FLEncoder_WriteString, FLError, FLSliceResult, FLTrust, FLValue_AsDict, FLValue_FromData,
    },
    replicator::slice_without_null_char,
    value::ValueRef,
};
use futures_util::{
    sink::SinkExt,
    stream::{SplitStream, StreamExt},
};
use log::{error, info, trace, warn};
use serde_fleece::NonNullConst;
use std::{
    borrow::Cow,
    mem,
    os::raw::{c_int, c_void},
    sync::{
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    runtime::Handle,
    sync::{oneshot, Mutex as TokioMutex, Notify},
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        self,
        handshake::client::{Request, Response},
        http::{self, HeaderValue, Uri},
        protocol::{frame::coding::CloseCode, CloseFrame},
        Message,
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
    read_push_pull: Arc<ReadPushPull>,
    writer: Arc<TokioMutex<Option<WsWriter>>>,
    close_control: Arc<CloseControl>,
}

struct ReadPushPull {
    nbytes_avaible: AtomicUsize,
    confirm: Notify,
}

struct CloseControl {
    confirm: Notify,
    stop_read_loop: TokioMutex<Option<oneshot::Sender<()>>>,
    state: AtomicCloseState,
    signaled: AtomicBool,
}

#[repr(u8)]
#[derive(Debug)]
enum CloseState {
    None = 0,
    Server = 1,
    Client = 2,
}

#[repr(transparent)]
struct AtomicCloseState(AtomicU8);
impl AtomicCloseState {
    fn new(val: CloseState) -> Self {
        Self(AtomicU8::new(val as u8))
    }
    fn store(&self, val: CloseState, ordering: Ordering) {
        self.0.store(val as u8, ordering);
    }
    fn load(&self, ordering: Ordering) -> CloseState {
        let val = self.0.load(ordering);
        match val {
            0 => CloseState::None,
            1 => CloseState::Server,
            2 => CloseState::Client,
            _ => unreachable!(),
        }
    }
}

type WsReader =
    SplitStream<WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>;
type WsWriter = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Message,
>;

#[repr(transparent)]
#[derive(Clone, Copy, Debug)]
struct C4SocketPtr(*mut C4Socket);
unsafe impl Send for C4SocketPtr {}

#[repr(transparent)]
#[derive(Debug)]
struct Error(C4Error);

unsafe extern "C" fn ws_open(
    c4sock: *mut C4Socket,
    addr: *const C4Address,
    options: C4Slice,
    context: *mut c_void,
) {
    assert!(!context.is_null());
    let handle: &Handle = &*(context as *mut Handle);
    assert!(!c4sock.is_null());
    let c4sock: &mut C4Socket = &mut *c4sock;
    assert!(c4Socket_getNativeHandle(c4sock).is_null());
    assert!(!addr.is_null());
    let addr: &C4Address = &*addr;

    let request = c4address_to_request(c4sock as *mut C4Socket as usize, addr, options);
    info!(
        "c4sock {:?}: open was called with uri: {:?}",
        c4sock as *const C4Socket,
        request.as_ref().map(Request::uri)
    );
    let (stop_tx, stop_rx) = oneshot::channel();
    let sock_impl = Box::new(SocketImpl {
        handle: handle.clone(),
        read_push_pull: Arc::new(ReadPushPull {
            nbytes_avaible: AtomicUsize::new(0),
            confirm: Notify::new(),
        }),
        writer: Arc::new(TokioMutex::new(None)),
        close_control: Arc::new(CloseControl {
            state: AtomicCloseState::new(CloseState::None),
            confirm: Notify::new(),
            stop_read_loop: TokioMutex::new(Some(stop_tx)),
            signaled: AtomicBool::new(false),
        }),
    });
    let read_push_pull = sock_impl.read_push_pull.clone();
    let writer = sock_impl.writer.clone();
    let close_control = sock_impl.close_control.clone();
    c4Socket_setNativeHandle(c4sock, Box::into_raw(sock_impl) as *mut c_void);
    let c4sock = C4SocketPtr(c4sock);
    handle.spawn(async move {
        let close_ctl = close_control.clone();
        match do_open(
            c4sock,
            request,
            stop_rx,
            read_push_pull,
            writer,
            close_control,
        )
        .await
        {
            Ok(()) => {}
            Err(err) => {
                trace!("c4sock {:?}: call c4socket_closed", c4sock);
                if !close_ctl.signaled.swap(true, Ordering::SeqCst) {
                    c4socket_closed(c4sock.0, err.0);
                }
            }
        }
    });
}

unsafe extern "C" fn ws_write(c4sock: *mut C4Socket, allocated_data: C4SliceResult) {
    trace!(
        "c4sock {:?}: write allocated_data.size {}",
        c4sock,
        allocated_data.size
    );
    assert!(!c4sock.is_null());
    let native = c4Socket_getNativeHandle(c4sock) as *mut SocketImpl;
    assert!(!native.is_null());
    let socket: &SocketImpl = &*native;
    let writer = socket.writer.clone();
    //TODO: change this when `Vec` allocator API was stabilized
    // https://github.com/rust-lang/rust/issues/32838
    let data: Vec<u8> = allocated_data.as_bytes().to_vec();
    let c4sock = C4SocketPtr(c4sock);
    socket.handle.spawn(async move {
        let mut writer = writer.lock().await;
        if let Some(writer) = writer.as_mut() {
            let n = data.len();
            if let Err(err) = writer.send(Message::Binary(data)).await {
                error!("c4sock {:?}: writer.send failure: {}", c4sock, err);
            } else {
                c4socket_completedWrite(c4sock.0, n);
            }
        }
    });
}

unsafe extern "C" fn ws_completed_receive(c4sock: *mut C4Socket, byte_count: usize) {
    trace!(
        "c4sock {:?}: completedReceive, byte_count {}",
        c4sock,
        byte_count
    );
    assert!(!c4sock.is_null());
    let native = c4Socket_getNativeHandle(c4sock) as *mut SocketImpl;
    assert!(!native.is_null());
    let socket: &SocketImpl = &*native;
    let nbytes = socket.read_push_pull.nbytes_avaible.load(Ordering::Acquire);
    let nbytes = if nbytes >= byte_count {
        nbytes - byte_count
    } else {
        0
    };
    socket
        .read_push_pull
        .nbytes_avaible
        .store(nbytes, Ordering::Release);
    if nbytes == 0 {
        socket.read_push_pull.confirm.notify_one();
    }
}

unsafe extern "C" fn ws_request_close(c4sock: *mut C4Socket, status: c_int, message: C4String) {
    trace!(
        "c4sock {:?}: requestClose status {}, message: {:?}",
        c4sock,
        status,
        std::str::from_utf8(message.into())
    );
    let code: CloseCode = u16::try_from(status).unwrap_or(1).into();
    let reason: Cow<'static, str> = String::from_utf8_lossy(message.into());

    assert!(!c4sock.is_null());
    let native = c4Socket_getNativeHandle(c4sock) as *mut SocketImpl;
    assert!(!native.is_null());
    let socket: &SocketImpl = &*native;
    let writer = socket.writer.clone();
    let close_control = socket.close_control.clone();
    socket.handle.block_on(async move {
        let state = close_control.state.load(Ordering::Acquire);
        let is_client_close = match state {
            CloseState::None => {
                close_control
                    .state
                    .store(CloseState::Client, Ordering::Release);
                true
            }
            CloseState::Server => false,
            CloseState::Client => panic!("Internal logic error: duplicate requestClose calls"),
        };
        let err = c4error_make(
            C4ErrorDomain::WebSocketDomain,
            status,
            reason.as_bytes().into(),
        );
        let mut writer = writer.lock().await;
        if let Some(writer) = writer.as_mut() {
            trace!("c4sock {:?}: sending close message", c4sock);
            if let Err(err) = writer
                .send(Message::Close(Some(CloseFrame { code, reason })))
                .await
            {
                error!(
                    "c4sock {:?}: requestClose, writer.send failure: {}",
                    c4sock, err
                );
            }
        } else {
            close_control.signal_to_stop_read_loop(c4sock).await;
            info!(
                "c4sock {:?}: requestClose writer is None => not intialized",
                c4sock
            );
            return;
        }
        if is_client_close {
            // acording to comment from c4SocketTypes.h
            const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
            if let Err(_) =
                tokio::time::timeout(CLOSE_TIMEOUT, close_control.confirm.notified()).await
            {
                warn!("c4sock {:?}: timeout for waiting close ack expired", c4sock);
                close_control.signal_to_stop_read_loop(c4sock).await;
            }
        }
        if !close_control.signaled.swap(true, Ordering::SeqCst) {
            c4socket_closed(c4sock, err);
        }
    });
}

unsafe extern "C" fn ws_dispose(c4sock: *mut C4Socket) {
    trace!("c4sock {:?}: dispose", c4sock);
    assert!(!c4sock.is_null());
    let native = c4Socket_getNativeHandle(c4sock) as *mut SocketImpl;
    assert!(!native.is_null());
    let sock_impl = Box::from_raw(native);
    mem::drop(sock_impl);
}

unsafe fn c4address_to_request(
    marker: usize,
    addr: &C4Address,
    options: C4Slice,
) -> Result<Request, Error> {
    let uri = Uri::builder()
        .scheme(<&[u8]>::from(addr.scheme))
        .authority(<&[u8]>::from(addr.hostname))
        .port(addr.port)
        .path_and_query(<&[u8]>::from(addr.path))
        .build()?;
    trace!("c4address_to_request, marker {:x}, uri {:?}", marker, uri);
    let mut request = Request::get(uri).body(())?;
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

    if let ValueRef::Dict(_opts) = ValueRef::from(FLDict_Get(
        options.as_ptr(),
        slice_without_null_char(kC4ReplicatorOptionExtraHeaders).into(),
    )) {
        unimplemented!()
    }

    if let ValueRef::String(cookies) = ValueRef::from(FLDict_Get(
        options.as_ptr(),
        slice_without_null_char(kC4ReplicatorOptionCookies).into(),
    )) {
        request
            .headers_mut()
            .insert("Cookie", HeaderValue::from_str(cookies)?);
    }

    if let ValueRef::String(protocol) = ValueRef::from(FLDict_Get(
        options.as_ptr(),
        slice_without_null_char(kC4SocketOptionWSProtocols).into(),
    )) {
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", HeaderValue::from_str(protocol)?);
    }

    Ok(request)
}

async fn do_open(
    c4sock: C4SocketPtr,
    request: Result<Request, Error>,
    mut stop_rx: oneshot::Receiver<()>,
    read_push_pull: Arc<ReadPushPull>,
    writer: Arc<TokioMutex<Option<WsWriter>>>,
    close_control: Arc<CloseControl>,
) -> Result<(), Error> {
    let request = request?;
    let (ws_stream, http_resp) = tokio::select! {
        v = connect_async(request) => {
            trace!("c4sock {:?}: connect_async finished", c4sock);
            v.map_err(|err| unsafe { tungstenite_err_to_c4_err(err) })?
        }
        _ = (&mut stop_rx) => {
            trace!("c4sock {:?}: do_open interrupted", c4sock);
            return Err(Error(unsafe {
                c4error_make(
                    C4ErrorDomain::NetworkDomain,
                    C4NetworkErrorCode::kC4NetErrNotConnected.0,
                    "open was interrupted by requestClose".into(),
                )
            }));
        }
    };

    {
        let headers = unsafe { headers_to_dict(&http_resp) }?;
        unsafe {
            c4socket_gotHTTPResponse(
                c4sock.0,
                http_resp.status().as_u16() as c_int,
                headers.as_fl_slice(),
            )
        };
        mem::drop(http_resp);
    }
    let (ws_writer, ws_reader) = ws_stream.split();
    {
        let mut lock = writer.lock().await;
        *lock = Some(ws_writer);
    }
    unsafe {
        c4socket_opened(c4sock.0);
    }
    main_read_loop(c4sock, ws_reader, stop_rx, read_push_pull, close_control).await?;
    Ok(())
}

async fn main_read_loop(
    c4sock: C4SocketPtr,
    mut ws_reader: WsReader,
    mut stop_rx: oneshot::Receiver<()>,
    read_push_pull: Arc<ReadPushPull>,
    close_control: Arc<CloseControl>,
) -> Result<(), Error> {
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
                        read_push_pull.nbytes_avaible.store(data.len(), Ordering::Release);
                        unsafe {
                            c4socket_received(c4sock.0, data.as_slice().into());
                        }
                        read_push_pull.confirm.notified().await;
                    }
                    Message::Close(close_frame) => {
                        info!(
                            "c4sock {:?}: close frame was received, state {:?}",
                            c4sock,
                            close_control.state.load(Ordering::Acquire)
                        );
                        let (code, reason) = close_frame.map(|x| (u16::from(&x.code) as c_int, x.reason)).unwrap_or_else(|| {
                            (-1, "".into())
                        });
                        let state = close_control
                            .state
                            .load(Ordering::Acquire);
                        match state {
                            CloseState::None => {
                                close_control.state.store(CloseState::Server, Ordering::Release);
                                unsafe {
                                    c4socket_closeRequested(c4sock.0, code, reason.as_bytes().into());
                                }
                            }
                            CloseState::Server => {
                                warn!("c4sock {:?} duplicate close message from server: {} {}",
                                      c4sock, code, reason);
                                continue 'read_loop;
                            }
                            CloseState::Client => {
                                close_control.confirm.notify_one();
                            }
                        }
                        break 'read_loop;
                    }
                    Message::Ping(_) => {
                        trace!("c4sock {:?}: ping frame was received", c4sock);
                        todo!();
                    }
                    Message::Pong(_) => {
                        trace!("c4sock {:?}: pong frame was received", c4sock);
                        todo!();
                    }
                }

            }

            _ = (&mut stop_rx) => {
                info!("c4sock {:?}: read from websocket was interrupted by requestClose", c4sock);
                return Err(Error(unsafe {
                    c4error_make(
                        C4ErrorDomain::NetworkDomain,
                        C4NetworkErrorCode::kC4NetErrNotConnected.0,
                        "open was interrupted by requestClose".into(),
                    )
                }));
            }

        };
    }
    trace!("c4sock {:?}: main read loop end", c4sock);
    Ok(())
}

unsafe fn tungstenite_err_to_c4_err(err: tungstenite::Error) -> Error {
    use tungstenite::error::Error::*;
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
        Io(_) => (
            C4ErrorDomain::NetworkDomain,
            C4NetworkErrorCode::kC4NumNetErrorCodesPlus1.0,
        ),
        #[cfg(feature = "tls")]
        Tls(_) => (
            C4ErrorDomain::NetworkDomain,
            C4NetworkErrorCode::kC4NumNetErrorCodesPlus1.0,
        ),
        Capacity(_) => (
            C4ErrorDomain::NetworkDomain,
            C4NetworkErrorCode::kC4NumNetErrorCodesPlus1.0,
        ),
        Protocol(_) => (
            C4ErrorDomain::NetworkDomain,
            C4NetworkErrorCode::kC4NumNetErrorCodesPlus1.0,
        ),
        SendQueueFull(_) => (
            C4ErrorDomain::NetworkDomain,
            C4NetworkErrorCode::kC4NumNetErrorCodesPlus1.0,
        ),
        Utf8 => (
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

unsafe fn headers_to_dict(http_resp: &Response) -> Result<FLSliceResult, FLError> {
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
        Ok(res.into())
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
    async fn signal_to_stop_read_loop(&self, ctx: *mut C4Socket) {
        let mut lock = self.stop_read_loop.lock().await;
        if let Some(stop_tx) = lock.take() {
            trace!("c4sock {:?}: stoping connect/read loop", ctx);
            let _ = stop_tx.send(());
        }
    }
}
